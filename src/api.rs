use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::body::Body;
use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::Sender;
use tokio_util::io::ReaderStream;
use ulid::Ulid;

use crate::AppState;
use crate::auth::User;
use crate::database::SubmissionRecord;
use crate::submission_actor::{SubmissionTask, source_file, submission_file};

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/submission",
            post(create_submission_handler).get(list_submissions_handler),
        )
        .route("/submission/{ulid}", get(get_submission_handler))
        .route(
            "/submission/{ulid}/trace",
            get(get_submission_trace_handler),
        )
        .route(
            "/submission/{ulid}/source",
            get(get_submission_source_handler),
        )
        .route("/me", get(me_handler))
}

async fn get_submission_trace_handler(
    State(state): State<Arc<AppState>>,
    Path(ulid): Path<Ulid>,
) -> Response {
    serve_file(
        submission_file(&state.actor_config, ulid),
        "application/json",
    )
    .await
}

async fn get_submission_source_handler(
    State(state): State<Arc<AppState>>,
    Path(ulid): Path<Ulid>,
) -> Response {
    serve_file(source_file(&state.actor_config, ulid), "application/json").await
}

async fn serve_file(path: PathBuf, content_type: &'static str) -> Response {
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let stream = ReaderStream::with_capacity(file, 16 * 1024);
    let mut res = Response::new(Body::from_stream(stream));
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    res
}

async fn me_handler(Extension(user): Extension<User>) -> Json<User> {
    Json(user)
}

async fn get_submission_handler(
    State(state): State<Arc<AppState>>,
    Path(ulid): Path<Ulid>,
) -> ApiResult<SubmissionRecord> {
    let record = state
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

async fn create_submission_handler(
    State(state): State<Arc<AppState>>,
    Extension(task_send): Extension<Sender<SubmissionTask>>,
    Extension(user): Extension<User>,
    multipart: Multipart,
) -> ApiResult<CreateSubmissionResponse> {
    let ulid = Ulid::new();
    let user_id = user.id;
    let user_login = user.login;

    let (ticks, source_code) = parse_submit_inputs(multipart, state.as_ref())
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

    if let Err(err) = state.db.create_submission_with_user(ulid, user_id).await {
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
    state: &AppState,
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
    if ticks > state.actor_config.ticks_max {
        anyhow::bail!("ticks number exceeds {}", state.actor_config.ticks_max)
    }
    if file.len() > state.actor_config.codesize_max as usize {
        anyhow::bail!("file length exceeds {}", state.actor_config.codesize_max)
    }
    Ok((ticks, file))
}

async fn ticks_from_field(field: Field<'_>) -> anyhow::Result<u32> {
    let ticks_str = field.text().await?;
    Ok(ticks_str.parse()?)
}

async fn list_submissions_handler(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<User>,
) -> ApiResult<UserSubmissionsResponse> {
    let submissions = state
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
