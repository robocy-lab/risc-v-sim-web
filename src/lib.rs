use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::Body,
    extract::{Multipart, Query, State, multipart::Field},
    http::{Request, StatusCode},
    response::Json,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{io::ErrorKind, time::Duration};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::time::timeout;
use tokio::{fs, net::TcpListener};
use tokio::{io::AsyncWriteExt, process::Command};
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{Instrument, error, info, info_span};
use ulid::{ULID_LEN, Ulid};

pub mod auth;

#[derive(Deserialize)]
pub struct Submission {
    ulid: Ulid,
}

pub async fn health_handler() -> &'static str {
    "Ok"
}

pub async fn compile_s_to_elf(
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

pub async fn run_simulator(config: &Config, submission_dir: &Path, ticks: u32) -> Result<String> {
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

    // FIXME: This will dump simulator logs.
    //        Not something we actually want to do.
    if !output.status.success() {
        bail!("simulator failed: {stderr}");
    }

    info!("Simulating has been successful");
    Ok(stdout)
}

pub async fn parse_submit_inputs(
    mut multipart: Multipart,
    config: &Config,
) -> Result<(u32, bytes::Bytes)> {
    let mut ticks: Option<u32> = None;
    let mut file: Option<bytes::Bytes> = None;

    while let Some(field) = multipart.next_field().await? {
        let Some(name) = field.name() else {
            bail!("field without name")
        };
        match name {
            "ticks" => ticks = Some(ticks_from_field(field).await.context("parsing ticks")?),
            "file" => file = Some(field.bytes().await.context("parsing file")?),
            name => bail!("unknown field {name:?}"),
        }
    }

    let Some(ticks) = ticks else {
        bail!("ticks field not set")
    };
    let Some(file) = file else {
        bail!("file field not set")
    };
    if ticks >= config.ticks_max {
        bail!("ticks number exceeds {}", config.ticks_max)
    }
    if file.len() >= config.codesize_max as usize {
        bail!("file length exceeds {}", config.codesize_max)
    }
    Ok((ticks, file))
}

async fn ticks_from_field(field: Field<'_>) -> Result<u32> {
    let ticks_str = field.text().await?;
    Ok(ticks_str.parse()?)
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
    .await
    .context("simulation")?;

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

async fn future_with_timeout<T>(
    duration: Duration,
    f: impl Future<Output = Result<T>>,
) -> Result<T> {
    timeout(duration, f)
        .await
        .map_err(anyhow::Error::from)
        .flatten()
}

async fn submit_handler(
    State(config): State<Arc<Config>>,
    multipart: Multipart,
) -> (StatusCode, Json<serde_json::Value>) {
    let (ticks, source_code) = match parse_submit_inputs(multipart, config.as_ref())
        .await
        .context("parse input")
    {
        Ok(x) => x,
        Err(e) => {
            info!("Bad request: {e:#}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("{e:#}"),
                })),
            );
        }
    };
    info!(
        "Received {} bytes of program code to run for {ticks} ticks",
        source_code.len()
    );

    let ulid = Ulid::new();
    let submission_dir = submission_dir(&config, ulid);
    if let Err(e) = fs::create_dir_all(&submission_dir).await {
        error!("can't create {:#?}: {e}", submission_dir);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json::from(serde_json::Value::Null),
        );
    }

    tokio::spawn(
        async move {
            let res = simulate(&config, ulid, source_code, ticks)
                .await
                .unwrap_or_else(|e| {
                    error!("Simulation task failed: {e:?}");
                    serde_json::json!({
                        "error": format!("{e:?}"),
                    })
                });

            if let Err(e) = fs::write(submission_file(&config, ulid), res.to_string()).await {
                error!("failed to write submission result: {e}");
            };
        }
        .instrument(info_span!(parent: tracing::Span::current(), "submit_task", ulid = %ulid)),
    );

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ulid": ulid
        })),
    )
}

async fn submission_handler(
    State(config): State<Arc<Config>>,
    submission: Query<Submission>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let submission = submission_file(&config, submission.ulid);
    let content = match fs::read(submission).await {
        Ok(x) => x,
        Err(e) => {
            if e.kind() == ErrorKind::NotFound {
                return (StatusCode::NOT_FOUND, Json(serde_json::Value::Null));
            } else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::Value::Null),
                );
            }
        }
    };
    let json_content = Json::from_bytes(&content);
    if let Err(e) = json_content {
        error!("{e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::Value::Null),
        );
    }
    (StatusCode::OK, json_content.unwrap())
}

