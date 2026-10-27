//! API tests: authentication, authorization, privilege-escalation guards, WebSocket, uploads.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use futures::StreamExt;
use http_body_util::BodyExt;
use james_api::{AppState, LibraryService, LinkCodes};
use james_core::source::SourceRegistry;
use james_db::permissions::Grant;
use james_db::Db;
use james_runtime::{
    AudioEngineFactory, EventEnvelope, InstanceEvent, InstanceManager, MemoryStore, Services,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;

struct FakeLibrary {
    dir: PathBuf,
    indexed: Mutex<Vec<PathBuf>>,
}

#[async_trait::async_trait]
impl LibraryService for FakeLibrary {
    async fn scan(&self) -> anyhow::Result<(usize, usize)> {
        Ok((0, 0))
    }
    async fn index(&self, files: Vec<PathBuf>) -> anyhow::Result<()> {
        self.indexed.lock().extend(files);
        Ok(())
    }
    fn upload_dir(&self) -> PathBuf {
        self.dir.clone()
    }
}

struct Api {
    app: axum::Router,
    db: Db,
    manager: Arc<InstanceManager>,
    library: Arc<FakeLibrary>,
    _dir: tempfile::TempDir,
}

struct Response {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}

impl Api {
    async fn new() -> Self {
        let db = Db::memory().await.unwrap();
        let services = Services::new(
            SourceRegistry::default(),
            vec![],
            Arc::new(AudioEngineFactory::new(tokio::runtime::Handle::current())),
            Arc::new(MemoryStore::default()),
            "ffmpeg",
        );
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(FakeLibrary {
            dir: dir.path().to_path_buf(),
            indexed: Mutex::new(Vec::new()),
        });
        let manager = Arc::new(InstanceManager::new(services));
        let state = AppState {
            setup_token: Arc::new(Mutex::new(Some("SETUPTOKEN".into()))),
            db: db.clone(),
            manager: manager.clone(),
            links: LinkCodes::default(),
            logins: Default::default(),
            actions: Default::default(),
            trusted_proxies: Default::default(),
            allowed_hosts: Default::default(),
            library: library.clone(),
            secure_cookies: false,
            ws_revalidate: Duration::from_millis(200),
        };
        Self {
            app: james_api::router(state),
            db,
            manager,
            library,
            _dir: dir,
        }
    }

    async fn send(&self, request: Request<Body>) -> Response {
        self.send_from(request, "127.0.0.1:40000").await
    }

    async fn send_from(&self, mut request: Request<Body>, peer: &str) -> Response {
        let peer: std::net::SocketAddr = peer.parse().unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(peer));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_owned);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Response {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
        }
    }

    /// `auth` is a session cookie (`james_session=…`) or an API token (`jms_…`).
    async fn call(&self, method: Method, path: &str, auth: Option<&str>, body: Option<Value>) -> Response {
        let mut request = Request::builder().method(method).uri(format!("/api/v1{path}"));
        match auth {
            Some(a) if a.starts_with("james_session=") => request = request.header(header::COOKIE, a),
            Some(a) => request = request.header(header::AUTHORIZATION, format!("Bearer {a}")),
            None => {}
        }
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        }
        .unwrap();
        self.send(request).await
    }

    fn login_request(username: &str, password: &str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"username": username, "password": password}).to_string(),
            ))
            .unwrap()
    }

    async fn login(&self, username: &str, password: &str) -> String {
        let r = self
            .call(
                Method::POST,
                "/auth/login",
                None,
                Some(json!({"username": username, "password": password})),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert!(
            r.body.get("token").is_none(),
            "the session token is only sent as HttpOnly cookie"
        );
        r.cookie.expect("session cookie")
    }

    async fn user_with_grants(&self, name: &str, grants: &[Grant]) -> (i64, String) {
        let user = self
            .db
            .create_user(name, Some(&format!("{name}-password")), false)
            .await
            .unwrap();
        let role = self.db.create_role(&format!("{name}-role"), "").await.unwrap();
        self.db.set_role_grants(&role.name, grants).await.unwrap();
        self.db.set_user_roles(user.id, &[role.name]).await.unwrap();
        (user.id, self.login(name, &format!("{name}-password")).await)
    }
}

