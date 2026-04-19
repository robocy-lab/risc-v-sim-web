use axum::{
    Router,
    extract::{Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use jsonwebtoken::{Header, Validation, decode, encode};
use oauth2::{AuthorizationCode, CsrfToken, Scope, TokenResponse, reqwest::async_http_client};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use time::{Duration, UtcDateTime};

use crate::{AppState, api::ApiError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthQuery {
    code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    // 'sub' is default in jwt, according to https://datatracker.ietf.org/doc/html/rfc7519#section-4.1.2
    // it means "Subject (whom the token refers to)", as well as 'exp'
    pub sub: String,
    pub login: String,
    pub name: Option<String>,
    pub exp: i64,
}

pub async fn login_handler(
    State(state): State<Arc<crate::AppState>>,
) -> Result<Redirect, StatusCode> {
    let (auth_url, _csrf_token) = state
        .oauth_client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("user:email".to_string()))
        .add_scope(Scope::new("read:user".to_string()))
        .url();

    Ok(Redirect::to(auth_url.as_str()))
}

pub async fn logout_handler() -> (CookieJar, Redirect) {
    let mut cookie = Cookie::new("jwt", "");
    cookie.set_path("/");
    cookie.make_removal();

    let jar = CookieJar::new();
    (jar.add(cookie), Redirect::to("/"))
}

pub async fn oauth_callback_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AuthQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), StatusCode> {
    let code = AuthorizationCode::new(query.code.clone());

    let token_response = state
        .oauth_client
        .exchange_code(code)
        .request_async(async_http_client)
        .await
        .map_err(|err| {
            tracing::error!("Failed to exchange code for token: {err:#}");
            StatusCode::BAD_REQUEST
        })?;

    let access_token = token_response.access_token().secret();

    let client = reqwest::Client::new();

    let user_response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "risc-v-sim-web")
        .send()
        .await
        .map_err(|err| {
            tracing::error!("Failed to fetch user from GitHub: {err:#}");
            StatusCode::BAD_REQUEST
        })?;

    let user_data: serde_json::Value = user_response.json().await.map_err(|err| {
        tracing::error!("Failed to parse GitHub user response: {err:#}");
        StatusCode::BAD_REQUEST
    })?;

    let user_id = user_data["id"].as_u64().unwrap_or(0).to_string();
    let login = user_data["login"].as_str().unwrap_or("").to_string();
    let name = user_data["name"].as_str().map(|s| s.to_string());

    let claims = Claims {
        sub: user_id.clone(),
        login: login.clone(),
        name,
        exp: (UtcDateTime::now() + Duration::hours(24 * 7)).unix_timestamp(),
    };

    let token = encode(&Header::default(), &claims, &state.jwt_encoding_key).map_err(|err| {
        tracing::error!("Failed to create JWT token: {err:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut cookie = Cookie::new("jwt", token);
    cookie.set_path("/");
    cookie.set_max_age(Some(time::Duration::hours(24 * 7)));
    cookie.set_http_only(true);

    Ok((jar.add(cookie), Redirect::to("/")))
}

pub fn auth_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", post(login_handler))
        .route("/callback", get(oauth_callback_handler))
        .route("/logout", post(logout_handler))
}

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    cookie_jar: CookieJar,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    match get_user_from_cookies(&state, &cookie_jar) {
        Ok(user) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        Err(e) => {
            tracing::debug!(path = path, "Unauthorized access");
            e.into_response()
        }
    }
}

fn get_user_from_cookies(state: &AppState, cookie_jar: &CookieJar) -> Result<User, ApiError> {
    let Some(token) = cookie_jar.get("jwt") else {
        return Err(ApiError::unauthorized());
    };

    let claims_result = decode::<Claims>(
        token.value(),
        &state.jwt_decoding_key,
        &Validation::default(),
    );
    match claims_result {
        Ok(token_data) => Ok(User {
            id: token_data.claims.sub.parse().unwrap_or(0),
            login: token_data.claims.login,
            name: token_data.claims.name,
        }),
        Err(err) => {
            tracing::debug!("Invalid JWT token: {err:#}");
            return Err(ApiError::unauthorized());
        }
    }
}
