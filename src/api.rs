use std::sync::Arc;

use anyhow::Context;
use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::mpsc::Sender;
use ulid::Ulid;

use crate::Config;
use crate::auth::User;
use crate::database::SubmissionRecord;
use crate::submission_actor::SubmissionTask;

pub fn api_routes() -> Router<Arc<Config>> {
    Router::new()
        .route(
            "/submission",
            post(create_submission_handler).get(list_submissions_handler),
        )
        .route("/submission/{ulid}", get(get_submission_handler))
        .route(
            "/submission/{ulid}/source",
            get(get_submission_source_handler),
        )
        .route(
            "/submission/{ulid}/trace",
            get(get_submission_trace_handler),
        )
        .route("/me", get(me_handler))
}

async fn me_handler(Extension(user): Extension<User>) -> Json<User> {
    Json(user)
}

async fn get_submission_handler(
    State(config): State<Arc<Config>>,
    Path(ulid): Path<Ulid>,
) -> ApiResult<SubmissionRecord> {
    let record = config
        .db
        .get_submission_by_uuid(ulid)
        .await
        .context("fetch")
        .map_err(ApiError::internal_error)?;

    match record {
        Some(r) => Ok(Json(r)),
        None => Err(ApiError::submission_not_found()),
    }
}

async fn get_submission_source_handler(
    State(config): State<Arc<Config>>,
    Path(ulid): Path<Ulid>,
) -> ApiResult<serde_json::Value> {
    let content = read_simulation_file(&config, ulid).await?;
    let json: serde_json::Value = serde_json::from_slice(&content)
        .context("parsing")
        .map_err(ApiError::internal_error)?;

    let code = json.get("code").cloned().unwrap_or(serde_json::Value::Null);
    Ok(Json(serde_json::json!({ "code": code })))
}

async fn get_submission_trace_handler(
    State(config): State<Arc<Config>>,
    Path(ulid): Path<Ulid>,
) -> ApiResult<serde_json::Value> {
    let content = read_simulation_file(&config, ulid).await?;
    let mut json: serde_json::Value = serde_json::from_slice(&content)
        .context("parsing")
        .map_err(ApiError::internal_error)?;

    if let serde_json::Value::Object(map) = &mut json {
        map.remove("code");
    }
    Ok(Json(json))
}

async fn read_simulation_file(config: &Config, ulid: Ulid) -> Result<Vec<u8>, ApiError> {
    let path = crate::submission_file(&config.actor_config, ulid);
    match fs::read(path).await {
        Ok(x) => Ok(x),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::submission_not_found()),
        Err(e) => {
            let cause = anyhow::Error::from(e).context("loading submission");
            Err(ApiError::internal_error(cause))
        }
    }
}

async fn create_submission_handler(
    State(config): State<Arc<Config>>,
    Extension(task_send): Extension<Sender<SubmissionTask>>,
    Extension(user): Extension<User>,
    multipart: Multipart,
) -> ApiResult<CreateSubmissionResponse> {
    let ulid = Ulid::new();
    let user_id = user.id;
    let user_login = user.login;

    let (ticks, source_code) = parse_submit_inputs(multipart, config.as_ref())
        .await
        .context("parse")
        .map_err(ApiError::bad_request)?;
    tracing::debug!(
        user_id=user_id,
        user_login=user_login,
        submission_id=%ulid,
        size=source_code.len(),
        "New submission",
    );

    if let Err(err) = config.db.create_submission_with_user(ulid, user_id).await {
        return Err(ApiError::internal_error(err));
    }

    task_send
        .send(SubmissionTask {
            source_code,
            ticks,
            ulid,
            user_id,
        })
        .await
        .context("send task")
        .map_err(ApiError::internal_error)?;

    Ok(Json(CreateSubmissionResponse { ulid }))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSubmissionResponse {
    pub ulid: Ulid,
}

async fn parse_submit_inputs(
    mut multipart: Multipart,
    config: &Config,
) -> anyhow::Result<(u32, bytes::Bytes)> {
    let mut ticks: Option<u32> = None;
    let mut file: Option<bytes::Bytes> = None;

    while let Some(field) = multipart.next_field().await? {
        let Some(name) = field.name() else {
            anyhow::bail!("field without name")
        };
        match name {
            "ticks" => ticks = Some(ticks_from_field(field).await.context("parsing ticks")?),
            "file" => file = Some(field.bytes().await.context("parsing file")?),
            name => anyhow::bail!("unknown field {name:?}"),
        }
    }

    let Some(ticks) = ticks else {
        anyhow::bail!("ticks field not set")
    };
    let Some(file) = file else {
        anyhow::bail!("file field not set")
    };
    if ticks > config.actor_config.ticks_max {
        anyhow::bail!("ticks number exceeds {}", config.actor_config.ticks_max)
    }
    if file.len() > config.actor_config.codesize_max as usize {
        anyhow::bail!("file length exceeds {}", config.actor_config.codesize_max)
    }
    Ok((ticks, file))
}

async fn ticks_from_field(field: Field<'_>) -> anyhow::Result<u32> {
    let ticks_str = field.text().await?;
    Ok(ticks_str.parse()?)
}

async fn list_submissions_handler(
    State(config): State<Arc<Config>>,
    Extension(user): Extension<User>,
) -> ApiResult<UserSubmissionsResponse> {
    let submissions = config
        .db
        .get_user_submissions(user.id)
        .await
        .context("fetch")
        .map_err(ApiError::internal_error)?;

    Ok(Json(UserSubmissionsResponse { submissions }))
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;

#[derive(Debug, Serialize, Deserialize)]
pub struct UserSubmissionsResponse {
    pub submissions: Vec<SubmissionRecord>,
}

pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub cause: anyhow::Error,
}

impl ApiError {
    pub fn internal_error(cause: anyhow::Error) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            cause,
        }
    }

    pub fn bad_request(cause: anyhow::Error) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            cause,
        }
    }

    pub fn submission_not_found() -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            code: "submission_not_found",
            cause: anyhow::anyhow!("Submission not found"),
        }
    }

    pub fn unauthorized() -> Self {
        ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            cause: anyhow::anyhow!("Unauthorized access"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::error!(err_code = self.code, "error: {:#}", self.cause);

        let err = format!("{:#}", self.cause);
        let body = Json(ApiErrorResponse {
            err,
            code: self.code,
        });

        (self.status, body).into_response()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub code: &'static str,
    pub err: String,
}
