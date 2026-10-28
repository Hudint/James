//! Text to speech: voices and speaking on an instance.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use james_core::tts::Voice;
use serde::Deserialize;
use serde_json::{json, Value};

use james_runtime::SpeechQueueFull;

use super::instances::running;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tts/voices", get(voices))
        .route("/instances/{id}/say", post(say))
        .route("/instances/{id}/say/stop", post(stop))
}

async fn voices(State(state): State<AppState>, _auth: Auth) -> Json<Vec<Voice>> {
    Json(state.manager.services().tts.read().voices())
}

#[derive(Deserialize)]
struct Say {
    text: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

/// Accepts the text and answers with the speech id; the outcome follows as `speechStarted` /
/// `speechEnded` events.
async fn say(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<Say>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let handle = running(&state, &auth, &id, "tts.say")?;
    state.actions.check(auth.user.id)?;
    let speech = handle
        .say(&body.text, body.voice.as_deref(), body.language.as_deref())
        .await
        .map_err(|e| match e.downcast_ref::<SpeechQueueFull>() {
            Some(full) => ApiError::new(StatusCode::TOO_MANY_REQUESTS, full.to_string()),
            None => e.into(),
        })?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "id": speech }))))
}

async fn stop(State(state): State<AppState>, auth: Auth, Path(id): Path<String>) -> ApiResult<StatusCode> {
    running(&state, &auth, &id, "tts.say")?.stop_speech()?;
    Ok(StatusCode::NO_CONTENT)
}
