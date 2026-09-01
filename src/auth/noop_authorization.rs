use axum::extract::Request;
use axum_extra::extract::CookieJar;

use crate::AppState;
use crate::api::ApiError;

use super::User;

pub const ADMIN_TOKEN: &'static str = "ADMIN-0000";

pub fn get_user(
    _state: &AppState,
    _cookie_jar: &CookieJar,
    request: &Request<axum::body::Body>,
) -> Result<User, ApiError> {
    let Some(auth) = request.headers().get(axum::http::header::AUTHORIZATION) else {
        return Err(ApiError::unauthorized());
    };

    if auth.as_bytes() != ADMIN_TOKEN.as_bytes() {
        return Err(ApiError::unauthorized());
    }

    Ok(User {
        id: 0,
        login: "admin".to_string(),
        name: None,
    })
}
