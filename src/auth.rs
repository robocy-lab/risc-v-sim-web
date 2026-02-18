use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    extract::{Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Json, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, Scope, TokenResponse, TokenUrl,
    basic::BasicClient, reqwest::async_http_client,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use time::{Duration, UtcDateTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub oauth_client: BasicClient,
    pub jwt_secret: String,
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

pub fn create_auth_config() -> Result<AuthConfig> {
    let client_id = std::env::var("GITHUB_CLIENT_ID").context("GITHUB_CLIENT_ID not set")?;
    let client_secret =
        std::env::var("GITHUB_CLIENT_SECRET").context("GITHUB_CLIENT_SECRET not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET not set")?;

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

    Ok(AuthConfig {
        oauth_client: client,
        jwt_secret,
    })
}

pub async fn login_handler(
    State(config): State<Arc<crate::Config>>,
) -> Result<Redirect, StatusCode> {
    let (auth_url, _csrf_token) = config
        .auth_config
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
    State(config): State<Arc<crate::Config>>,
    Query(query): Query<AuthQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), StatusCode> {
    let code = AuthorizationCode::new(query.code.clone());

    let token_response = config
        .auth_config
        .oauth_client
        .exchange_code(code)
        .request_async(async_http_client)
        .await
        .map_err(|e| {
            tracing::error!("Failed to exchange code for token: {:?}", e);
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
        .map_err(|e| {
            tracing::error!("Failed to fetch user from GitHub: {:?}", e);
            StatusCode::BAD_REQUEST
        })?;

    let user_data: serde_json::Value = user_response.json().await.map_err(|e| {
        tracing::error!("Failed to parse GitHub user response: {:?}", e);
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

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.auth_config.jwt_secret.as_ref()),
    )
    .map_err(|e| {
        tracing::error!("Failed to create JWT token: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut cookie = Cookie::new("jwt", token);
    cookie.set_path("/");
    cookie.set_max_age(Some(time::Duration::hours(24 * 7)));
    cookie.set_http_only(true);

    Ok((jar.add(cookie), Redirect::to("/")))
}

pub fn auth_routes() -> Router<Arc<crate::Config>> {
    Router::new()
        .route("/login", post(login_handler))
        .route("/callback", get(oauth_callback_handler))
        .route("/logout", post(logout_handler))
}

pub async fn auth_middleware(
    State(config): State<Arc<crate::Config>>,
    cookie_jar: CookieJar,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();

    let token = cookie_jar.get("jwt");
    if let Some(token) = token {
        return match decode::<Claims>(
            token.value(),
            &DecodingKey::from_secret(config.auth_config.jwt_secret.as_ref()),
            &Validation::default(),
        ) {
            Ok(token_data) => {
                request.extensions_mut().insert(User {
                    id: token_data.claims.sub.parse().unwrap_or(0),
                    login: token_data.claims.login,
                    name: token_data.claims.name,
                });
                next.run(request).await
            }
            Err(e) => {
                tracing::debug!("Invalid JWT token: {:?}", e);
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "Invalid authorization token"})),
                )
                    .into_response()
            }
        };
    }

    tracing::debug!("Unauthorized access attempt to {}", path);
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "Authentication required"})),
    )
        .into_response()
}
