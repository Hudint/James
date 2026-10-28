//! Instances: configuration, lifecycle, playback, queue and server interaction.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use james_core::platform::{MessageTarget, Snapshot};
use james_runtime::{
    ConfigureError, ConfigureMode, InstanceConfig, InstanceHandle, InstanceStatus, PlayMode, QueueItem,
    RepeatMode, Requester,
};
use serde::{Deserialize, Serialize};

use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/instances", get(list).post(create))
        .route("/instances/{id}", get(detail).put(update).delete(remove))
        .route("/instances/{id}/start", post(start))
        .route("/instances/{id}/stop", post(stop))
        .route("/instances/{id}/restart", post(restart))
        .route("/instances/{id}/status", get(status))
        .route("/instances/{id}/play", post(play))
        .route("/instances/{id}/skip", post(skip))
        .route("/instances/{id}/previous", post(previous))
        .route("/instances/{id}/stop-playback", post(stop_playback))
        .route("/instances/{id}/pause", post(pause))
        .route("/instances/{id}/resume", post(resume))
        .route("/instances/{id}/volume", put(volume))
        .route("/instances/{id}/seek", post(seek))
        .route("/instances/{id}/repeat", put(repeat))
        .route("/instances/{id}/shuffle", put(shuffle))
        .route(
            "/instances/{id}/queue",
            get(queue).post(add_tracks).delete(clear_queue),
        )
        .route("/instances/{id}/queue/shuffle", post(shuffle_queue))
        .route("/instances/{id}/queue/move", post(move_item))
        .route("/instances/{id}/queue/{item}", delete(remove_item))
        .route("/instances/{id}/queue/{item}/play", post(jump))
        .route("/instances/{id}/search", get(search))
        .route("/instances/{id}/move", post(move_bot))
        .route("/instances/{id}/server", get(server))
        .route("/instances/{id}/message", post(message))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceSummary {
    id: String,
    name: String,
    platform: String,
    autostart: bool,
    running: bool,
    status: Option<InstanceStatus>,
    /// Full configuration, only for users allowed to manage the instance.
    config: Option<InstanceConfig>,
}

async fn summary(auth: &Auth, state: &AppState, config: InstanceConfig) -> InstanceSummary {
    let handle = state.manager.get(&config.id);
    let status = match &handle {
        Some(h) => tokio::time::timeout(Duration::from_secs(2), h.status())
            .await
            .ok()
            .and_then(Result::ok),
        None => None,
    };
    let manage = auth.can("instance.manage", Some(&config.id));
    InstanceSummary {
        id: config.id.clone(),
        name: config.name.clone(),
        platform: config.platform.clone(),
        autostart: config.autostart,
        running: handle.is_some(),
        status,
        config: manage.then_some(config),
    }
}

async fn list(State(state): State<AppState>, auth: Auth) -> Json<Vec<InstanceSummary>> {
    // Statuses are fetched concurrently (and time out individually) so many instances or a
    // stuck one cannot make the list slow for everybody.
    let visible = state
        .manager
        .configs()
        .into_iter()
        .filter(|c| auth.can("instance.view", Some(&c.id)));
    Json(futures::future::join_all(visible.map(|config| summary(&auth, &state, config))).await)
}

fn config_of(state: &AppState, id: &str) -> ApiResult<InstanceConfig> {
    state
        .manager
        .config(id)
        .ok_or_else(|| ApiError::not_found(format!("instance '{id}'")))
}

async fn detail(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<InstanceSummary>> {
    auth.require("instance.view", Some(&id))?;
    let config = config_of(&state, &id)?;
    Ok(Json(summary(&auth, &state, config).await))
}

fn validate_id(id: &str) -> ApiResult<()> {
    let valid = !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "instance ids may only contain a-z, 0-9, '-' and '_' (max. 32)",
        ))
    }
}

/// Maps manager configuration errors to API errors.
fn configure_error(e: anyhow::Error) -> ApiError {
    match e.downcast_ref::<ConfigureError>() {
        Some(ConfigureError::Exists) => ApiError::conflict("an instance with this id already exists"),
        Some(ConfigureError::NotFound) => ApiError::not_found("instance"),
        None => ApiError::bad_request(crate::error::user_message(&format!("{e:#}"))),
    }
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(config): Json<InstanceConfig>,
) -> ApiResult<(StatusCode, Json<InstanceSummary>)> {
    auth.require("instance.manage", None)?;
    validate_id(&config.id)?;
    let db = state.db.clone();
    state
        .manager
        .configure(config.clone(), ConfigureMode::Create, |c| async move {
            Ok(db.save_instance(&c.id, &c).await?)
        })
        .await
        .map_err(configure_error)?;
    Ok((StatusCode::CREATED, Json(summary(&auth, &state, config).await)))
}

