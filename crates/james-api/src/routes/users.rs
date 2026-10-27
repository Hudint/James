//! Account and role administration (`users.manage`).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use james_db::permissions::{Grant, Role};
use james_db::users::{Identity, User};
use serde::Deserialize;

use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users", get(list).post(create))
        .route("/users/{id}", get(detail).patch(update).delete(remove))
        .route("/users/{id}/identities", get(identities))
        .route("/roles", get(roles).post(create_role))
        .route("/roles/{name}", put(set_grants).delete(delete_role))
        .route("/group-roles/{platform}", get(group_roles))
        .route("/group-roles/{platform}/{group}", put(set_group_roles))
}

fn admin(auth: &Auth) -> ApiResult<()> {
    auth.require("users.manage", None)
}

/// Nobody can hand out rights they do not have themselves.
fn ensure_grants_covered(auth: &Auth, grants: &[Grant]) -> ApiResult<()> {
    if grants
        .iter()
        .all(|g| auth.can(&g.permission, g.instance.as_deref()))
    {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

/// A manager may only modify accounts that are not more powerful than themselves:
/// admins only by admins, and only users whose roles the manager could assign.
pub(crate) async fn ensure_can_manage(state: &AppState, auth: &Auth, target: &User) -> ApiResult<()> {
    if target.id == auth.user.id || auth.user.is_admin {
        return Ok(());
    }
    if target.is_admin {
        return Err(ApiError::forbidden());
    }
    ensure_roles_covered(state, auth, &target.roles).await
}

async fn ensure_roles_covered(state: &AppState, auth: &Auth, roles: &[String]) -> ApiResult<()> {
    for name in roles {
        let role = state
            .db
            .role(name)
            .await
            .map_err(|_| ApiError::bad_request(format!("unknown role '{name}'")))?;
        ensure_grants_covered(auth, &role.grants)?;
    }
    Ok(())
}

async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<User>>> {
    admin(&auth)?;
    Ok(Json(state.db.users().await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewUser {
    username: String,
    password: Option<String>,
    #[serde(default)]
    is_admin: bool,
    #[serde(default)]
    roles: Vec<String>,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<NewUser>,
) -> ApiResult<(StatusCode, Json<User>)> {
    admin(&auth)?;
    if body.is_admin && !auth.user.is_admin {
        return Err(ApiError::forbidden());
    }
    ensure_roles_covered(&state, &auth, &body.roles).await?;
    let user = state
        .db
        .create_user(&body.username, body.password.as_deref(), body.is_admin)
        .await?;
    if !body.roles.is_empty() {
        state.db.set_user_roles(user.id, &body.roles).await?;
    }
    Ok((StatusCode::CREATED, Json(state.db.user(user.id).await?)))
}

async fn detail(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<User>> {
    admin(&auth)?;
    Ok(Json(state.db.user(id).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserUpdate {
    password: Option<String>,
    is_admin: Option<bool>,
    disabled: Option<bool>,
    roles: Option<Vec<String>>,
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<UserUpdate>,
) -> ApiResult<Json<User>> {
    admin(&auth)?;
    let target = state.db.user(id).await?;
    // All checks before any change, so a rejected request changes nothing.
    ensure_can_manage(&state, &auth, &target).await?;
    if body.is_admin == Some(true) && !auth.user.is_admin {
        return Err(ApiError::forbidden());
    }
    if id == auth.user.id && (body.is_admin == Some(false) || body.disabled == Some(true)) {
        return Err(ApiError::bad_request("you cannot demote or disable yourself"));
    }
    if let Some(roles) = &body.roles {
        ensure_roles_covered(&state, &auth, roles).await?;
    }
    if id == auth.user.id && body.password.is_some() {
        // Changing the own password requires the current one (`POST /me/password`).
        return Err(ApiError::bad_request(
            "use the account page to change your own password",
        ));
    }
    if let Some(password) = &body.password {
        state.db.set_password(id, Some(password)).await?;
    }
    if let Some(is_admin) = body.is_admin {
        state.db.set_admin(id, is_admin).await?;
    }
    if let Some(disabled) = body.disabled {
        state.db.set_disabled(id, disabled).await?;
    }
    if let Some(roles) = &body.roles {
        state.db.set_user_roles(id, roles).await?;
    }
    // Credentials or rights changed: existing sessions of that user must not keep working.
    let security_relevant = body.password.is_some()
        || body.disabled == Some(true)
        || body.is_admin == Some(false)
        || body.roles.is_some();
    if security_relevant && id != auth.user.id {
        state.db.revoke_user_tokens(id, None).await?;
    }
    Ok(Json(state.db.user(id).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    admin(&auth)?;
    if id == auth.user.id {
        return Err(ApiError::bad_request("you cannot delete yourself"));
    }
    let target = state.db.user(id).await?;
    ensure_can_manage(&state, &auth, &target).await?;
    state.db.delete_user(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn identities(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Identity>>> {
    admin(&auth)?;
    Ok(Json(state.db.identities(id).await?))
}

async fn roles(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Role>>> {
    admin(&auth)?;
    Ok(Json(state.db.roles().await?))
}

#[derive(Deserialize)]
struct NewRole {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    grants: Vec<Grant>,
}

async fn create_role(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<NewRole>,
) -> ApiResult<(StatusCode, Json<Role>)> {
    admin(&auth)?;
    ensure_grants_covered(&auth, &body.grants)?;
    let role = state.db.create_role(&body.name, &body.description).await?;
    state.db.set_role_grants(&role.name, &body.grants).await?;
    Ok((StatusCode::CREATED, Json(state.db.role(&role.name).await?)))
}

async fn set_grants(
    State(state): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(grants): Json<Vec<Grant>>,
) -> ApiResult<Json<Role>> {
    admin(&auth)?;
    // Granting '*' is equivalent to admin rights.
    ensure_grants_covered(&auth, &grants)?;
    ensure_grants_covered(&auth, &state.db.role(&name).await?.grants)?;
    state.db.set_role_grants(&name, &grants).await?;
    Ok(Json(state.db.role(&name).await?))
}

async fn delete_role(
    State(state): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    admin(&auth)?;
    ensure_grants_covered(&auth, &state.db.role(&name).await?.grants)?;
    state.db.delete_role(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn group_roles(
    State(state): State<AppState>,
    auth: Auth,
    Path(platform): Path<String>,
) -> ApiResult<Json<Vec<(String, String)>>> {
    admin(&auth)?;
    Ok(Json(state.db.group_roles(&platform).await?))
}

async fn set_group_roles(
    State(state): State<AppState>,
    auth: Auth,
    Path((platform, group)): Path<(String, String)>,
    Json(roles): Json<Vec<String>>,
) -> ApiResult<StatusCode> {
    admin(&auth)?;
    ensure_roles_covered(&state, &auth, &roles).await?;
    // Removing a mapping to a stronger role is as much a privilege change as adding one.
    let current: Vec<String> = state
        .db
        .group_roles(&platform)
        .await?
        .into_iter()
        .filter(|(g, _)| *g == group)
        .map(|(_, role)| role)
        .collect();
    ensure_roles_covered(&state, &auth, &current).await?;
    state.db.set_group_roles(&platform, &group, &roles).await?;
    Ok(StatusCode::NO_CONTENT)
}
