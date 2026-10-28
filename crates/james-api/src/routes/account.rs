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
        .route("/auth/providers", get(providers))
        .route("/auth/oidc/login", get(oidc_login))
        .route("/auth/oidc/callback", get(oidc_callback))
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
    match state.db.password_login(&body.username, &body.password).await? {
        Some((user, proof)) => {
            state.logins.succeeded(&attempt);
            let cookie = session_cookie(&state, &user, Some(&proof)).await?;
            Ok((jar.add(cookie), Json(LoginResponse { user })))
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
    let cookie = session_cookie(state, &user, None).await?;
    Ok((jar.add(cookie), Json(LoginResponse { user })))
}

/// A new session for `user` as cookie.
/// With a `proof`, only while the credentials the login was checked against still hold.
async fn session_cookie(
    state: &AppState,
    user: &User,
    proof: Option<&james_db::users::Proof>,
) -> ApiResult<Cookie<'static>> {
    let token = match proof {
        Some(proof) => state
            .db
            .create_session(user.id, proof, SESSION_LIFETIME)
            .await?
            .ok_or_else(ApiError::unauthorized)?,
        None => {
            state
                .db
                .create_token(user.id, TokenKind::Session, "web", Some(SESSION_LIFETIME))
                .await?
        }
    };
    Ok(Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.secure_cookies)
        .max_age(time::Duration::seconds(SESSION_LIFETIME))
        .build())
}

// ---- single sign-on ---------------------------------------------------------------------

const OIDC_STATE_COOKIE: &str = "james_oidc";
const OIDC_PATH: &str = "/api/v1/auth/oidc";

/// Login methods besides username and password (shown on the login page).
async fn providers(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "oidc": state.oidc.as_ref().map(|o| json!({ "name": o.config.display_name })),
    }))
}

/// Why a single sign-on failed. The login page shows a translated message for the code, never
/// text from the URL (anyone could put text into a link); details go to the log only.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SsoFailure {
    NotConfigured,
    /// The first administrator has not been created yet.
    Setup,
    Unreachable,
    Denied,
    Expired,
    NotLinked,
    NotAllowed,
    Disabled,
    Failed,
}

impl SsoFailure {
    fn code(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Setup => "setup",
            Self::Unreachable => "unreachable",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::NotLinked => "not_linked",
            Self::NotAllowed => "not_allowed",
            Self::Disabled => "disabled",
            Self::Failed => "failed",
        }
    }

    fn redirect(self) -> axum::response::Redirect {
        axum::response::Redirect::to(&format!("/login?error={}", self.code()))
    }
}

async fn oidc_login(
    State(state): State<AppState>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
) -> axum::response::Response {
    let Some(oidc) = state.oidc.clone() else {
        return SsoFailure::NotConfigured.redirect().into_response();
    };
    // Each start may fetch the provider's metadata: limited per address.
    if let Some(ip) = client_ip(&headers, connect.map(|c| c.0 .0), &state.trusted_proxies) {
        if let Err(e) = state.actions.check_address(ip) {
            return e.into_response();
        }
    }
    match oidc.begin().await {
        Ok((url, pending)) => {
            // The login in progress, bound to this browser (checked in the callback).
            let cookie = Cookie::build((OIDC_STATE_COOKIE, pending))
                .path(OIDC_PATH)
                .http_only(true)
                .same_site(SameSite::Lax)
                .secure(state.secure_cookies)
                .max_age(time::Duration::seconds(
                    crate::oidc::PENDING_LIFETIME.as_secs() as i64
                ))
                .build();
            (jar.add(cookie), axum::response::Redirect::to(&url)).into_response()
        }
        Err(e) => {
            tracing::warn!(error = ?format!("{e:#}"), "single sign-on failed to start");
            SsoFailure::Unreachable.redirect().into_response()
        }
    }
}

