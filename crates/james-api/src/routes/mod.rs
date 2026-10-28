mod account;
mod instances;
mod library;
mod playlists;
pub(crate) mod plugins;
mod users;
mod ws;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::AppState;

/// JSON bodies are small; only uploads (lossless albums) may be large.
const MAX_BODY: usize = 1024 * 1024;
pub(crate) const MAX_UPLOAD: usize = 512 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .merge(account::routes())
        .merge(instances::routes())
        .merge(users::routes())
        .merge(playlists::routes())
        .merge(library::routes())
        .merge(plugins::routes())
        .merge(ws::routes());
    Router::new()
        .nest("/api/v1", api)
        .merge(plugins::web_routes())
        .layer(axum::middleware::from_fn(crate::security::same_origin))
        .layer(axum::middleware::from_fn(crate::security::headers))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                // The path only: query strings may carry tokens (`/ws?token=`).
                tracing::info_span!("http", method = %request.method(), path = %request.uri().path())
            }),
        )
        .with_state(state)
}
