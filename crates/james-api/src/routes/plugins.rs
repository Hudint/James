//! Plugins: administration, per-instance configuration, plugin HTTP endpoints and
//! WebSockets, and the plugins' web pages (`/plugins/<id>/`).
//!
//! Plugin pages are served with `Content-Security-Policy: sandbox`, so they run in an opaque
//! origin without access to the session. They authenticate with a plugin token
//! ([`crate::plugin_tokens`]) that the web interface puts into the page's URL fragment.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Component, Path as FsPath};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use james_plugins::{ConfigureError, HttpEvent, PluginManager, WebHandler};
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::auth::{authenticate, token_from_headers, Auth};
use crate::error::{ApiError, ApiResult};
use crate::plugin_tokens::{self, PluginToken};
use crate::AppState;

/// WebSocket subprotocol of plugin pages; the token is passed as a second protocol.
const WS_PROTOCOL: &str = "james-plugin";
/// Messages a web client may send per window before it is disconnected.
const WS_MESSAGES: usize = 50;
const WS_WINDOW: Duration = Duration::from_secs(10);
/// Sandbox of plugin pages: scripts and forms, but an opaque origin (no session), and nothing
/// may be loaded from or sent to other sites (no exfiltration of what users type).
const PAGE_CSP: &str = "sandbox allow-scripts allow-forms allow-modals allow-downloads; \
     default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; media-src 'self' data: blob:; font-src 'self' data:; \
     connect-src 'self'; form-action 'self'; frame-src 'none'; frame-ancestors 'none'; \
     object-src 'none'; base-uri 'none'";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/plugins", get(list))
        .route("/plugins/reload", post(reload))
        .route("/plugins/{id}", put(configure))
        .route("/plugins/{id}/permissions", put(set_permissions))
        .route("/plugins/{id}/config", get(configs))
        .route("/instances/{instance}/plugins/{id}", put(set_active))
        .route("/instances/{instance}/plugins/{id}/config", put(set_config))
        .route("/instances/{instance}/plugins/{id}/token", post(token))
        .route(
            "/instances/{instance}/plugins/{id}/events/{name}",
            post(event)
                .options(preflight)
                .layer(axum::middleware::map_response(with_cors)),
        )
        .route("/instances/{instance}/plugins/{id}/ws", get(ws))
        .route(
            "/public/instances/{instance}/plugins/{id}/events/{name}",
            post(public_event)
                .options(preflight)
                .layer(axum::middleware::map_response(with_cors)),
        )
}

/// Plugin web pages and their helper script (outside `/api/v1`).
pub fn web_routes() -> Router<AppState> {
    Router::new()
        .route("/plugins/_/james.js", get(helper_script))
        .route("/plugins/{id}/", get(page_index))
        .route("/plugins/{id}/{*path}", get(page))
}

fn plugins(state: &AppState) -> ApiResult<&Arc<PluginManager>> {
    state
        .plugins
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "plugins are disabled"))
}

fn configure_error(e: ConfigureError) -> ApiError {
    match e {
        ConfigureError::NotFound => ApiError::not_found("plugin"),
        ConfigureError::Unavailable => ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        ConfigureError::Busy => ApiError::new(StatusCode::TOO_MANY_REQUESTS, e.to_string()),
        ConfigureError::Invalid(message) => ApiError::bad_request(message),
        ConfigureError::Internal(message) => ApiError::from(anyhow::anyhow!(message)),
    }
}

// ---- administration ---------------------------------------------------------------------

async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("plugins.manage", None)?;
    let manager = plugins(&state)?;
    Ok(Json(json!({
        "host": manager.host_status(),
        "plugins": manager.list(),
        "errors": manager.discovery_errors(),
        "pluginsDir": manager.plugins_dir(),
        "permissions": james_plugins::manifest::PERMISSIONS,
    })))
}