/// Replaces the configuration; a running instance is restarted to apply it.
async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(mut config): Json<InstanceConfig>,
) -> ApiResult<Json<InstanceSummary>> {
    auth.require("instance.manage", Some(&id))?;
    config.id = id.clone();
    let db = state.db.clone();
    state
        .manager
        .configure(config.clone(), ConfigureMode::Update, |c| async move {
            Ok(db.save_instance(&c.id, &c).await?)
        })
        .await
        .map_err(configure_error)?;
    Ok(Json(summary(&auth, &state, config).await))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> ApiResult<StatusCode> {
    auth.require("instance.manage", Some(&id))?;
    let db = state.db.clone();
    let instance = id.clone();
    state
        .manager
        .delete(&id, || async move {
            db.delete_instance(&instance).await?;
            // Plugin settings and stores of the instance go with it.
            Ok(db.delete_plugin_instance_data(&instance).await?)
        })
        .await
        .map_err(configure_error)?;
    if let Some(plugins) = &state.plugins {
        plugins.reconcile().await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> ApiResult<StatusCode> {
    auth.require("instance.manage", Some(&id))?;
    config_of(&state, &id)?;
    state.manager.start(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stop(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> ApiResult<StatusCode> {
    auth.require("instance.manage", Some(&id))?;
    config_of(&state, &id)?;
    state.manager.stop(&id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn restart(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> ApiResult<StatusCode> {
    auth.require("instance.manage", Some(&id))?;
    config_of(&state, &id)?;
    state.manager.restart(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The running instance, after checking `permission` for it.
pub(super) fn running(
    state: &AppState,
    auth: &Auth,
    id: &str,
    permission: &str,
) -> ApiResult<InstanceHandle> {
    auth.require(permission, Some(id))?;
    state
        .manager
        .get(id)
        .ok_or_else(|| ApiError::conflict(format!("instance '{id}' is not running")))
}

async fn status(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<InstanceStatus>> {
    let handle = running(&state, &auth, &id, "instance.view")?;
    Ok(Json(handle.status().await?))
}

#[derive(Deserialize)]
struct PlayRequest {
    /// URL, search text, `local:` path, …
    input: String,
    #[serde(default = "default_mode")]
    mode: PlayMode,
}

fn default_mode() -> PlayMode {
    PlayMode::Now
}

fn requester(auth: &Auth) -> Option<Requester> {
    Some(Requester {
        uid: format!("user:{}", auth.user.id),
        name: auth.user.username.clone(),
    })
}

async fn play(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<PlayRequest>,
) -> ApiResult<Json<Vec<QueueItem>>> {
    let handle = running(&state, &auth, &id, "playback.play")?;
    state.actions.check(auth.user.id)?;
    Ok(Json(handle.play(&body.input, body.mode, requester(&auth)).await?))
}

macro_rules! simple_action {
    ($name:ident, $permission:literal, |$handle:ident| $body:expr) => {
        async fn $name(
            State(state): State<AppState>,
            auth: Auth,
            Path(id): Path<String>,
        ) -> ApiResult<StatusCode> {
            let $handle = running(&state, &auth, &id, $permission)?;
            $body?;
            Ok(StatusCode::NO_CONTENT)
        }
    };
}

simple_action!(skip, "playback.control", |h| h.skip());
simple_action!(previous, "playback.control", |h| h.previous());
simple_action!(stop_playback, "playback.control", |h| h.stop());
simple_action!(pause, "playback.control", |h| h.pause(true));
simple_action!(resume, "playback.control", |h| h.pause(false));
simple_action!(clear_queue, "queue.manage", |h| h.clear_queue());
simple_action!(shuffle_queue, "queue.manage", |h| h.shuffle_queue());

#[derive(Deserialize)]
struct VolumeRequest {
    volume: u8,
}

async fn volume(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<VolumeRequest>,
) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "playback.volume")?.set_volume(body.volume)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeekRequest {
    position_ms: u64,
}

async fn seek(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<SeekRequest>,
) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "playback.control")?.seek(Duration::from_millis(body.position_ms))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RepeatRequest {
    mode: RepeatMode,
}

async fn repeat(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<RepeatRequest>,
) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "playback.control")?.set_repeat(body.mode)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ShuffleRequest {
    enabled: bool,
}

async fn shuffle(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<ShuffleRequest>,
) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "queue.manage")?.set_shuffle(body.enabled)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn queue(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<QueueItem>>> {
    Ok(Json(running(&state, &auth, &id, "instance.view")?.queue().await?))
}

/// Queue items are addressed by their id, which stays valid while others change the queue.
#[derive(Deserialize)]
struct MoveRequest {
    item: u64,
    to: usize,
}

async fn move_item(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<MoveRequest>,
) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "queue.manage")?
        .move_item(body.item, body.to)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_item(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, item)): Path<(String, u64)>,
) -> ApiResult<Json<QueueItem>> {
    Ok(Json(
        running(&state, &auth, &id, "queue.manage")?.remove(item).await?,
    ))
}

/// Plays a queued item immediately.
async fn jump(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, item)): Path<(String, u64)>,
) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "playback.control")?
        .jump(item)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AddTracks {
    /// Tracks as returned by search or stored in playlists; re-validated when opened.
    tracks: Vec<james_core::source::Track>,
    #[serde(default = "append_mode")]
    mode: PlayMode,
}

fn append_mode() -> PlayMode {
    PlayMode::Append
}

/// Maximum tracks per add request (playlists are added through their own endpoint).
const MAX_ADD: usize = 100;

async fn add_tracks(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<AddTracks>,
) -> ApiResult<Json<Vec<QueueItem>>> {
    let handle = running(&state, &auth, &id, "playback.play")?;
    state.actions.check(auth.user.id)?;
    if body.tracks.len() > MAX_ADD {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_ADD} tracks per request"
        )));
    }
    let known = state.manager.services().sources.read().ids();
    for track in &body.tracks {
        track
            .check_client_limits()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if !known.contains(&track.source) {
            return Err(ApiError::bad_request(format!(
                "unknown source '{}'",
                track.source
            )));
        }
    }
    let tracks = body
        .tracks
        .into_iter()
        .map(james_core::source::Track::sanitized)
        .collect();
    Ok(Json(handle.add(tracks, body.mode, requester(&auth)).await?))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    10
}

