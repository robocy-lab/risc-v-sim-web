use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Redirect, Response},
    routing::get,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, Scope, TokenResponse, TokenUrl,
    basic::BasicClient, reqwest::async_http_client,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthState {
    pub client: BasicClient,
    pub jwt_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthQuery {
    code: String,
    state: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    user: User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub login: String,
    pub name: Option<String>,
    pub exp: i64,
}

pub fn create_auth_state() -> Result<AuthState> {
    let client_id = std::env::var("GITHUB_CLIENT_ID").context("GITHUB_CLIENT_ID not set")?;
    let client_secret =
        std::env::var("GITHUB_CLIENT_SECRET").context("GITHUB_CLIENT_SECRET not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET not set")?;

    tracing::info!(
        "Creating OAuth client with client_id: {}...",
        &client_id[..client_id.len().min(8)]
    );

    let auth_url = AuthUrl::new("https://github.com/login/oauth/authorize".to_string())
        .map_err(|e| anyhow!("Invalid auth URL: {}", e))?;
    let token_url = TokenUrl::new("https://github.com/login/oauth/access_token".to_string())
        .map_err(|e| anyhow!("Invalid token URL: {}", e))?;

    let client = BasicClient::new(
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
        auth_url,
        Some(token_url),
    );

    tracing::info!("OAuth client created successfully");

    Ok(AuthState { client, jwt_secret })
}

pub async fn login_handler(
    State(config): State<Arc<crate::Config>>,
) -> Result<Redirect, StatusCode> {
    let (auth_url, _csrf_token) = config
        .auth_state
        .client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("user:email".to_string()))
        .add_scope(Scope::new("read:user".to_string()))
        .url();

    Ok(Redirect::to(auth_url.as_str()))
}

pub async fn logout_handler(_config: State<Arc<crate::Config>>) -> Response {
    let cookie = "jwt=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax".to_string();
    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", "/")
        .header("Set-Cookie", cookie)
        .body(axum::body::Body::empty())
        .unwrap()
}

pub async fn callback_handler(
    State(config): State<Arc<crate::Config>>,
    Query(query): Query<AuthQuery>,
) -> Result<Response, StatusCode> {
    tracing::info!(
        "Received OAuth callback with code length: {} and state: {}",
        query.code.len(),
        query.state
    );

    let code = AuthorizationCode::new(query.code.clone());

    let token_response = config
        .auth_state
        .client
        .exchange_code(code)
        .request_async(async_http_client)
        .await
        .map_err(|e| {
            tracing::error!("Failed to exchange code for token: {:?}", e);
            StatusCode::BAD_REQUEST
        })?;

    let access_token = token_response.access_token().secret();

    let client = reqwest::Client::new();
    tracing::info!("Fetching user data from GitHub API");

    let user_response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "risc-v-sim-web")
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Failed to fetch user from GitHub: {:?}", e);
            StatusCode::BAD_REQUEST
        })?;

    tracing::info!("GitHub API response status: {}", user_response.status());

    let user_data: serde_json::Value = user_response.json().await.map_err(|e| {
        tracing::error!("Failed to parse GitHub user response: {:?}", e);
        StatusCode::BAD_REQUEST
    })?;

    let user_id = user_data["id"].as_u64().unwrap_or(0).to_string();
    let login = user_data["login"].as_str().unwrap_or("").to_string();
    let name = user_data["name"].as_str().map(|s| s.to_string());

    tracing::info!("Creating JWT for user: {} (ID: {})", login, user_id);

    let claims = Claims {
        sub: user_id.clone(),
        login: login.clone(),
        name,
        exp: (Utc::now() + Duration::hours(24 * 7)).timestamp(),
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.auth_state.jwt_secret.as_ref()),
    )
    .map_err(|e| {
        tracing::error!("Failed to create JWT token: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let cookie = format!(
        "jwt={}; Path=/; Max-Age=86400; HttpOnly; SameSite=Lax",
        token
    );

    tracing::info!("Redirecting user to / with auth cookie");

    Ok(Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", "/")
        .header("Set-Cookie", cookie)
        .body(axum::body::Body::empty())
        .unwrap())
}

pub async fn me_handler(
    State(config): State<Arc<crate::Config>>,
    headers: HeaderMap,
) -> Result<Json<AuthResponse>, StatusCode> {
    let auth_header = headers
        .get("cookie")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .split("jwt=")
        .nth(1)
        .and_then(|s| s.split(';').next())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.auth_state.jwt_secret.as_ref()),
        &Validation::default(),
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let claims = token_data.claims;
    let user = User {
        id: claims.sub.parse().unwrap_or(0),
        login: claims.login,
        name: claims.name,
        avatar_url: None,
    };

    Ok(Json(AuthResponse { user }))
}

pub fn auth_routes() -> Router<Arc<crate::Config>> {
    Router::new()
        .route("/login", get(login_handler))
        .route("/callback", get(callback_handler))
        .route("/logout", get(logout_handler))
        .route("/me", get(me_handler))
}

pub async fn auth_middleware(
    State(config): State<Arc<crate::Config>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    if path != "/submit" && path != "/submission" {
        return next.run(request).await;
    }

    let headers = request.headers();
    let auth_header = headers.get("cookie").and_then(|h| h.to_str().ok());

    if let Some(auth_header) = auth_header {
        if let Some(token) = auth_header
            .split("jwt=")
            .nth(1)
            .and_then(|s| s.split(';').next())
        {
            match decode::<Claims>(
                token,
                &DecodingKey::from_secret(config.auth_state.jwt_secret.as_ref()),
                &Validation::default(),
            ) {
                Ok(_) => {
                    return next.run(request).await;
                }
                Err(e) => {
                    tracing::warn!("Invalid JWT token: {:?}", e);
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({"error": "Invalid authorization token"})),
                    )
                        .into_response();
                }
            }
        }
    }

    tracing::warn!("Unauthorized access attempt to {}", path);
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "Authentication required"})),
    )
        .into_response()
}
