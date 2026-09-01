#[cfg(feature = "github_authentication")]
mod github_authentication;
#[cfg(feature = "jwt_authorization")]
mod jwt_authorization;
#[cfg(feature = "noop_authentication")]
mod noop_authentication;
#[cfg(feature = "noop_authorization")]
mod noop_authorization;

#[cfg(feature = "noop_authorization")]
pub use noop_authorization::ADMIN_TOKEN;

use axum::{
    Router,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::post,
};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{AppState, api::ApiError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
}

pub fn auth_routes() -> Router<Arc<AppState>> {
    #[allow(unused_mut)]
    let mut res = Router::new().route("/logout", post(logout_handler));

    #[cfg(feature = "noop_authentication")]
    {
        res = res.nest("/noop", noop_authentication::routes());
    }

    #[cfg(feature = "github_authentication")]
    {
        res = res.nest("/github", github_authentication::routes());
    }

    res
}

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    cookie_jar: CookieJar,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let to_try: [fn(&AppState, &CookieJar, &Request) -> Result<User, ApiError>; _] = [
        #[cfg(feature = "jwt_authorization")]
        jwt_authorization::get_user,
        #[cfg(feature = "noop_authorization")]
        noop_authorization::get_user,
    ];

    for auth_option in to_try {
        let res = auth_option(&state, &cookie_jar, &request);
        match res {
            Ok(user) => {
                request.extensions_mut().insert(user);
                return next.run(request).await;
            }
            Err(e) if e.is_unauthorized() => continue,
            Err(e) => {
                tracing::debug!(path = path, "Unauthorized access");
                return e.into_response();
            }
        }
    }

    return ApiError::unauthorized().into_response();
}

pub async fn logout_handler() -> (CookieJar, Redirect) {
    let mut cookie = Cookie::new("jwt", "");
    cookie.set_path("/");
    cookie.make_removal();

    let jar = CookieJar::new();
    (jar.add(cookie), Redirect::to("/"))
}
