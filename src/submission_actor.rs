use anyhow::{Context, Result, bail};
use bytes::Bytes;
use serde_json::json;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs;

use crate::database::{DatabaseService, SubmissionStatus};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;
use tracing::{Instrument, debug, error, info, info_span};
use ulid::{ULID_LEN, Ulid};

#[derive(Debug)]
pub struct SubmissionTask {
    pub source_code: Bytes,
    pub ticks: u32,
    pub ulid: Ulid,
    pub user_id: i64,
}

#[derive(Clone)]
pub struct Config {
    pub as_binary: PathBuf,
    pub ld_binary: PathBuf,
    pub simulator_binary: PathBuf,
    pub submissions_folder: PathBuf,
    pub ticks_max: u32,
    pub codesize_max: u32,
}

pub async fn run_submission_actor(
    config: Arc<Config>,
    db_service: Arc<DatabaseService>,
    mut tasks: Receiver<SubmissionTask>,
) {
    while let Some(task) = tasks.recv().await {
        let ulid = task.ulid;
        debug!("Received task {ulid}");
        tokio::spawn(
            submission_task(config.clone(), db_service.clone(), task)
                .instrument(info_span!("submission_task", ulid=%ulid)),
        );
    }
}

async fn future_with_timeout<T>(
    duration: Duration,
    f: impl Future<Output = Result<T>>,
) -> Result<T> {
    timeout(duration, f)
        .await
        .map_err(anyhow::Error::from)
        .flatten()
}

async fn simulate(
    config: &Config,
    ulid: Ulid,
    source_code: bytes::Bytes,
    ticks: u32,
) -> Result<serde_json::Value> {
    let submission_dir = submission_dir(config, ulid);
    future_with_timeout(
        Duration::from_secs(5),
        compile_s_to_elf(config, &source_code, &submission_dir),
    )
    .await
    .context("compilation")?;

    let stdout = future_with_timeout(
        Duration::from_secs(10),
        run_simulator(config, &submission_dir, ticks),
    )
    .await?;

    let mut json = serde_json::from_str(&stdout).context("parse simulation output")?;
    if let serde_json::Value::Object(map) = &mut json {
        map.insert("ulid".to_string(), json!(ulid));
        map.insert("ticks".to_string(), json!(ticks));
        map.insert(
            "code".to_string(),
            json!(String::from_utf8_lossy(&source_code)),
        );
    }
    Ok(json)
}

async fn submission_task(
    config: Arc<Config>,
    db_service: Arc<DatabaseService>,
    task: SubmissionTask,
) {
    let ulid_str = task.ulid.to_string();
    info!("Processing submission {}", ulid_str);

    if let Err(e) = db_service
        .create_submission_with_user(ulid_str.clone(), task.user_id)
        .await
    {
        error!("Failed to create submission record in database: {e:#}");
        return;
    }

    let sub_dir = submission_dir(&config, task.ulid);
    if let Err(e) = fs::create_dir_all(&sub_dir).await {
        error!("can't create submission_dir: {e:#}");
        return;
    }

    if let Err(e) = db_service
        .update_submission_status(&ulid_str, SubmissionStatus::InProgress)
        .await
    {
        error!("Failed to update submission status to InProgress: {e:#}");
    }

    let sim_res = simulate(&config, task.ulid, task.source_code.clone(), task.ticks).await;
    let file_path = submission_file(config.as_ref(), task.ulid);

    let (final_status, to_write) = match sim_res {
        Ok(mut json) => {
            if let serde_json::Value::Object(map) = &mut json {
                if !map.contains_key("ulid") {
                    map.insert("ulid".to_string(), json!(task.ulid));
                }
            }
            (SubmissionStatus::Completed, json)
        }
        Err(e) => {
            error!("simulation failed: {e:#}");
            (
                SubmissionStatus::Completed,
                serde_json::json!({
                    "error": format!("{e:?}"),
                    "ulid": task.ulid,
                    "ticks": task.ticks,
                    "code": String::from_utf8_lossy(&task.source_code)
                }),
            )
        }
    };

    if let Err(write_err) = fs::write(&file_path, to_write.to_string()).await {
        error!("failed to write submission task result: {write_err:#}");
    }

    if let Err(e) = db_service
        .update_submission_status(&ulid_str, final_status)
        .await
    {
        error!("Failed to update final submission status: {e:#}");
    }

    info!(
        "Completed submission {} with status {:?}",
        ulid_str, final_status
    );
}

