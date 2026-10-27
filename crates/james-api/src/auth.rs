//! Authentication (bearer token or session cookie) and permission checks.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum_extra::extract::CookieJar;
use james_db::permissions::{allowed, Grant};
use james_db::users::User;

use crate::error::ApiError;
use crate::AppState;

pub const SESSION_COOKIE: &str = "james_session";

/// An authenticated account with its effective grants.
#[derive(Debug, Clone)]
pub struct Auth {
    pub user: User,
    pub grants: Vec<Grant>,
}

impl Auth {
    pub fn can(&self, permission: &str, instance: Option<&str>) -> bool {
        allowed(&self.grants, permission, instance)
    }

    pub fn require(&self, permission: &str, instance: Option<&str>) -> Result<(), ApiError> {
        if self.can(permission, instance) {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token.trim().to_owned());
        }
    }
    CookieJar::from_headers(headers)
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_owned())
}

pub async fn authenticate(state: &AppState, token: &str) -> Result<Auth, ApiError> {
    let user = state
        .db
        .token_user(token)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let grants = state.db.user_grants(user.id).await?;
    Ok(Auth { user, grants })
}

impl<S> FromRequestParts<S> for Auth
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(state);
        let token = token_from_headers(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        authenticate(&state, &token).await
    }
}
