//! WebSocket event stream: `GET /api/v1/ws` (session cookie or bearer header; tokens are
//! never accepted in the URL, where they would end up in proxy logs).
//!
//! Every message is a JSON [`EventEnvelope`](james_runtime::EventEnvelope);
//! only events of instances the user may view are sent.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tokio::sync::broadcast::error::RecvError;

use crate::auth::{authenticate, token_from_headers, Auth};
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/ws", get(upgrade))
}

async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let token = token_from_headers(&headers).ok_or_else(ApiError::unauthorized)?;
    let auth = authenticate(&state, &token).await?;
    Ok(ws.on_upgrade(move |socket| stream(socket, state, auth, token)))
}

/// Sends a text frame; a client that does not read for 10 s is disconnected.
async fn send_text(socket: &mut WebSocket, text: String) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            socket.send(Message::Text(text.into()))
        )
        .await,
        Ok(Ok(()))
    )
}

async fn stream(mut socket: WebSocket, state: AppState, mut auth: Auth, token: String) {
    let mut events = state.manager.subscribe();
    let mut revalidate = tokio::time::interval(state.ws_revalidate);
    revalidate.tick().await;
    loop {
        tokio::select! {
            _ = revalidate.tick() => match authenticate(&state, &token).await {
                // Logout, token revocation, disabling or role changes take effect here.
                Ok(fresh) => auth = fresh,
                Err(_) => {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            },
            event = events.recv() => match event {
                Ok(envelope) => {
                    if !auth.can("instance.view", Some(&envelope.instance))
                        || (envelope.sensitive && !auth.can("server.chat.read", Some(&envelope.instance)))
                    {
                        continue;
                    }
                    let Ok(text) = serde_json::to_string(&envelope) else { continue };
                    if !send_text(&mut socket, text).await {
                        break;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    // Tell the client to refetch state instead of silently dropping events.
                    let text = format!(r#"{{"type":"lagged","skipped":{skipped}}}"#);
                    if !send_text(&mut socket, text).await {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}