pub fn submission_dir(config: &Config, ulid: Ulid) -> PathBuf {
    let mut buf = [0u8; ULID_LEN];
    let ulid_str = ulid.array_to_str(&mut buf);
    config.submissions_folder.join(&ulid_str)
}

pub fn submission_file(config: &Config, ulid: Ulid) -> PathBuf {
    let mut buf = [0u8; ULID_LEN];
    let ulid_str = ulid.array_to_str(&mut buf);
    let mut path = config.submissions_folder.clone();
    path.extend([&ulid_str, "simulation.json"]);

    path
}

async fn compile_s_to_elf(
    config: &Config,
    s_content: &[u8],
    submission_dir: impl AsRef<Path>,
) -> Result<()> {
    let dir = submission_dir.as_ref();
    let s_path = dir.join("input.s");
    let o_path = dir.join("output.o");
    let elf_path = dir.join("output.elf");

    info!("Writing program to {s_path:?}");
    let mut file = fs::File::create_new(&s_path)
        .await
        .context("writing source code")?;
    file.write_all(s_content).await?;

    info!("Compiling {s_path:?} to object file {o_path:?}");
    let as_output = Command::new(&config.as_binary)
        .arg(&s_path)
        .arg("-o")
        .arg(&o_path)
        .kill_on_drop(true)
        .output()
        .await
        .context("assembling")?;
    if !as_output.status.success() {
        let stderr = String::from_utf8_lossy(&as_output.stderr);
        let stdout = String::from_utf8_lossy(&as_output.stdout);
        bail!("Assembler error:\n{}\n{}", stderr, stdout);
    }

    info!("Linking {o_path:?} to elf {elf_path:?}");
    let ld_output = Command::new(&config.ld_binary)
        .arg(&o_path)
        .arg("-Ttext=0x80000000")
        .arg("-o")
        .arg(&elf_path)
        .kill_on_drop(true)
        .output()
        .await
        .context("linking")?;
    if !ld_output.status.success() {
        let stderr = String::from_utf8_lossy(&ld_output.stderr);
        let stdout = String::from_utf8_lossy(&ld_output.stdout);
        bail!("Linker error:\n{}\n{}", stderr, stdout);
    }

    info!("Elf ready");
    Ok(())
}

async fn run_simulator(config: &Config, submission_dir: &Path, ticks: u32) -> Result<String> {
    let elf_path = submission_dir.join("output.elf");
    info!("Simulating the program at {elf_path:?}");

    let output = Command::new(&config.simulator_binary)
        .arg("--ticks")
        .arg(ticks.to_string())
        .arg("--path")
        .arg(&elf_path)
        .kill_on_drop(true)
        .output()
        .await
        .context("simulating")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!("Simulation error: {stderr}");
    }

    info!("Simulating has been successful");
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_utils() {
        let config = Config {
            as_binary: "dummy".into(),
            ld_binary: "dummy".into(),
            simulator_binary: "dummy".into(),
            submissions_folder: "submissions".into(),
            ticks_max: u32::MAX,
            codesize_max: u32::MAX,
        };
        for _ in 0..10 {
            let ulid = Ulid::new();
            let dir = submission_dir(&config, ulid);
            let file = submission_file(&config, ulid);
            assert!(file.starts_with(dir));
        }
    }
}