#[derive(Deserialize)]
struct Callback {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

async fn oidc_callback(
    State(state): State<AppState>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    axum::extract::Query(query): axum::extract::Query<Callback>,
) -> axum::response::Response {
    // Each callback makes a request to the provider: limited per address (a browser lands
    // on the login page, not on an error document).
    if let Some(ip) = client_ip(&headers, connect.map(|c| c.0 .0), &state.trusted_proxies) {
        if state.actions.check_address(ip).is_err() {
            return SsoFailure::Failed.redirect().into_response();
        }
    }
    let cookie = jar.get(OIDC_STATE_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(Cookie::build(OIDC_STATE_COOKIE).path(OIDC_PATH));
    match sso_user(&state, query, cookie.as_deref()).await {
        // The account was found by this subject: no session if it was unlinked meanwhile.
        Ok((user, subject)) => {
            match session_cookie(&state, &user, Some(&james_db::users::Proof::Oidc(subject))).await {
                Ok(cookie) => (jar.add(cookie), axum::response::Redirect::to("/")).into_response(),
                Err(e) => (jar, e).into_response(),
            }
        }
        Err((failure, e)) => {
            // Debug formatting escapes line breaks: the text partly comes from the URL.
            let detail: String = format!("{e:#}").chars().take(500).collect();
            tracing::info!(reason = failure.code(), error = ?detail, "single sign-on failed");
            (jar, failure.redirect()).into_response()
        }
    }
}

/// The account a provider's answer signs in, with the subject it was found by.
async fn sso_user(
    state: &AppState,
    query: Callback,
    cookie: Option<&str>,
) -> Result<(User, String), (SsoFailure, anyhow::Error)> {
    let fail = |kind: SsoFailure| move |e: anyhow::Error| (kind, e);
    let oidc = state.oidc.clone().ok_or_else(|| {
        (
            SsoFailure::NotConfigured,
            anyhow::anyhow!("single sign-on is not configured"),
        )
    })?;
    // Before setup the first account must be the administrator (created with the setup token).
    if state.setup_token.lock().is_some() {
        return Err((SsoFailure::Setup, anyhow::anyhow!("setup is not complete")));
    }
    if let Some(error) = query.error {
        return Err((
            SsoFailure::Denied,
            anyhow::anyhow!("the identity provider reported: {error}"),
        ));
    }
    let (Some(code), Some(csrf)) = (query.code, query.state) else {
        return Err((
            SsoFailure::Failed,
            anyhow::anyhow!("incomplete answer of the identity provider"),
        ));
    };
    let identity = oidc.finish(&code, &csrf, cookie).await.map_err(|e| {
        match e.downcast_ref::<crate::oidc::Expired>() {
            Some(_) => (SsoFailure::Expired, e),
            None => (SsoFailure::Failed, e),
        }
    })?;
    if !oidc.is_allowed(&identity) {
        return Err((
            SsoFailure::NotAllowed,
            anyhow::anyhow!("{} is in none of the allowed groups", identity.subject),
        ));
    }
    let user = oidc_account(state, &oidc, &identity)
        .await
        .map_err(fail(SsoFailure::Failed))?
        .ok_or_else(|| {
            (
                SsoFailure::NotLinked,
                anyhow::anyhow!("no account is linked to {}", identity.subject),
            )
        })?;
    if user.disabled {
        return Err((
            SsoFailure::Disabled,
            anyhow::anyhow!("account {} is disabled", user.username),
        ));
    }
    Ok((user, identity.subject))
}

/// The account of an SSO identity: linked by subject, created on first login if allowed
/// (`None`: no account and none may be created). Group-managed administrator rights are
/// updated.
async fn oidc_account(
    state: &AppState,
    oidc: &crate::oidc::Oidc,
    identity: &crate::oidc::OidcIdentity,
) -> anyhow::Result<Option<User>> {
    let user = match state.db.user_by_oidc(&identity.subject).await {
        Ok(user) => user,
        Err(james_db::DbError::NotFound) => {
            if !oidc.config.create_users {
                return Ok(None);
            }
            match create_sso_account(state, oidc, identity).await? {
                Some(user) => user,
                None => return Ok(None),
            }
        }
        Err(e) => return Err(e.into()),
    };
    // Disabled accounts are refused by the caller; their rights stay as they are.
    let Some(admin) = oidc.is_admin(identity).filter(|_| !user.disabled) else {
        return Ok(Some(user));
    };
    if admin == user.is_admin {
        return Ok(Some(user));
    }
    if admin {
        state.db.set_admin(user.id, true).await?;
    } else {
        if !state.db.demote_admin(user.id).await? && state.db.user(user.id).await?.is_admin {
            // Never lock everybody out: the last administrator stays one.
            tracing::warn!(user = %user.username, "not removing administrator rights of the last administrator");
            return Ok(Some(user));
        }
        // Sessions and tokens created with administrator rights end.
        state.db.revoke_user_tokens(user.id, None).await?;
        state.plugin_tokens.revoke_user(user.id);
    }
    tracing::info!(user = %user.username, admin, "administrator rights changed by single sign-on");
    Ok(Some(state.db.user(user.id).await?))
}

/// Creates the account of a new SSO identity (a free name derived from the provider's claim).
async fn create_sso_account(
    state: &AppState,
    oidc: &crate::oidc::Oidc,
    identity: &crate::oidc::OidcIdentity,
) -> anyhow::Result<Option<User>> {
    let base = crate::oidc::username_from(identity.username.as_deref());
    for n in 0..100 {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}{}", n + 1)
        };
        match state
            .db
            .create_oidc_user(&name, &identity.subject, &oidc.config.default_roles)
            .await
        {
            Ok(user) => {
                tracing::info!(user = %user.username, "account created by single sign-on");
                return Ok(Some(user));
            }
            Err(james_db::DbError::Conflict) => {
                // A concurrent login of the same identity may have created it.
                match state.db.user_by_oidc(&identity.subject).await {
                    Ok(user) => return Ok(Some(user)),
                    Err(james_db::DbError::NotFound) => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("no free user name for this login")
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
    // An SSO account without a password would gain a login that outlives its removal at the
    // identity provider: only an administrator can give it one.
    if !auth.user.has_password && auth.user.oidc_subject.is_some() {
        return Err(ApiError::forbidden());
    }
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

/// Longest API token lifetime of accounts without a password (single sign-on only).
const SSO_TOKEN_DAYS: i64 = 30;

async fn create_token(
    State(state): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    Json(body): Json<NewToken>,
) -> ApiResult<Json<serde_json::Value>> {
    // Accounts that only sign in through SSO lose access when the identity provider removes
    // them; their tokens must run out as well, and an API token cannot renew itself (only a
    // login session, which needs the provider, creates tokens).
    let sso_only = !auth.user.has_password && auth.user.oidc_subject.is_some();
    if sso_only {
        let token = crate::auth::token_from_headers(&headers).unwrap_or_default();
        if !state.db.is_session(&token).await? {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "single sign-on accounts create tokens in the web interface",
            ));
        }
    }
    let lifetime = match body.days {
        None if sso_only => {
            return Err(ApiError::bad_request(format!(
                "single sign-on accounts need a token lifetime (at most {SSO_TOKEN_DAYS} days)"
            )))
        }
        None => None,
        Some(days) if sso_only && days > SSO_TOKEN_DAYS => {
            return Err(ApiError::bad_request(format!(
                "single sign-on accounts can create tokens for at most {SSO_TOKEN_DAYS} days"
            )))
        }
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