pub struct Config {
    pub as_binary: PathBuf,
    pub ld_binary: PathBuf,
    pub simulator_binary: PathBuf,
    pub submissions_folder: PathBuf,
    pub ticks_max: u32,
    pub codesize_max: u32,
    pub auth_state: auth::AuthState,
}

pub async fn run(root_span: tracing::Span, listener: TcpListener, cfg: Config) {
    let def_span = move |request: &Request<Body>| {
        tracing::debug_span!(
            parent: &root_span,
            "request",
            method = %request.method(),
            uri = %request.uri(),
            version = ?request.version(),
        )
    };

    let state = Arc::new(cfg);
    let api_router = Router::new()
        .route("/health", get(health_handler))
        .route("/submit", post(submit_handler))
        .route("/submission", get(submission_handler))
        .nest("/auth", auth::auth_routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    let router = Router::new()
        .nest("/api", api_router)
        .nest("/auth", auth::auth_routes())
        .fallback_service(ServeDir::new("static"))
        .layer(ServiceBuilder::new().layer(tower_http::cors::CorsLayer::permissive()))
        .layer(TraceLayer::new_for_http().make_span_with(def_span))
        .with_state(state);

    axum::serve(listener, router).await.unwrap();
}

fn submission_dir(config: &Config, ulid: Ulid) -> PathBuf {
    let mut buf = [0u8; ULID_LEN];
    let ulid_str = ulid.array_to_str(&mut buf);
    config.submissions_folder.join(&ulid_str)
}

fn submission_file(config: &Config, ulid: Ulid) -> PathBuf {
    let mut buf = [0u8; ULID_LEN];
    let ulid_str = ulid.array_to_str(&mut buf);
    let mut path = config.submissions_folder.clone();
    path.extend([&ulid_str, "simulation.json"]);

    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::u32;

    fn create_test_config() -> Config {
        unsafe {
            std::env::set_var("GITHUB_CLIENT_ID", "test_client_id");
            std::env::set_var("GITHUB_CLIENT_SECRET", "test_client_secret");
            std::env::set_var("JWT_SECRET", "test_jwt_secret");
        }

        Config {
            as_binary: "dummy".into(),
            ld_binary: "dummy".into(),
            simulator_binary: "dummy".into(),
            submissions_folder: "submissions".into(),
            ticks_max: u32::MAX,
            codesize_max: u32::MAX,
            auth_state: auth::create_auth_state().unwrap(),
        }
    }

    #[tokio::test]
    async fn test_health_handler() {
        let response = health_handler().await;
        assert_eq!(response, "Ok");
    }

    #[test]
    fn test_path_utils() {
        let config = create_test_config();
        for _ in 0..10 {
            let ulid = Ulid::new();
            let dir = submission_dir(&config, ulid);
            let file = submission_file(&config, ulid);
            assert!(file.starts_with(dir));
        }
    }

    #[test]
    fn test_auth_state_creation() {
        unsafe {
            std::env::set_var("GITHUB_CLIENT_ID", "test_client_id");
            std::env::set_var("GITHUB_CLIENT_SECRET", "test_client_secret");
            std::env::set_var("JWT_SECRET", "test_jwt_secret");
        }

        let auth_state = auth::create_auth_state();
        assert!(auth_state.is_ok());
    }

    #[test]
    fn test_jwt_creation_and_validation() {
        let config = create_test_config();

        let claims = auth::Claims {
            sub: "123".to_string(),
            login: "testuser".to_string(),
            name: Some("Test User".to_string()),
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        };

        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(config.auth_state.jwt_secret.as_ref()),
        )
        .unwrap();

        let decoded = jsonwebtoken::decode::<auth::Claims>(
            &token,
            &jsonwebtoken::DecodingKey::from_secret(config.auth_state.jwt_secret.as_ref()),
            &jsonwebtoken::Validation::default(),
        )
        .unwrap();

        assert_eq!(decoded.claims.login, "testuser");
        assert_eq!(decoded.claims.sub, "123");
    }
}
