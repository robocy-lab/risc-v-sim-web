use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Redirect,
    routing::{get, post},
};
use axum_extra::extract::CookieJar;
use oauth2::{AuthorizationCode, CsrfToken, Scope, TokenResponse, reqwest::async_http_client};
use serde::Deserialize;
use std::sync::Arc;

use crate::{AppState, auth::User};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", post(oauth_consent_redirect))
        .route("/callback", get(oauth_callback_handler))
}

async fn oauth_consent_redirect(
    State(state): State<Arc<AppState>>,
) -> Result<Redirect, StatusCode> {
    let (auth_url, _csrf_token) = state
        .oauth_client
        .authorize_url(CsrfToken::new_random)
        .add_scopes([
            Scope::new("openid".into()),
            Scope::new("profile".into()),
            Scope::new("name".into()),
        ])
        .add_extra_param("prompt", "consent")
        .url();

    Ok(Redirect::to(auth_url.as_str()))
}

#[derive(Debug, Deserialize)]
pub struct AuthQuery {
    code: String,
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
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let access_token = token_response.access_token().secret();
    let client = reqwest::Client::new();

    let user_response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        // GitHub API requires the User-Agent header.
        // GitHub recomments it to be set to the application name.
        // REF: https://docs.github.com/en/rest/using-the-rest-api/getting-started-with-the-rest-api?apiVersion=2026-03-10#user-agent
        .header(reqwest::header::USER_AGENT, "rvsim-test")
        .send()
        .await
        .map_err(|err| {
            tracing::error!("Failed to fetch user from GitHub: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let github_user: GithubUser = user_response.json().await.map_err(|err| {
        tracing::error!("Failed to parse GitHub user response: {err:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let user = User {
        id: github_user.id,
        login: github_user.login,
        name: github_user.name,
    };

    let jwt_res = super::jwt_authorization::new_jwt_cookie_from_user(&state, &user);
    let cookie = match jwt_res {
        Ok(cookie) => cookie,
        Err(err) => {
            tracing::error!("failed to sign a token for noop login: {err:#}");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    tracing::info!(
        "Authorized Github user: id={}, login={}, name={:?}",
        user.id,
        user.login,
        user.name
    );

    Ok((jar.add(cookie), Redirect::to("/")))
}

#[derive(Deserialize)]
struct GithubUser {
    login: String,
    id: i64,
    name: Option<String>,
}