async fn reload(State(state): State<AppState>, auth: Auth) -> ApiResult<StatusCode> {
    auth.require("plugins.manage", None)?;
    plugins(&state)?
        .reload()
        .await
        .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Configure {
    enabled: bool,
    #[serde(default)]
    permissions: Vec<String>,
    /// `null` = all instances.
    #[serde(default)]
    instances: Option<Vec<String>>,
}

async fn configure(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<Configure>,
) -> ApiResult<Json<james_plugins::PluginInfo>> {
    auth.require("plugins.manage", None)?;
    if let Some(instances) = &body.instances {
        if let Some(unknown) = instances.iter().find(|i| state.manager.config(i).is_none()) {
            return Err(ApiError::bad_request(format!("unknown instance '{unknown}'")));
        }
    }
    let info = plugins(&state)?
        .configure(&id, body.enabled, body.permissions, body.instances)
        .await
        .map_err(configure_error)?;
    Ok(Json(info))
}

#[derive(Deserialize)]
struct SetActive {
    active: bool,
    /// Replaces the granted permissions (they apply on every instance); omitted = unchanged.
    #[serde(default)]
    permissions: Option<Vec<String>>,
}

/// Activates or deactivates a plugin on one instance.
async fn set_active(
    State(state): State<AppState>,
    auth: Auth,
    Path((instance, id)): Path<(String, String)>,
    Json(body): Json<SetActive>,
) -> ApiResult<Json<james_plugins::PluginInfo>> {
    // Permissions are granted for the whole plugin: managers of all plugins only.
    auth.require("plugins.manage", None)?;
    if state.manager.config(&instance).is_none() {
        return Err(ApiError::not_found("instance"));
    }
    let all: Vec<String> = state.manager.configs().into_iter().map(|c| c.id).collect();
    let info = plugins(&state)?
        .set_active(&id, &instance, body.active, body.permissions, &all)
        .await
        .map_err(configure_error)?;
    Ok(Json(info))
}

#[derive(Deserialize)]
struct SetPermissions {
    permissions: Vec<String>,
    /// Enables a disabled plugin (where it was active before).
    #[serde(default)]
    enable: bool,
}

/// Replaces a plugin's granted permissions (they apply on every instance it is active on),
/// keeping where it runs; restarts it.
async fn set_permissions(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<SetPermissions>,
) -> ApiResult<Json<james_plugins::PluginInfo>> {
    auth.require("plugins.manage", None)?;
    let info = plugins(&state)?
        .set_permissions(&id, body.permissions, body.enable)
        .await
        .map_err(configure_error)?;
    Ok(Json(info))
}

async fn configs(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let manager = plugins(&state)?;
    manager.get(&id).ok_or_else(|| ApiError::not_found("plugin"))?;
    // Per instance: only the instances the user may manage plugins on.
    let configs: serde_json::Map<String, Value> = manager
        .plugin_instance_configs(&id)
        .await?
        .into_iter()
        .filter(|(instance, _)| auth.can("plugins.manage", Some(instance)))
        .collect();
    Ok(Json(Value::Object(configs)))
}

async fn set_config(
    State(state): State<AppState>,
    auth: Auth,
    Path((instance, id)): Path<(String, String)>,
    Json(config): Json<Value>,
) -> ApiResult<StatusCode> {
    auth.require("plugins.manage", Some(&instance))?;
    if state.manager.config(&instance).is_none() {
        return Err(ApiError::not_found("instance"));
    }
    plugins(&state)?
        .set_instance_config(&id, &instance, config)
        .await
        .map_err(configure_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- plugin tokens ----------------------------------------------------------------------

/// Issues a token for a plugin page (requested by the web interface, same origin).
async fn token(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Path((instance, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let manager = plugins(&state)?;
    if manager.web_dir(&id).is_none() || !manager.has_context(&id, &instance) {
        return Err(ApiError::not_found("plugin page"));
    }
    state.actions.check(auth.user.id)?;
    let session = token_from_headers(&headers).ok_or_else(ApiError::unauthorized)?;
    let (token, lifetime) = state.plugin_tokens.issue(auth.user.id, &session, &id, &instance);
    Ok(Json(
        json!({ "token": token, "expiresInSecs": lifetime.as_secs() }),
    ))
}

/// The caller of a plugin endpoint: a plugin token (plugin pages) or the normal session.
async fn caller(state: &AppState, headers: &HeaderMap, plugin: &str, instance: &str) -> ApiResult<Auth> {
    match plugin_token(headers) {
        Some(token) => {
            let PluginToken {
                user,
                plugin: bound_plugin,
                instance: bound_instance,
                session,
                ..
            } = state
                .plugin_tokens
                .resolve(&token)
                .ok_or_else(ApiError::unauthorized)?;
            if bound_plugin != plugin || bound_instance != instance {
                return Err(ApiError::forbidden());
            }
            // Valid only as long as the session that requested it (logout ends both).
            let auth = authenticate(state, &session).await?;
            if auth.user.id != user {
                return Err(ApiError::unauthorized());
            }
            Ok(auth)
        }
        None => {
            let token = token_from_headers(headers).ok_or_else(ApiError::unauthorized)?;
            authenticate(state, &token).await
        }
    }
}

/// A plugin token from the `Authorization` header or the WebSocket subprotocols.
pub(crate) fn plugin_token(headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.starts_with(plugin_tokens::PREFIX));
    let protocol = || {
        headers
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|v| v.to_str().ok())?
            .split(',')
            .map(str::trim)
            .find(|p| p.starts_with(plugin_tokens::PREFIX))
    };
    bearer.or_else(protocol).map(str::to_owned)
}

/// CORS for sandboxed plugin pages (origin `null`). Requests from there carry no cookies;
/// only plugin tokens authenticate them.
fn cors(response: &mut Response) {
    let h = response.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("null"),
    );
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
}

/// Every response of the plugin endpoints carries the CORS headers, errors included, so a
/// sandboxed page can read why a request failed (e.g. an expired token).
async fn with_cors(mut response: Response) -> Response {
    cors(&mut response);
    response
}

async fn preflight() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    cors(&mut response);
    let h = response.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type"),
    );
    h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    response
}

