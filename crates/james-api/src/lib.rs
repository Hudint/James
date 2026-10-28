//! HTTP API (`/api/v1`): REST endpoints plus a WebSocket event stream.
//!
//! Every endpoint except setup/login requires a bearer token or session
//! cookie. Permissions are checked per endpoint, scoped to the instance where
//! applicable (see `docs/api.md` for the full list).

pub mod auth;
pub mod error;
pub mod linking;
pub mod oidc;
pub mod plugin_tokens;
mod routes;
pub mod security;
pub mod throttle;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use james_db::Db;
use james_runtime::InstanceManager;

pub use linking::LinkCodes;
pub use routes::router;
pub use throttle::{ActionLimiter, LoginThrottle};

/// Local library operations the API needs (implemented by the library scanner).
#[async_trait::async_trait]
pub trait LibraryService: Send + Sync {
    /// Rescans the media directories; returns (added or updated, removed).
    async fn scan(&self) -> anyhow::Result<(usize, usize)>;
    /// Indexes specific (new) files without a full rescan.
    async fn index(&self, files: Vec<PathBuf>) -> anyhow::Result<()>;
    /// Directory uploads are stored in.
    fn upload_dir(&self) -> PathBuf;
}

/// Shared state of all handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub manager: Arc<InstanceManager>,
    pub links: LinkCodes,
    pub logins: LoginThrottle,
    pub actions: ActionLimiter,
    pub library: Arc<dyn LibraryService>,
    /// Set the `Secure` flag on session cookies (when served via HTTPS).
    pub secure_cookies: bool,
    /// One-time token required for the first-run setup (printed to the log); `None` once
    /// an account exists.
    pub setup_token: Arc<parking_lot::Mutex<Option<String>>>,
    /// Host names the web interface is served under; empty = any (see `security::allowed_hosts`).
    pub allowed_hosts: Arc<Vec<String>>,
    /// Reverse proxies whose `X-Forwarded-For` header is trusted for client addresses.
    pub trusted_proxies: Arc<Vec<std::net::IpAddr>>,
    /// How often WebSocket connections re-validate their token and permissions.
    pub ws_revalidate: std::time::Duration,
    /// Plugin system (`None` when disabled).
    pub plugins: Option<Arc<james_plugins::PluginManager>>,
    /// Tokens of plugin pages.
    pub plugin_tokens: plugin_tokens::PluginTokens,
    /// Single sign-on (`None` when not configured).
    pub oidc: Option<Arc<oidc::Oidc>>,
}

/// Builds the complete application router (API + optional static web UI).
pub fn app(state: AppState, web_root: Option<PathBuf>) -> Router {
    let hosts = state.allowed_hosts.clone();
    let api = router(state);
    match web_root {
        Some(root) => {
            let index = root.join("index.html");
            api.fallback_service(
                tower_http::services::ServeDir::new(root)
                    .fallback(tower_http::services::ServeFile::new(index)),
            )
            // Layers added in `router` only cover the API routes, not the static files.
            .layer(axum::middleware::from_fn(security::headers))
        }
        None => api,
    }
    .layer(axum::middleware::from_fn_with_state(
        hosts,
        security::allowed_hosts,
    ))
}
