mod common;
use common::*;

use tokio::{fs, task::JoinSet, time::Instant};
use ulid::Ulid;

use reqwest::{Client, Response};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tracing::{Instrument, info, info_span};

const WAIT_TIMEOUT: f32 = 5.0;
const CONCURRENCY: usize = 5;

#[derive(serde::Deserialize)]
struct SubmitResponse {
    pub ulid: Ulid,
}

#[derive(serde::Deserialize)]
struct SubmissionResponse {
    pub ulid: Ulid,
    pub ticks: u32,
    pub code: String,
    pub steps: serde_json::Value,
}

#[tokio::test]
async fn submit_simple() {
    run_test(
        "submit_simple",
        |_| {},
        async |port| {
            let client = reqwest::Client::new();
            let submit_response =
                submit_program(&client, port, 5, "riscv-samples/src/basic.s").await;
            let submit_status = submit_response.status();
            let resp_text = match submit_response.text().await {
                Ok(x) => format!("Response as text: {x}"),
                Err(e) => format!("Response has no text: {e}"),
            };
            assert_eq!(submit_status, reqwest::StatusCode::ACCEPTED, "{resp_text}");
        },
    )
    .await;
}

#[tokio::test]
async fn submit_and_wait() {
    run_test(
        "submit_and_wait",
        |_| {},
        async |port| {
            make_submission_and_wait_for_success(port, "basic.s").await;
        },
    )
    .await;
}

#[tokio::test]
async fn submit_non_existent() {
    run_test(
        "submit_non_existent",
        |_| {},
        async |port| {
            let client = reqwest::Client::new();
            let fake_submission_id = Ulid::new();
            let response = get_submission(&client, port, fake_submission_id).await;
            assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        },
    )
    .await;
}

#[tokio::test]
async fn submit_concurrent() {
    run_test(
        "submit_concurrent",
        |_| {},
        async |port| {
            let set = (0..CONCURRENCY)
                .map(|id| {
                    tokio::spawn(
                        make_submission_and_wait_for_success(port, "basic.s")
                            .instrument(info_span!("concurrent_client", id = id)),
                    )
                })
                .collect::<JoinSet<_>>();
            set.join_all().await;
        },
    )
    .await;
}

#[tokio::test]
async fn codesize_max_restriction() {
    run_test(
        "codesize_max_restriction",
        |_| {},
        async |port| {
            let client = reqwest::Client::new();
            let submit_response = submit_program(&client, port, 5, "riscv-samples/src/big.s").await;
            let submit_status = submit_response.status();
            let resp_text = match submit_response.text().await {
                Ok(x) => format!("Response as text: {x}"),
                Err(e) => format!("Response has no text: {e}"),
            };
            assert_eq!(
                submit_status,
                reqwest::StatusCode::BAD_REQUEST,
                "{resp_text}"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn ticks_max_restriction() {
    run_test(
        "ticks_max_restriction",
        |_| {},
        async |port| {
            let client = reqwest::Client::new();
            let submit_response =
                submit_program(&client, port, 100, "riscv-samples/src/basic.s").await;
            let submit_status = submit_response.status();
            let resp_text = match submit_response.text().await {
                Ok(x) => format!("Response as text: {x}"),
                Err(e) => format!("Response has no text: {e}"),
            };
            assert_eq!(
                submit_status,
                reqwest::StatusCode::BAD_REQUEST,
                "{resp_text}"
            );
        },
    )
    .await;
}

async fn make_submission_and_wait_for_success(port: u16, source_file: impl AsRef<Path>) {
    let client = reqwest::Client::new();

    let mut source_path = PathBuf::from_iter(["riscv-samples", "src"]);
    source_path.push(source_file.as_ref());
    let original_code = String::from_utf8_lossy(&fs::read(&source_path).await.unwrap()).to_string();

    let mut ticks_path = PathBuf::from_iter(["riscv-samples", "cfg"]);
    ticks_path.push(source_file.as_ref().file_stem().unwrap());
    let ticks = String::from_utf8_lossy(&fs::read(ticks_path).await.unwrap()).to_string();
    let ticks = ticks.trim().parse().unwrap();

    let start = Instant::now();

    let submit_response = submit_program(&client, port, ticks, &source_path).await;
    let submit_status = submit_response.status();
    assert_eq!(submit_status, reqwest::StatusCode::ACCEPTED);
    let submit_response = parse_response_json::<SubmitResponse>(submit_response).await;
    info!("Got submission id: {}", submit_response.ulid);

    let timeout = Duration::from_secs_f32(WAIT_TIMEOUT);
    let submission_response = tokio::time::timeout(
        timeout,
        wait_submission(&client, port, submit_response.ulid),
    )
    .await
    .unwrap();
    let dur = Instant::now().duration_since(start);
    info!("Waited for {:.2} seconds", dur.as_secs_f32());

    let submission_response = parse_response_json::<SubmissionResponse>(submission_response).await;
    assert_eq!(submission_response.ulid, submit_response.ulid);
    assert_eq!(submission_response.ticks, ticks);
    assert_eq!(submission_response.code, original_code);
    verify_submission_trace(submission_response, &source_path).await;
}

async fn verify_submission_trace(submission_response: SubmissionResponse, source_path: &Path) {
    info!("Checking trace VS actual run of risc-v-sim");
    let mut filename = PathBuf::from(source_path.file_name().unwrap());
    filename.set_extension("json");
    let mut path = PathBuf::from("traces");
    path.push(filename);

    let data = fs::read(path).await.unwrap();
    let actual_trace: serde_json::Value = serde_json::from_slice(&data).unwrap();
    let actual_trace = actual_trace.as_object().unwrap();
    let actual_steps = &actual_trace["steps"];
    assert_eq!(actual_steps, &submission_response.steps);
    info!("Traces match");
}

async fn wait_submission(client: &Client, port: u16, submission_id: Ulid) -> Response {
    loop {
        let response = get_submission(&client, port, submission_id).await;
        match response.status() {
            reqwest::StatusCode::OK => (),
            reqwest::StatusCode::NOT_FOUND => {
                info!("Submission {submission_id} is not ready");
                tokio::time::sleep(Duration::from_secs_f32(0.5)).await;
                continue;
            }
            status => panic!("Unexpected HTTP status {status}"),
        }
        info!("Submission {submission_id} is ready");
        break response;
    }
}