#[tokio::test]
async fn setup_is_only_possible_once() {
    let api = Api::new().await;
    assert_eq!(
        api.call(Method::GET, "/setup", None, None).await.body["required"],
        true
    );
    let setup = |token: &str, name: &str| json!({"username": name, "password": "secret-password", "setupToken": token});
    let r = api
        .call(Method::POST, "/setup", None, Some(setup("wrong", "admin")))
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "the setup token from the log is required"
    );
    let r = api
        .call(Method::POST, "/setup", None, Some(setup("SETUPTOKEN", "admin")))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["user"]["isAdmin"], true);
    assert!(r.cookie.is_some());
    let r = api
        .call(Method::POST, "/setup", None, Some(setup("SETUPTOKEN", "evil")))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn cross_origin_requests_are_refused() {
    let api = Api::new().await;
    api.db
        .create_user("carol", Some("carol-password"), false)
        .await
        .unwrap();
    let session = api.login("carol", "carol-password").await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/me/identities/code")
        .header(header::COOKIE, &session)
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .unwrap();
    assert_eq!(api.send(request).await.status, StatusCode::FORBIDDEN);
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/me/identities/code")
        .header(header::COOKIE, &session)
        .header(header::HOST, "bot.example")
        .header(header::ORIGIN, "https://bot.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        api.send(request).await.status,
        StatusCode::OK,
        "same origin is fine"
    );
}