// ---- HTTP events ------------------------------------------------------------------------

fn remote(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<std::net::IpAddr> {
    crate::throttle::client_ip(headers, peer, &state.trusted_proxies)
}

/// Accepts any JSON body (or none).
fn body(bytes: &[u8]) -> ApiResult<Value> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(bytes).map_err(|_| ApiError::bad_request("the body must be JSON"))
}

fn check_name(name: &str) -> ApiResult<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::bad_request("invalid event name"));
    }
    Ok(())
}

/// Checks that the caller may use a plugin endpoint (its own permission, else `plugins.use`).
fn authorize(auth: &Auth, handler: &WebHandler, instance: &str) -> ApiResult<()> {
    auth.require(
        handler.permission.as_deref().unwrap_or("plugins.use"),
        Some(instance),
    )
}

async fn event(
    State(state): State<AppState>,
    Path((instance, id, name)): Path<(String, String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    bytes: axum::body::Bytes,
) -> ApiResult<Response> {
    check_name(&name)?;
    let auth = caller(&state, &headers, &id, &instance).await?;
    let manager = plugins(&state)?;
    let handler = manager
        .web_handler(&id, &instance, &name)
        .ok_or_else(|| ApiError::not_found("plugin endpoint"))?;
    authorize(&auth, &handler, &instance)?;
    state.actions.check(auth.user.id)?;
    let request = HttpEvent {
        name,
        public: false,
        method: "POST".into(),
        body: body(&bytes)?,
        query,
        remote_addr: remote(&state, &headers, peer.map(|p| p.0 .0))
            .map(|ip| ip.to_string())
            .unwrap_or_default(),
        user: Some(json!({ "id": auth.user.id, "username": auth.user.username })),
    };
    let result = manager
        .http_event(&id, &instance, request)
        .await
        .map_err(configure_error)?;
    Ok(Json(result).into_response())
}

async fn public_event(
    State(state): State<AppState>,
    Path((instance, id, name)): Path<(String, String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    bytes: axum::body::Bytes,
) -> ApiResult<Response> {
    check_name(&name)?;
    let remote = remote(&state, &headers, peer.map(|p| p.0 .0));
    if let Some(ip) = remote {
        state.actions.check_address(ip)?;
    }
    let manager = plugins(&state)?;
    let public = manager
        .web_handler(&id, &instance, &name)
        .is_some_and(|h| h.public);
    if !public {
        return Err(ApiError::not_found("plugin endpoint"));
    }
    let request = HttpEvent {
        name,
        public: true,
        method: "POST".into(),
        body: body(&bytes)?,
        query,
        remote_addr: remote.map(|ip| ip.to_string()).unwrap_or_default(),
        user: None,
    };
    let result = manager
        .http_event(&id, &instance, request)
        .await
        .map_err(|e| match e {
            // Plugin errors are not shown to anonymous callers.
            ConfigureError::Invalid(detail) | ConfigureError::Internal(detail) => {
                tracing::debug!(plugin = %id, %detail, "public plugin endpoint failed");
                ApiError::bad_request("the plugin could not handle the request")
            }
            other => configure_error(other),
        })?;
    Ok(Json(result).into_response())
}

// ---- WebSockets -------------------------------------------------------------------------

async fn ws(
    State(state): State<AppState>,
    Path((instance, id)): Path<(String, String)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let auth = caller(&state, &headers, &id, &instance).await?;
    auth.require("plugins.use", Some(&instance))?;
    let manager = plugins(&state)?.clone();
    if !manager.has_context(&id, &instance) {
        return Err(ApiError::not_found("plugin"));
    }
    let upgrade = if plugin_token(&headers).is_some() {
        upgrade.protocols([WS_PROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| async move {
        // Registered only once the connection is established.
        match manager.ws_open(&id, &instance, auth.user.id, &auth.user.username) {
            Ok((connection, outgoing)) => {
                let session = Session {
                    state,
                    headers,
                    manager,
                    connection,
                    plugin: id,
                    instance,
                };
                session.relay(socket, outgoing).await;
            }
            Err(e) => tracing::debug!(plugin = %id, error = %e, "plugin WebSocket refused"),
        }
    }))
}

/// One web client's plugin WebSocket.
struct Session {
    state: AppState,
    headers: HeaderMap,
    manager: Arc<PluginManager>,
    connection: u64,
    plugin: String,
    instance: String,
}

impl Session {
    async fn relay(self, mut socket: WebSocket, mut outgoing: tokio::sync::mpsc::Receiver<String>) {
        let mut revalidate = tokio::time::interval(self.state.ws_revalidate);
        revalidate.tick().await;
        let mut window = (Instant::now(), 0usize);
        loop {
            tokio::select! {
                _ = revalidate.tick() => {
                    let allowed = caller(&self.state, &self.headers, &self.plugin, &self.instance)
                        .await
                        .is_ok_and(|auth| auth.can("plugins.use", Some(&self.instance)));
                    if !allowed {
                        break;
                    }
                }
                message = outgoing.recv() => match message {
                    Some(text) => {
                        let sent = tokio::time::timeout(
                            Duration::from_secs(10),
                            socket.send(Message::Text(text.into())),
                        )
                        .await;
                        if !matches!(sent, Ok(Ok(()))) {
                            break;
                        }
                    }
                    // Closed by the plugin (or the plugin stopped).
                    None => break,
                },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) if text.len() <= 64 * 1024 => {
                        if window.0.elapsed() > WS_WINDOW {
                            window = (Instant::now(), 0);
                        }
                        window.1 += 1;
                        if window.1 > WS_MESSAGES || !self.manager.ws_message(self.connection, text.to_string()) {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => break,
                },
            }
        }
        let _ = socket.send(Message::Close(None)).await;
        self.manager.ws_close(self.connection);
    }
}

// ---- web pages --------------------------------------------------------------------------

/// Helper for plugin pages (`James.call`, `James.connect`), see docs/plugins/README.md.
async fn helper_script() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("plugin_page.js"),
    )
        .into_response()
}

async fn page_index(state: State<AppState>, path: Path<String>, request: Request) -> Response {
    serve_page(state, path.0, String::new(), request).await
}

async fn page(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    request: Request,
) -> Response {
    serve_page(State(state), id, path, request).await
}

/// Serves a file of a plugin's web page (only for running plugins granted `web`), sandboxed.
async fn serve_page(State(state): State<AppState>, id: String, path: String, request: Request) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(dir) = state.plugins.as_ref().and_then(|p| p.web_dir(&id)) else {
        return ApiError::not_found("plugin page").into_response();
    };
    if !safe_path(&dir, &path) {
        return ApiError::not_found("file").into_response();
    }
    let Ok(request) = axum::http::Request::builder()
        .uri(format!("/{path}"))
        .body(axum::body::Body::empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut response = match tower_http::services::ServeDir::new(dir).oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_CSP),
    );
    response
}

/// No hidden files and nothing outside the page directory (also through symlinks).
fn safe_path(dir: &FsPath, path: &str) -> bool {
    let relative = FsPath::new(path);
    let plain = relative.components().all(|c| match c {
        Component::Normal(part) => !part.to_string_lossy().starts_with('.'),
        _ => false,
    });
    if !plain {
        return false;
    }
    match std::fs::canonicalize(dir.join(relative)) {
        Ok(full) => full.starts_with(dir),
        // Missing files are answered by ServeDir (404).
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_paths_stay_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("index.html"), "x").unwrap();
        std::fs::write(root.join(".env"), "secret").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("passwd")).unwrap();
        assert!(safe_path(&root, ""));
        assert!(safe_path(&root, "index.html"));
        assert!(safe_path(&root, "missing.css"));
        assert!(!safe_path(&root, ".env"));
        assert!(!safe_path(&root, "a/.git/config"));
        assert!(!safe_path(&root, "../x"));
        assert!(!safe_path(&root, "passwd"), "symlink leaving the directory");
    }
}