async fn search(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<SearchQuery>,
) -> ApiResult<Json<Vec<james_core::source::Track>>> {
    let handle = running(&state, &auth, &id, "playback.play")?;
    state.actions.check(auth.user.id)?;
    Ok(Json(handle.search(&query.q, query.limit.clamp(1, 25)).await?))
}

#[derive(Deserialize)]
struct MoveBot {
    channel: String,
    password: Option<String>,
}

/// Moves the bot into another channel.
async fn move_bot(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<MoveBot>,
) -> ApiResult<StatusCode> {
    let handle = running(&state, &auth, &id, "server.move")?;
    handle
        .platform(|reply| james_core::platform::PlatformCommand::MoveSelf {
            channel: body.channel,
            password: body.password,
            reply,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn server(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Snapshot>> {
    let handle = running(&state, &auth, &id, "instance.view")?;
    // Many open server views must not flood the platform connection with snapshot requests:
    // answers are shared for a second.
    if let Some((at, snapshot)) = SNAPSHOTS.lock().get(&id) {
        if at.elapsed() < SNAPSHOT_TTL {
            return Ok(Json(snapshot.clone()));
        }
    }
    let snapshot = handle.snapshot().await?;
    SNAPSHOTS
        .lock()
        .insert(id, (std::time::Instant::now(), snapshot.clone()));
    Ok(Json(snapshot))
}

const SNAPSHOT_TTL: Duration = Duration::from_secs(1);
static SNAPSHOTS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<String, (std::time::Instant, Snapshot)>>,
> = std::sync::LazyLock::new(Default::default);

#[derive(Deserialize)]
struct MessageRequest {
    target: MessageTarget,
    client: Option<String>,
    text: String,
}

async fn message(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<MessageRequest>,
) -> ApiResult<StatusCode> {
    let handle = running(&state, &auth, &id, "server.message")?;
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("message must not be empty"));
    }
    handle.send_message(body.target, body.client, body.text).await?;
    Ok(StatusCode::NO_CONTENT)
}
