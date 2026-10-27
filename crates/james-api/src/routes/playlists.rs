//! Saved playlists.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use james_core::source::Track;
use james_db::playlists::Playlist;
use james_runtime::{PlayMode, QueueItem, Requester};
use serde::{Deserialize, Serialize};

use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/playlists", get(list).post(create))
        .route("/playlists/{id}", get(detail).patch(rename).delete(remove))
        .route("/playlists/{id}/tracks", get(tracks).put(set_tracks).post(append))
        .route("/playlists/{id}/play", post(play))
}

async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Playlist>>> {
    auth.require("playlist.read", None)?;
    Ok(Json(state.db.playlists().await?))
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<Named>,
) -> ApiResult<(StatusCode, Json<Playlist>)> {
    auth.require("playlist.manage", None)?;
    Ok((
        StatusCode::CREATED,
        Json(state.db.create_playlist(&body.name, Some(auth.user.id)).await?),
    ))
}

#[derive(Serialize)]
struct PlaylistDetail {
    #[serde(flatten)]
    playlist: Playlist,
    tracks: Vec<Track>,
}

async fn detail(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<PlaylistDetail>> {
    auth.require("playlist.read", None)?;
    Ok(Json(PlaylistDetail {
        playlist: state.db.playlist(id).await?,
        tracks: state.db.playlist_tracks(id).await?,
    }))
}

async fn rename(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<Named>,
) -> ApiResult<Json<Playlist>> {
    auth.require("playlist.manage", None)?;
    state.db.rename_playlist(id, &body.name).await?;
    Ok(Json(state.db.playlist(id).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    auth.require("playlist.manage", None)?;
    state.db.delete_playlist(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn tracks(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Track>>> {
    auth.require("playlist.read", None)?;
    Ok(Json(state.db.playlist_tracks(id).await?))
}

async fn set_tracks(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(tracks): Json<Vec<Track>>,
) -> ApiResult<StatusCode> {
    auth.require("playlist.manage", None)?;
    state.actions.check(auth.user.id)?;
    let known = state.manager.services().sources.read().ids();
    if let Some(track) = tracks.iter().find(|t| !known.contains(&t.source)) {
        return Err(ApiError::bad_request(format!(
            "unknown source '{}'",
            track.source
        )));
    }
    if tracks.len() > MAX_PLAYLIST {
        return Err(ApiError::bad_request(format!(
            "a playlist holds at most {MAX_PLAYLIST} tracks"
        )));
    }
    for track in &tracks {
        track
            .check_client_limits()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
    }
    let tracks: Vec<Track> = tracks.into_iter().map(Track::sanitized).collect();
    state.db.set_playlist_tracks(id, &tracks).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AppendBody {
    /// Resolve user input (URL, search, …) through an instance's sources.
    Input {
        input: String,
    },
    Tracks(Vec<Track>),
}

async fn append(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<AppendBody>,
) -> ApiResult<Json<Vec<Track>>> {
    auth.require("playlist.manage", None)?;
    state.actions.check(auth.user.id)?;
    let tracks = match body {
        AppendBody::Tracks(tracks) => {
            if tracks.len() > 500 {
                return Err(ApiError::bad_request("at most 500 tracks per request"));
            }
            for track in &tracks {
                track
                    .check_client_limits()
                    .map_err(|e| ApiError::bad_request(e.to_string()))?;
            }
            tracks.into_iter().map(Track::sanitized).collect()
        }
        AppendBody::Input { input } => {
            // Resolves outside any instance: bounded by a global slot budget.
            let _slot = RESOLVE_SLOTS
                .acquire()
                .await
                .map_err(|_| ApiError::from(anyhow::anyhow!("shutting down")))?;
            let registry = state.manager.services().sources.read().clone();
            registry.resolve(&input).await?
        }
    };
    let known = state.manager.services().sources.read().ids();
    if let Some(track) = tracks.iter().find(|t| !known.contains(&t.source)) {
        return Err(ApiError::bad_request(format!(
            "unknown source '{}'",
            track.source
        )));
    }
    state.db.append_playlist_tracks(id, &tracks, MAX_PLAYLIST).await?;
    Ok(Json(tracks))
}

#[derive(Deserialize)]
struct PlayBody {
    instance: String,
    #[serde(default = "append_mode")]
    mode: PlayMode,
}

fn append_mode() -> PlayMode {
    PlayMode::Append
}

/// Maximum tracks per playlist (bounds storage and the size of one play request).
const MAX_PLAYLIST: usize = 5000;

/// Concurrent playlist resolves across all users.
static RESOLVE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

async fn play(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<PlayBody>,
) -> ApiResult<Json<Vec<QueueItem>>> {
    auth.require("playlist.read", None)?;
    auth.require("playback.play", Some(&body.instance))?;
    let handle = state
        .manager
        .get(&body.instance)
        .ok_or_else(|| ApiError::conflict(format!("instance '{}' is not running", body.instance)))?;
    let tracks: Vec<Track> = state.db.playlist_tracks(id).await?;
    let requester = Requester {
        uid: format!("user:{}", auth.user.id),
        name: auth.user.username.clone(),
    };
    Ok(Json(handle.add(tracks, body.mode, Some(requester)).await?))
}