#[tokio::test]
async fn authentication_is_required() {
    let api = Api::new().await;
    api.db
        .create_user("alice", Some("alice-password"), false)
        .await
        .unwrap();
    assert_eq!(
        api.call(Method::GET, "/me", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api.call(Method::GET, "/me", Some("jms_bogus"), None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = api
        .call(
            Method::POST,
            "/auth/login",
            None,
            Some(json!({"username": "alice", "password": "wrong-pass"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let session = api.login("alice", "alice-password").await;
    let r = api.call(Method::GET, "/me", Some(&session), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["user"]["username"], "alice");
    assert_eq!(
        api.call(Method::POST, "/auth/logout", Some(&session), None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        api.call(Method::GET, "/me", Some(&session), None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn logins_are_throttled_per_client() {
    let api = Api::new().await;
    api.db
        .create_user("victim", Some("victim-password"), false)
        .await
        .unwrap();
    for _ in 0..5 {
        let r = api
            .send_from(Api::login_request("victim", "guess-guess"), "6.6.6.6:1000")
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = api
        .send_from(Api::login_request("victim", "victim-password"), "6.6.6.6:1000")
        .await;
    assert_eq!(
        r.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the attacker is blocked even with the right password"
    );
    let r = api
        .send_from(Api::login_request("victim", "victim-password"), "1.1.1.1:1000")
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the owner is not locked out from another address"
    );
}

#[tokio::test]
async fn managers_cannot_change_their_own_password_without_the_current_one() {
    let api = Api::new().await;
    let (id, session) = api
        .user_with_grants("selfmgr", &[Grant::global("users.manage")])
        .await;
    let r = api
        .call(
            Method::PATCH,
            &format!("/users/{id}"),
            Some(&session),
            Some(json!({"password": "taken-over-pass"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(api
        .db
        .authenticate("selfmgr", "selfmgr-password")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn password_change_revokes_other_sessions() {
    let api = Api::new().await;
    api.db
        .create_user("bob", Some("bob-password"), false)
        .await
        .unwrap();
    let first = api.login("bob", "bob-password").await;
    let second = api.login("bob", "bob-password").await;
    let r = api
        .call(
            Method::POST,
            "/me/password",
            Some(&first),
            Some(json!({"currentPassword": "bob-password", "newPassword": "new-bob-password"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        api.call(Method::GET, "/me", Some(&first), None).await.status,
        StatusCode::OK
    );
    assert_eq!(
        api.call(Method::GET, "/me", Some(&second), None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn permissions_are_enforced() {
    let api = Api::new().await;
    let dj = api
        .db
        .create_user("dj", Some("dj-password"), false)
        .await
        .unwrap();
    api.db.set_user_roles(dj.id, &["dj".into()]).await.unwrap();
    let session = api.login("dj", "dj-password").await;
    assert_eq!(
        api.call(Method::GET, "/users", Some(&session), None).await.status,
        StatusCode::FORBIDDEN
    );
    let r = api
        .call(
            Method::POST,
            "/instances",
            Some(&session),
            Some(json!({"id": "x", "platform": "teamspeak"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        api.call(Method::GET, "/playlists", Some(&session), None)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn user_managers_cannot_escalate() {
    let api = Api::new().await;
    let admin = api
        .db
        .create_user("root", Some("root-password"), true)
        .await
        .unwrap();
    let (manager_id, session) = api
        .user_with_grants("manager", &[Grant::global("users.manage")])
        .await;
    // A non-admin user that is more powerful than the manager.
    let strong = api
        .db
        .create_user("strong", Some("strong-password"), false)
        .await
        .unwrap();
    api.db.set_user_roles(strong.id, &["admin".into()]).await.unwrap();

    let forbidden = [
        (
            Method::POST,
            "/users".to_string(),
            json!({"username": "x", "password": "x-password", "isAdmin": true}),
        ),
        (
            Method::PATCH,
            format!("/users/{manager_id}"),
            json!({"isAdmin": true}),
        ),
        (
            Method::PATCH,
            format!("/users/{}", admin.id),
            json!({"password": "hijacked-pass"}),
        ),
        (
            Method::PATCH,
            format!("/users/{}", strong.id),
            json!({"password": "hijacked-pass"}),
        ),
        (
            Method::PATCH,
            format!("/users/{}", strong.id),
            json!({"disabled": true}),
        ),
        (
            Method::PUT,
            "/roles/manager-role".to_string(),
            json!([{"permission": "*", "instance": null}]),
        ),
        (
            Method::POST,
            "/users".to_string(),
            json!({"username": "y", "password": "y-password", "roles": ["admin"]}),
        ),
    ];
    for (method, path, body) in forbidden {
        let r = api
            .call(method.clone(), &path, Some(&session), Some(body.clone()))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method} {path} {body}");
    }
    for id in [admin.id, strong.id] {
        let r = api
            .call(Method::DELETE, &format!("/users/{id}"), Some(&session), None)
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "delete {id}");
    }
    // The strong user was not changed by the rejected requests.
    assert!(api
        .db
        .authenticate("strong", "strong-password")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn api_tokens_work_and_can_be_revoked() {
    let api = Api::new().await;
    api.db
        .create_user("bot", Some("bot-password"), false)
        .await
        .unwrap();
    let session = api.login("bot", "bot-password").await;
    let r = api
        .call(
            Method::POST,
            "/me/tokens",
            Some(&session),
            Some(json!({"name": "ci"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let token = r.body["token"].as_str().unwrap().to_owned();
    assert_eq!(
        api.call(Method::GET, "/me", Some(&token), None).await.status,
        StatusCode::OK
    );
    let list = api
        .call(Method::GET, "/me/tokens", Some(&session), None)
        .await
        .body;
    let id = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "api")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = api
        .call(Method::DELETE, &format!("/me/tokens/{id}"), Some(&session), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        api.call(Method::GET, "/me", Some(&token), None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn json_bodies_are_limited() {
    let api = Api::new().await;
    let huge = "x".repeat(2 * 1024 * 1024);
    let r = api
        .call(
            Method::POST,
            "/auth/login",
            None,
            Some(json!({"username": "a", "password": huge})),
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

fn multipart(name: &str, content: &[u8]) -> (String, Vec<u8>) {
    let boundary = "jamesboundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[tokio::test]
async fn uploads_are_sanitised() {
    let api = Api::new().await;
    let (_, session) = api
        .user_with_grants("uploader", &[Grant::global("library.manage")])
        .await;
    let upload = |name: &str| {
        let (content_type, body) = multipart(name, b"not really audio");
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/library/upload")
            .header(header::COOKIE, &session)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap()
    };
    let r = api.send(upload("../../escape.mp3")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(
        r.body["files"],
        json!(["escape.mp3"]),
        "directory components are stripped"
    );
    assert!(api.library.dir.join("escape.mp3").is_file());
    assert_eq!(
        api.library.indexed.lock().len(),
        1,
        "only the new file is indexed"
    );
    assert!(
        !api.library.dir.join(".escape.mp3.part").exists(),
        "temporary file renamed"
    );

    assert_eq!(
        api.send(upload("escape.mp3")).await.status,
        StatusCode::CONFLICT,
        "no overwrite"
    );
    assert_eq!(
        api.send(upload("script.sh")).await.status,
        StatusCode::BAD_REQUEST,
        "audio files only"
    );
}

#[tokio::test]
async fn websocket_filters_by_instance_and_closes_on_revocation() {
    let api = Api::new().await;
    let (user_id, session) = api
        .user_with_grants(
            "viewer",
            &[Grant {
                permission: "instance.view".into(),
                instance: Some("a".into()),
            }],
        )
        .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = api.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut request = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        format!("ws://{address}/api/v1/ws"),
    )
    .unwrap();
    request
        .headers_mut()
        .insert(header::COOKIE, session.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let events = &api.manager.services().events;
    let send = |instance: &str, sensitive: bool| {
        let _ = events.send(EventEnvelope {
            instance: instance.into(),
            event: InstanceEvent::QueueChanged,
            sensitive,
        });
    };
    send("b", false); // other instance: filtered
    send("a", true); // private chat: needs server.chat.read
    send("a", false); // visible

    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(value["instance"], "a");
    assert_eq!(value["type"], "queueChanged");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), socket.next())
            .await
            .is_err(),
        "no further (filtered) events"
    );

    // Revoking all sessions closes the socket at the next re-validation.
    api.db.revoke_user_tokens(user_id, None).await.unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) => break,
                Some(Ok(m)) if m.is_close() => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "socket closed after revocation");
}

#[tokio::test]
async fn new_endpoints_check_permissions() {
    let api = Api::new().await;
    let (_, viewer) = api
        .user_with_grants("viewer2", &[Grant::global("instance.view")])
        .await;
    let (_, player) = api
        .user_with_grants("player", &[Grant::global("playback.play")])
        .await;
    // Permission is checked before the instance lookup (403 vs. 409 "not running").
    let cases = [
        (Method::GET, "/instances/x/search?q=a", None),
        (Method::POST, "/instances/x/queue", Some(json!({"tracks": []}))),
        (Method::POST, "/instances/x/queue/1/play", None),
        (Method::POST, "/instances/x/move", Some(json!({"channel": "1"}))),
    ];
    for (method, path, body) in &cases {
        let r = api.call(method.clone(), path, Some(&viewer), body.clone()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "viewer: {method} {path}");
    }
    let r = api
        .call(Method::GET, "/instances/x/search?q=a", Some(&player), None)
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "allowed, but the instance is not running"
    );
    let r = api
        .call(
            Method::POST,
            "/instances/x/move",
            Some(&player),
            Some(json!({"channel": "1"})),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "moving the bot needs server.move"
    );
}

#[tokio::test]
async fn unknown_hosts_are_refused_when_configured() {
    let api = Api::new().await;
    let services = Services::new(
        SourceRegistry::default(),
        vec![],
        Arc::new(AudioEngineFactory::new(tokio::runtime::Handle::current())),
        Arc::new(MemoryStore::default()),
        "ffmpeg",
    );
    let state = AppState {
        setup_token: Arc::new(Mutex::new(None)),
        db: api.db.clone(),
        manager: Arc::new(InstanceManager::new(services)),
        links: LinkCodes::default(),
        logins: Default::default(),
        actions: Default::default(),
        library: api.library.clone(),
        secure_cookies: false,
        trusted_proxies: Default::default(),
        allowed_hosts: Arc::new(vec!["bot.example".into()]),
        ws_revalidate: Duration::from_secs(30),
    };
    let app = james_api::app(state, None);
    let get = |host: &str| {
        Request::builder()
            .uri("/api/v1/setup")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(get("bot.example")).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(get("bot.example:8087"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(get("rebind.attacker.example"))
            .await
            .unwrap()
            .status(),
        StatusCode::MISDIRECTED_REQUEST
    );
}
