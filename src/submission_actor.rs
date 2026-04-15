use anyhow::Context;
use bytes::Bytes;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::fs;
use tokio::io;

use crate::database::{DbClient, SubmissionStatus};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;
use tracing::Instrument;
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
    db_service: Arc<DbClient>,
    mut tasks: Receiver<SubmissionTask>,
) {
    while let Some(task) = tasks.recv().await {
        let ulid = task.ulid;
        tracing::debug!(ulid=%ulid, "Received task");
        tokio::spawn(
            submission_task(config.clone(), db_service.clone(), task)
                .instrument(tracing::info_span!("submission_task", ulid=%ulid)),
        );
    }
}

async fn future_with_timeout<T>(
    duration: Duration,
    f: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
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
    out_path: &Path,
) -> anyhow::Result<()> {
    let submission_dir = submission_dir(config, ulid);
    future_with_timeout(
        Duration::from_secs(5),
        compile_s_to_elf(config, &source_code, &submission_dir),
    )
    .await
    .context("compilation")?;

    future_with_timeout(
        Duration::from_secs(10),
        run_simulator(config, &submission_dir, ticks, out_path),
    )
    .await
}

async fn submission_task(config: Arc<Config>, db_service: Arc<DbClient>, task: SubmissionTask) {
    let sub_dir = submission_dir(&config, task.ulid);
    if let Err(err) = fs::create_dir_all(&sub_dir).await {
        tracing::error!("Can't create submission_dir: {err:#}");
        return;
    }

    db_service
        .update_submission_status(task.ulid, SubmissionStatus::InProgress)
        .await;

    let trace_file = submission_file(config.as_ref(), task.ulid);
    let trace_file = trace_file.as_path();
    let sim_res = simulate(
        &config,
        task.ulid,
        task.source_code.clone(),
        task.ticks,
        trace_file,
    )
    .await;

    let source_json =
        serde_json::json!({ "code": String::from_utf8_lossy(&task.source_code) }).to_string();

    let final_status = match sim_res {
        Ok(()) => SubmissionStatus::Completed,
        Err(e) => {
            tracing::error!("simulation failed: {e:#}");
            if let Err(err) = fs::write(
                trace_file,
                serde_json::json!({ "error": format!("{e:?}") }).to_string(),
            )
            .await
            {
                tracing::error!("Failed to write trace: {err:#}");
            }
            SubmissionStatus::Completed
        }
    };

    if let Err(err) = fs::write(source_file(config.as_ref(), task.ulid), source_json).await {
        tracing::error!("Failed to write source: {err:#}");
    }

    db_service
        .update_submission_status(task.ulid, final_status)
        .await;

    tracing::info!(status=?final_status, "Complete");
}

pub fn submission_dir(config: &Config, ulid: Ulid) -> PathBuf {
    let mut buf = [0u8; ULID_LEN];
    let ulid_str = ulid.array_to_str(&mut buf);
    config.submissions_folder.join(&ulid_str)
}

pub fn submission_file(config: &Config, ulid: Ulid) -> PathBuf {
    submission_dir(config, ulid).join("trace.json")
}

pub fn source_file(config: &Config, ulid: Ulid) -> PathBuf {
    submission_dir(config, ulid).join("source.json")
}

async fn compile_s_to_elf(
    config: &Config,
    s_content: &[u8],
    submission_dir: impl AsRef<Path>,
) -> anyhow::Result<()> {
    tracing::info!("Compiling...");

    let dir = submission_dir.as_ref();
    let s_path = dir.join("input.s");
    let o_path = dir.join("output.o");
    let elf_path = dir.join("output.elf");

    let mut file = fs::File::create_new(&s_path)
        .await
        .context("writing source code")?;
    file.write_all(s_content).await?;

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
        anyhow::bail!("Assembler error:\n{}\n{}", stderr, stdout);
    }

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
        anyhow::bail!("Linker error:\n{}\n{}", stderr, stdout);
    }

    Ok(())
}

async fn run_simulator(
    config: &Config,
    submission_dir: &Path,
    ticks: u32,
    out_path: &Path,
) -> anyhow::Result<()> {
    tracing::info!("Simulating...");

    let elf_path = submission_dir.join("output.elf");
    let mut child = Command::new(&config.simulator_binary)
        .arg("--ticks")
        .arg(ticks.to_string())
        .arg("--path")
        .arg(&elf_path)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("simulating")?;

    let mut stdout = child.stdout.take().context("no stdout")?;
    let mut file = fs::File::create(out_path).await?;

    io::copy(&mut stdout, &mut file).await?;

    child.wait().await?;
    Ok(())
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
