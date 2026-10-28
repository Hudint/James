//! Setup, login/logout, own account, API tokens and identity linking.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use james_db::tokens::{TokenInfo, TokenKind};
use james_db::users::{Identity, User};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{Auth, SESSION_COOKIE};
use crate::error::{ApiError, ApiResult};
use crate::throttle::{client_ip, LoginKeys};
use crate::AppState;

const SESSION_LIFETIME: i64 = 30 * 24 * 3600;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/setup", get(setup_status).post(setup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/me/password", post(change_password))
        .route("/me/tokens", get(tokens).post(create_token))
        .route("/me/tokens/{id}", delete(revoke_token))
        .route("/me/identities", get(identities))
        .route("/me/identities/code", post(link_code))
        .route("/me/identities/{platform}/{uid}", delete(unlink))
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupRequest {
    username: String,
    password: String,
    /// The one-time token printed to the server log, proving access to the host.
    setup_token: String,
}

/// The session token is only delivered as an HttpOnly cookie, never in the body.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    user: User,
}

async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({ "required": state.db.user_count().await? == 0 })))
}

/// Creates the first (admin) account. Only possible while no account exists.
async fn setup(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<SetupRequest>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let expected = state.setup_token.lock().clone();
    let Some(expected) = expected else {
        return Err(ApiError::conflict("setup is already complete"));
    };
    if !constant_time_eq(body.setup_token.trim().as_bytes(), expected.as_bytes()) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "invalid setup token"));
    }
    let user = match state.db.create_first_admin(&body.username, &body.password).await {
        Err(james_db::DbError::Conflict) => return Err(ApiError::conflict("setup is already complete")),
        other => other?,
    };
    *state.setup_token.lock() = None;
    session(&state, jar, user).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn login(
    State(state): State<AppState>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    Json(body): Json<Credentials>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let ip = client_ip(&headers, connect.map(|c| c.0 .0), &state.trusted_proxies);
    let attempt = LoginKeys::new(&body.username, ip);
    if let Some(wait) = state.logins.blocked(&attempt) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("too many failed logins, try again in {} s", wait.as_secs().max(1)),
        ));
    }
    if let Some(delay) = state.logins.delay(&attempt) {
        tokio::time::sleep(delay).await;
    }
    match state.db.authenticate(&body.username, &body.password).await? {
        Some(user) => {
            state.logins.succeeded(&attempt);
            session(&state, jar, user).await
        }
        None => {
            state.logins.failed(&attempt);
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid username or password",
            ))
        }
    }
}

async fn session(
    state: &AppState,
    jar: CookieJar,
    user: User,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let token = state
        .db
        .create_token(user.id, TokenKind::Session, "web", Some(SESSION_LIFETIME))
        .await?;
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.secure_cookies)
        .max_age(time::Duration::seconds(SESSION_LIFETIME))
        .build();
    Ok((jar.add(cookie), Json(LoginResponse { user })))
}

async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = crate::auth::token_from_headers(&headers) {
        let _ = state.db.revoke_token(&token).await;
    }
    (
        jar.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Me {
    user: User,
    grants: Vec<james_db::permissions::Grant>,
}

async fn me(auth: Auth) -> Json<Me> {
    Json(Me {
        user: auth.user,
        grants: auth.grants,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordChange {
    current_password: Option<String>,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    Json(body): Json<PasswordChange>,
) -> ApiResult<StatusCode> {
    if auth.user.has_password {
        let current = body.current_password.unwrap_or_default();
        if state
            .db
            .authenticate(&auth.user.username, &current)
            .await?
            .is_none()
        {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "current password is wrong"));
        }
    }
    state
        .db
        .set_password(auth.user.id, Some(&body.new_password))
        .await?;
    // Sign out everywhere else (a leaked session must not survive a password change).
    let current = crate::auth::token_from_headers(&headers);
    state
        .db
        .revoke_user_tokens(auth.user.id, current.as_deref())
        .await?;
    state.plugin_tokens.revoke_user(auth.user.id);
    Ok(StatusCode::NO_CONTENT)
}

async fn tokens(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<TokenInfo>>> {
    Ok(Json(state.db.tokens(auth.user.id).await?))
}

#[derive(Deserialize)]
struct NewToken {
    name: String,
    /// Lifetime in days; omitted = no expiry.
    days: Option<i64>,
}

async fn create_token(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<NewToken>,
) -> ApiResult<Json<serde_json::Value>> {
    let lifetime = match body.days {
        None => None,
        Some(days @ 1..=3650) => Some(days * 24 * 3600),
        Some(_) => return Err(ApiError::bad_request("the lifetime must be 1 to 3650 days")),
    };
    let token = state
        .db
        .create_token(auth.user.id, TokenKind::Api, body.name.trim(), lifetime)
        .await?;
    Ok(Json(json!({ "token": token })))
}

async fn revoke_token(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.db.revoke_token_id(auth.user.id, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn identities(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Identity>>> {
    Ok(Json(state.db.identities(auth.user.id).await?))
}

/// Issues a code the user sends to the bot (`!link <code>`) to link their voice identity.
async fn link_code(State(state): State<AppState>, auth: Auth) -> Json<serde_json::Value> {
    Json(json!({ "code": state.links.issue(auth.user.id), "expiresInSecs": 600 }))
}

async fn unlink(
    State(state): State<AppState>,
    auth: Auth,
    Path((platform, uid)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let owner = state
        .db
        .user_by_identity(&platform, &uid)
        .await?
        .ok_or_else(|| ApiError::not_found("identity"))?;
    if owner.id != auth.user.id {
        if !auth.can("users.manage", None) {
            return Err(ApiError::not_found("identity"));
        }
        super::users::ensure_can_manage(&state, &auth, &owner).await?;
    }
    state.db.unlink_identity(&platform, &uid).await?;
    Ok(StatusCode::NO_CONTENT)
}
