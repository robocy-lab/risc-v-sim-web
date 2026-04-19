use anyhow::Context;
use bytes::Bytes;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs;
use tracing::instrument;

use crate::database::{DbClient, SubmissionStatus};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;
use ulid::{ULID_LEN, Ulid};

pub fn submission_file(submissions_folder: &Path, ulid: Ulid) -> PathBuf {
    submission_dir(submissions_folder, ulid).join("trace.json")
}

pub fn source_file(submissions_folder: &Path, ulid: Ulid) -> PathBuf {
    submission_dir(submissions_folder, ulid).join("source.json")
}

pub fn submission_dir(submissions_folder: &Path, ulid: Ulid) -> PathBuf {
    let mut buf = [0u8; ULID_LEN];
    let ulid_str = ulid.array_to_str(&mut buf);
    submissions_folder.join(&ulid_str)
}

#[derive(Debug)]
pub struct SubmissionTask {
    pub source_code: Bytes,
    pub ticks: u32,
    pub ulid: Ulid,
    pub user_id: i64,
}

pub struct SubmissionActor {
    tasks: Receiver<SubmissionTask>,
    internal: Arc<InternalShare>,
}

struct InternalShare {
    db_client: Arc<DbClient>,
    as_binary: PathBuf,
    ld_binary: PathBuf,
    simulator_binary: PathBuf,
    submissions_folder: PathBuf,
}

impl SubmissionActor {
    pub fn new(
        tasks: Receiver<SubmissionTask>,
        db_client: Arc<DbClient>,
        as_binary: PathBuf,
        ld_binary: PathBuf,
        simulator_binary: PathBuf,
        submissions_folder: PathBuf,
    ) -> Self {
        SubmissionActor {
            tasks,
            internal: Arc::new(InternalShare {
                db_client,
                as_binary,
                ld_binary,
                simulator_binary,
                submissions_folder,
            }),
        }
    }

    #[instrument(name = "submission_actor", skip(self))]
    pub async fn run(mut self) {
        while let Some(task) = self.tasks.recv().await {
            let ulid = task.ulid;
            tracing::debug!(ulid=%ulid, "Received task");
            let run = SubmissionTaskRun {
                task,
                internal: self.internal.clone(),
            };
            tokio::spawn(run.run());
        }
    }
}

struct SubmissionTaskRun {
    task: SubmissionTask,
    internal: Arc<InternalShare>,
}

impl SubmissionTaskRun {
    #[instrument(name = "submission_task", skip(self), fields(ulid=%self.task.ulid))]
    async fn run(self) {
        let SubmissionTaskRun { task, internal } = self;

        let sub_dir = submission_dir(&internal.submissions_folder, task.ulid);
        if let Err(err) = fs::create_dir_all(&sub_dir).await {
            tracing::error!("Can't create submission_dir: {err:#}");
            return;
        }

        internal
            .db_client
            .update_submission_status(task.ulid, SubmissionStatus::InProgress)
            .await;

        let trace_file = submission_file(&internal.submissions_folder, task.ulid);
        let trace_file = trace_file.as_path();
        let sim_res = simulate(
            &internal,
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
                SubmissionStatus::Completed
            }
        };

        if let Err(err) = fs::write(
            source_file(&internal.submissions_folder, task.ulid),
            source_json,
        )
        .await
        {
            tracing::error!("Failed to write source: {err:#}");
        }

        internal
            .db_client
            .update_submission_status(task.ulid, final_status)
            .await;

        tracing::info!(status=?final_status, "Complete");
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
    config: &InternalShare,
    ulid: Ulid,
    source_code: bytes::Bytes,
    ticks: u32,
    out_path: &Path,
) -> anyhow::Result<()> {
    let submission_dir = submission_dir(&config.submissions_folder, ulid);
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

async fn compile_s_to_elf(
    config: &InternalShare,
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
    config: &InternalShare,
    submission_dir: &Path,
    ticks: u32,
    out_path: &Path,
) -> anyhow::Result<()> {
    tracing::info!("Simulating...");

    let file = fs::File::create(out_path).await?;

    let elf_path = submission_dir.join("output.elf");
    let mut child = Command::new(&config.simulator_binary)
        .arg("--ticks")
        .arg(ticks.to_string())
        .arg("--path")
        .arg(&elf_path)
        .stdout(file.into_std().await)
        .kill_on_drop(true)
        .spawn()
        .context("simulating")?;

    child.wait().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_utils() {
        let submissions_folder = Path::new("submissions");
        for _ in 0..10 {
            let ulid = Ulid::new();
            let dir = submission_dir(submissions_folder, ulid);
            let file = submission_file(submissions_folder, ulid);
            assert!(file.starts_with(dir));
        }
    }
}
