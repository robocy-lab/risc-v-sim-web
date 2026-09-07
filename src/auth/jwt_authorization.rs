use std::path::PathBuf;

use anyhow::Context;
use axum::extract::Request;
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use clap::Args;
use jsonwebtoken::{Validation, decode};
use serde::{Deserialize, Serialize};
use time::{Duration, UtcDateTime};
use tokio::fs;

use crate::AppState;
use crate::api::ApiError;

use super::User;

#[derive(Args, Debug)]
pub struct AuthArgs {
    #[arg(long)]
    pub jwt_secret_path: PathBuf,
}

pub struct AuthConfig {
    /// JWT secret used to sign user's claims.
    pub jwt_secret_path: PathBuf,
}

impl AuthConfig {
    pub fn from_flags(args: AuthArgs) -> Self {
        AuthConfig {
            jwt_secret_path: args.jwt_secret_path,
        }
    }
}

pub struct AuthState {
    pub jwt_encoding_key: jsonwebtoken::EncodingKey,
    pub jwt_decoding_key: jsonwebtoken::DecodingKey,
}

impl AuthState {
    pub async fn load(cfg: AuthConfig) -> anyhow::Result<Self> {
        let jwt_secret = fs::read(&cfg.jwt_secret_path).await?;

        Ok(AuthState {
            jwt_encoding_key: jsonwebtoken::EncodingKey::from_secret(&jwt_secret),
            jwt_decoding_key: jsonwebtoken::DecodingKey::from_secret(&jwt_secret),
        })
    }
}

pub fn new_jwt_cookie_from_user(state: &AuthState, user: &User) -> anyhow::Result<Cookie<'static>> {
    let claims = Claims {
        sub: user.id.to_string(),
        login: user.login.clone(),
        name: user.name.clone(),
        exp: (UtcDateTime::now() + Duration::hours(24 * 7)).unix_timestamp(),
    };

    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &state.jwt_encoding_key,
    )
    .context("failed to encode JWT")?;

    let mut cookie = Cookie::new("jwt", token);
    cookie.set_path("/");
    cookie.set_max_age(Some(time::Duration::hours(24 * 7)));
    cookie.set_http_only(true);
    Ok(cookie)
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

pub fn get_user(
    state: &AppState,
    cookie_jar: &CookieJar,
    _request: &Request<axum::body::Body>,
) -> Result<User, ApiError> {
    let state = &state.jwt_authorization;
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
