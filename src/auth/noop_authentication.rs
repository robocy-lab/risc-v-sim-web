use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use axum::routing::post;
use axum_extra::extract::cookie::CookieJar;
use std::sync::Arc;

use super::User;

use crate::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/login", post(noop_login))
}

async fn noop_login(
    State(state): State<Arc<AppState>>,
    Query(user): Query<User>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), StatusCode> {
    let jwt_res =
        super::jwt_authorization::new_jwt_cookie_from_user(&state.jwt_authorization, &user);
    match jwt_res {
        Ok(cookie) => Ok((jar.add(cookie), Redirect::to("/"))),
        Err(err) => {
            tracing::error!("failed to sign a token for noop login: {err:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
