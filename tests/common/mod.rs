use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;

use jsonwebtoken::{EncodingKey, Header, encode};
use reqwest::{Client, Response, Url};
use time::{Duration, UtcDateTime};
use tokio::{net::TcpListener, task::JoinHandle};
use tracing::{Instrument, Level, Span, info};
use ulid::Ulid;

pub const TEST_MONGO_URI: &'static str = "mongodb://mongodb:27017";
pub const TEST_DB_NAME: &'static str = "riscv_sim";

#[allow(dead_code)]
pub async fn run_test<Patch, Body, F>(test_name: &str, patch_cfg: Patch, body: Body)
where
    Patch: FnOnce(&mut risc_v_sim_web::Config),
    Body: FnOnce(u16) -> F,
    F: Future<Output = ()>,
{
    init_test();
    let mut cfg = default_config(test_name).await;
    patch_cfg(&mut cfg);

    let span = tracing::info_span!("test", test_name = test_name);
    let (port, server_task) = spawn_server(&span, cfg).await;
    body(port).instrument(span).await;
    server_task.abort();
}

#[allow(dead_code)]
pub fn init_test() {
    // Tests run in parallel, so some might have already created the logger.
    let _ = tracing_subscriber::fmt()
        .with_level(true)
        .with_max_level(Level::DEBUG)
        .try_init();
}

/// Spawns a risc-v-sim-web instance, listening on the specified port.
/// Make sure to .await the result of this function as soon as possible to
/// avoid any weird bugs.
/// The function returns a JoinHandle. For quick and clean test termination,
/// make sure to [`JoinHandle::abort()`] the returned future.
#[allow(dead_code)]
pub async fn spawn_server(
    span: &Span,
    cfg: risc_v_sim_web::Config,
) -> (u16, JoinHandle<anyhow::Result<()>>) {
    let (port, listener) = make_listener().instrument(span.clone()).await;
    let task = tokio::spawn(risc_v_sim_web::run(span.clone(), listener, cfg));
    (port, task)
}

#[allow(dead_code)]
async fn make_listener() -> (u16, TcpListener) {
    // NOTE: we specifically create a listener on the same thread and make the
    //       caller wait. This is because we want to make sure the server properly
    //       reserves the port. Otherwise, the caller's HTTP requests will race
    //       and get a "connection refused" response.
    let address = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    info!("Listening on port {port}");
    (port, listener)
}

#[allow(dead_code)]
pub async fn default_config(test_name: &str) -> risc_v_sim_web::Config {
    risc_v_sim_web::Config {
        actor_config: risc_v_sim_web::submission_actor::Config {
            as_binary: std::env::var("AS_BINARY")
                .unwrap_or_else(|_| "riscv64-elf-as".to_string())
                .into(),
            ld_binary: std::env::var("LD_BINARY")
                .unwrap_or_else(|_| "riscv64-elf-ld".to_string())
                .into(),
            simulator_binary: std::env::var("SIMULATOR_BINARY")
                .unwrap_or_else(|_| "simulator".to_string())
                .into(),
            submissions_folder: format!("submissions-{test_name}").into(),
            ticks_max: 15,
            codesize_max: 256,
        },
        mongo_uri: TEST_MONGO_URI.to_string(),
        db_name: TEST_DB_NAME.to_string(),
        client_id: "test_client_id".to_string(),
        client_secret: "test_client_secret".to_string(),
        jwt_secret: "test_secret_key_for_integration_tests".to_string(),
        auth_url: "https://example.com/auth".to_string(),
        token_url: "https://example.com/token".to_string(),
    }
}

#[allow(dead_code)]
pub fn generate_test_token(user_id: &str, login: &str, jwt_secret: &str) -> String {
    let claims = risc_v_sim_web::auth::Claims {
        sub: user_id.to_string(),
        login: login.to_string(),
        name: Some("Test User".to_string()),
        exp: (UtcDateTime::now() + Duration::hours(24)).unix_timestamp(),
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(jwt_secret.as_ref()),
    )
    .unwrap()
}

#[allow(dead_code)]
pub async fn submit_program(
    client: &Client,
    port: u16,
    ticks: u32,
    path: impl AsRef<Path>,
) -> Response {
    let request_url = server_url(port).join("api/submission").unwrap();
    let token = generate_test_token(
        "123456",
        "testuser",
        "test_secret_key_for_integration_tests",
    );
    let cookie = format!("jwt={}", token);

    let form = reqwest::multipart::Form::new()
        .text("ticks", ticks.to_string())
        .file("file", path)
        .await
        .unwrap();
    client
        .post(request_url)
        .header("Cookie", cookie)
        .multipart(form)
        .send()
        .await
        .unwrap()
}

#[allow(dead_code)]
pub async fn get_submission(client: &Client, port: u16, submission_id: Ulid) -> Response {
    let request_url = server_url(port)
        .join(&format!("api/submission/{submission_id}/trace"))
        .unwrap();
    let token = generate_test_token(
        "123456",
        "testuser",
        "test_secret_key_for_integration_tests",
    );
    let cookie = format!("jwt={}", token);

    client
        .get(request_url)
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap()
}

#[allow(dead_code)]
pub async fn get_submission_source(client: &Client, port: u16, submission_id: Ulid) -> Response {
    let request_url = server_url(port)
        .join(&format!("api/submission/{submission_id}/source"))
        .unwrap();
    let token = generate_test_token(
        "123456",
        "testuser",
        "test_secret_key_for_integration_tests",
    );
    let cookie = format!("jwt={}", token);

    client
        .get(request_url)
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap()
}

#[allow(dead_code)]
pub fn server_url(port: u16) -> Url {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    Url::parse(&format!("http://{addr}")).unwrap()
}

#[allow(dead_code)]
pub async fn parse_response_json<T>(response: Response) -> T
where
    T: for<'a> serde::Deserialize<'a>,
{
    let response_bytes = response.bytes().await.unwrap();
    serde_json::from_slice::<T>(&response_bytes).unwrap()
}
