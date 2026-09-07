pub mod api;
pub mod auth;
pub mod database;
pub mod submission_actor;

use axum::{Router, body::Body, http::Request, middleware, routing::get};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::{join, net::TcpListener};
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::database::DbClient;
use crate::submission_actor::SubmissionActor;
use auth::auth_middleware;
use submission_actor::SubmissionTask;

pub struct Config {
    /* Submission processing configuration */
    /// Path to the assembler program, capable of producing RiscV object code.
    pub as_binary: PathBuf,
    /// Path to the linker program, capable of producing RiscV elfs.
    pub ld_binary: PathBuf,
    /// Path to a compiler risc-v-sim.
    pub simulator_binary: PathBuf,
    /// Path to a folder, that will be used to store submission artifacts.
    pub submissions_folder: PathBuf,
    /// Max of amount of risc-v-sim allowed.
    pub ticks_max: u32,
    /// Max size of the upload in bytes.
    pub codesize_max: u32,

    /* Db configuration */
    /// URI of the mongo server.
    pub mongo_uri: String,
    /// Mongo database name.
    pub db_name: String,

    #[cfg(feature = "github_authentication")]
    pub github_authentication: auth::github_authentication::AuthConfig,

    #[cfg(feature = "jwt_authorization")]
    pub jwt_authorization: auth::jwt_authorization::AuthConfig,
}

pub struct AppState {
    pub ticks_max: u32,
    pub codesize_max: u32,
    pub submissions_folder: PathBuf,
    pub db: Arc<DbClient>,
    pub task_send: Sender<SubmissionTask>,

    #[cfg(feature = "github_authentication")]
    pub github_authentication: auth::github_authentication::AuthState,

    #[cfg(feature = "jwt_authorization")]
    pub jwt_authorization: auth::jwt_authorization::AuthState,
}

pub async fn health_handler() -> &'static str {
    "Ok"
}

pub async fn run(
    root_span: tracing::Span,
    listener: TcpListener,
    cfg: Config,
) -> anyhow::Result<()> {
    let (task_send, task_recv) = tokio::sync::mpsc::channel::<SubmissionTask>(100);
    let db_client = DbClient::new(&cfg.mongo_uri, &cfg.db_name).await?;

    let state = Arc::new(AppState {
        db: Arc::new(db_client),
        task_send,
        ticks_max: cfg.ticks_max,
        codesize_max: cfg.codesize_max,
        submissions_folder: cfg.submissions_folder.clone(),

        #[cfg(feature = "github_authentication")]
        github_authentication: auth::github_authentication::AuthState::new(
            cfg.github_authentication,
        ),

        #[cfg(feature = "jwt_authorization")]
        jwt_authorization: auth::jwt_authorization::AuthState::load(cfg.jwt_authorization).await?,
    });

    let submission_actor = SubmissionActor::new(
        task_recv,
        state.db.clone(),
        cfg.as_binary,
        cfg.ld_binary,
        cfg.simulator_binary,
        cfg.submissions_folder,
    );

    let router = Router::new()
        .nest(
            "/api",
            api::api_routes()
                .with_state(state.clone())
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    auth_middleware,
                )),
        )
        .nest("/auth", auth::auth_routes().with_state(state.clone()))
        .route("/health", get(health_handler))
        .fallback_service(ServeDir::new("static"))
        .layer(ServiceBuilder::new().layer(tower_http::cors::CorsLayer::permissive()))
        .layer(
            TraceLayer::new_for_http().make_span_with(move |request: &Request<Body>| {
                tracing::debug_span!(
                    parent: &root_span,
                    "request",
                    method = %request.method(),
                    uri = %request.uri(),
                    version = ?request.version(),
                )
            }),
        );

    let (res, _) = join!(axum::serve(listener, router), submission_actor.run(),);
    res.map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn test_health_handler() {
        let response = health_handler().await;
        assert_eq!(response, "Ok");
    }
}
