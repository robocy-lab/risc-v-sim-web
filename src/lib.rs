pub mod api;
pub mod auth;
pub mod database;
pub mod submission_actor;

use axum::{Extension, Router, body::Body, http::Request, middleware, routing::get};
use std::sync::Arc;
use tokio::{join, net::TcpListener};
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{Instrument, info_span};

use crate::database::DbClient;
use auth::{AuthConfig, auth_middleware};
use submission_actor::{Config as ActorConfig, SubmissionTask, run_submission_actor};

pub struct Config {
    pub actor_config: ActorConfig,
    pub auth_config: AuthConfig,
    pub db: Arc<DbClient>,
}

pub async fn health_handler() -> &'static str {
    "Ok"
}

pub async fn run(root_span: tracing::Span, listener: TcpListener, cfg: Config) {
    let (task_send, task_recv) = tokio::sync::mpsc::channel::<SubmissionTask>(100);
    let config = Arc::new(cfg);

    let submission_actor = run_submission_actor(
        Arc::new(config.actor_config.clone()),
        config.db.clone(),
        task_recv,
    )
    .instrument(info_span!("submission_actor"));

    let submissions_dir = ServeDir::new(&config.actor_config.submissions_folder);

    let router = Router::new()
        .nest(
            "/api",
            api::api_routes()
                .nest_service("/files", submissions_dir)
                .layer(Extension(task_send))
                .with_state(config.clone())
                .layer(middleware::from_fn_with_state(
                    config.clone(),
                    auth_middleware,
                )),
        )
        .nest("/auth", auth::auth_routes().with_state(config.clone()))
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

    let (res, _) = join!(axum::serve(listener, router), submission_actor,);
    res.unwrap();
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
