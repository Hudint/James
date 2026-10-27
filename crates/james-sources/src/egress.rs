//! Local egress proxy enforcing the [`NetworkPolicy`] on every outbound connection.
//!
//! ffmpeg, yt-dlp and the HTTP client of the stream source all connect through this proxy
//! (HTTP `CONNECT` for TLS, absolute-form requests for plain HTTP). For each connection the
//! proxy resolves the target itself, refuses private addresses and connects to exactly the
//! address it checked. This covers redirects, HLS segment URLs, DNS rebinding and URL parser
//! differences between the tools, which a check of the initial URL alone cannot.
//!
//! The proxy listens on 127.0.0.1 and requires a random credential. Child processes receive
//! the proxy URL through their environment ([`EgressProxy::apply`]), never on the command
//! line, where other local users could read it from `/proc`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::prelude::*;
use rand::RngCore;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use james_core::process::apply_proxy_env;

use crate::net::{is_public_ip, NetworkPolicy};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time a client has to send the complete request head (local tools send it at once).
const HEADER_TIMEOUT: Duration = Duration::from_secs(2);
/// Connections allowed to sit in the unauthenticated phase at the same time.
const MAX_PENDING: usize = 64;
const MAX_HEADER_BYTES: u64 = 16 * 1024;
/// Request bodies forwarded for plain HTTP (media requests are GETs).
const MAX_BODY_BYTES: u64 = 1024 * 1024;
const MAX_CONNECTIONS: usize = 512;

/// A running egress proxy.
#[derive(Clone)]
pub struct EgressProxy {
    url: String,
}

impl EgressProxy {
    /// Proxy URL including credentials, e.g. `http://james:<secret>@127.0.0.1:41234`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Starts the proxy on a random loopback port. `exempt` lists `host:port` targets that
    /// may be private (e.g. a PO token provider sidecar).
    pub async fn start(policy: NetworkPolicy, exempt: Vec<String>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("cannot start egress proxy")?;
        let address = listener.local_addr()?;
        let mut secret = [0u8; 18];
        rand::rng().fill_bytes(&mut secret);
        let secret = BASE64_URL_SAFE_NO_PAD.encode(secret);
        let expected = format!("Basic {}", BASE64_STANDARD.encode(format!("james:{secret}")));
        let exempt = exempt.into_iter().map(|e| e.to_ascii_lowercase()).collect();
        let state = Arc::new(Shared {
            policy,
            exempt,
            expected,
            slots: tokio::sync::Semaphore::new(MAX_CONNECTIONS),
            pending: tokio::sync::Semaphore::new(MAX_PENDING),
        });
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        // e.g. out of file descriptors: back off instead of spinning.
                        tracing::warn!(error = %e, "egress proxy accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let state = state.clone();
                tokio::spawn(async move {
                    // Unauthenticated connections only take a small pending budget, so they
                    // cannot use up the slots of real (authenticated) transfers.
                    let Ok(pending) = state.pending.try_acquire() else {
                        respond(&mut stream, "503 Service Unavailable").await;
                        return;
                    };
                    if let Err(e) = handle(stream, &state, pending).await {
                        tracing::debug!(error = %format!("{e:#}"), "egress proxy request refused");
                    }
                });
            }
        });
        Ok(Self {
            url: format!("http://james:{secret}@{address}"),
        })
    }

    /// Configures a child process to use the proxy (see [`apply_proxy_env`]).
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        apply_proxy_env(cmd, Some(&self.url));
    }
}

struct Shared {
    policy: NetworkPolicy,
    exempt: Vec<String>,
    expected: String,
    slots: tokio::sync::Semaphore,
    pending: tokio::sync::Semaphore,
}

struct Request {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Reads the request head; the total size is bounded before any line is buffered.
async fn read_request(reader: &mut BufReader<TcpStream>) -> anyhow::Result<Request> {
    let mut limited = (&mut *reader).take(MAX_HEADER_BYTES);
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if limited.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            bail!("incomplete or oversized request head");
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let request_line = lines.first().ok_or_else(|| anyhow!("empty request"))?;
    let mut parts = request_line.split(' ');
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v)) => (m.to_owned(), t.to_owned(), v.to_owned()),
        _ => bail!("malformed request line"),
    };
    // Only well-formed tokens are forwarded (no stray control characters).
    if method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
        || !matches!(version.as_str(), "HTTP/1.0" | "HTTP/1.1")
    {
        bail!("malformed request line");
    }
    let headers = lines[1..]
        .iter()
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect();
    Ok(Request {
        method,
        target,
        version,
        headers,
    })
}

/// Ports media may be served from: 80, 443 and unprivileged ports (Icecast commonly uses
/// 8000/8443). Other privileged ports (SSH, SMTP, …) are never needed.
fn port_allowed(port: u16) -> bool {
    port == 80 || port == 443 || port >= 1024
}

/// Resolves `host` and returns the addresses to try, after checking them against the policy.
async fn resolve(state: &Shared, host: &str, port: u16) -> anyhow::Result<Vec<SocketAddr>> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port)).await?.collect(),
    };
    if addrs.is_empty() {
        bail!("cannot resolve {host}");
    }
    let exempt = state
        .exempt
        .contains(&format!("{}:{port}", host.to_ascii_lowercase()));
    if !state.policy.allow_private_networks && !exempt && !addrs.iter().all(|a| is_public_ip(a.ip())) {
        bail!("{host} resolves to a private address");
    }
    Ok(addrs)
}

/// Connects to the first reachable address.
async fn connect(addrs: &[SocketAddr]) -> anyhow::Result<TcpStream> {
    let mut last = anyhow!("no address");
    for address in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = e.into(),
            Err(_) => last = anyhow!("connection to {address} timed out"),
        }
    }
    Err(last)
}

async fn respond(stream: &mut TcpStream, status: &str) {
    // ffmpeg only sends credentials after a Basic challenge.
    let challenge = if status.starts_with("407") {
        "Proxy-Authenticate: Basic realm=\"james\"\r\n"
    } else {
        ""
    };
    let _ = stream
        .write_all(
            format!("HTTP/1.1 {status}\r\n{challenge}Content-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await;
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Resolves and connects, answering the client with an error status on failure.
async fn open_upstream(
    state: &Shared,
    client: &mut TcpStream,
    host: &str,
    port: u16,
) -> anyhow::Result<TcpStream> {
    if !port_allowed(port) {
        respond(client, "403 Forbidden").await;
        bail!("port {port} not allowed");
    }
    let addrs = match resolve(state, host, port).await {
        Ok(addrs) => addrs,
        Err(e) => {
            respond(client, "403 Forbidden").await;
            return Err(e);
        }
    };
    match connect(&addrs).await {
        Ok(stream) => Ok(stream),
        Err(e) => {
            respond(client, "502 Bad Gateway").await;
            Err(e)
        }
    }
}

async fn handle(
    stream: TcpStream,
    state: &Shared,
    pending: tokio::sync::SemaphorePermit<'_>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream);
    let request = tokio::time::timeout(HEADER_TIMEOUT, read_request(&mut reader)).await??;
    let authorized = request
        .header("proxy-authorization")
        .is_some_and(|v| constant_time_eq(v.as_bytes(), state.expected.as_bytes()));
    if !authorized {
        respond(reader.get_mut(), "407 Proxy Authentication Required").await;
        bail!("missing proxy credentials");
    }
    drop(pending);
    let Ok(_slot) = state.slots.try_acquire() else {
        respond(reader.get_mut(), "503 Service Unavailable").await;
        bail!("too many connections");
    };

    if request.method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = request
            .target
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("invalid CONNECT target"))?;
        let port: u16 = port.parse()?;
        let mut upstream = open_upstream(state, reader.get_mut(), host, port).await?;
        let buffered = reader.buffer().to_vec();
        let mut client = reader.into_inner();
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        if !buffered.is_empty() {
            upstream.write_all(&buffered).await?;
        }
        pump(client, upstream, true).await;
        return Ok(());
    }

    // Plain HTTP: one absolute-form request (`GET http://host/path HTTP/1.1`) per connection.
    let url = url::Url::parse(&request.target).context("absolute URL required")?;
    if url.scheme() != "http" {
        respond(reader.get_mut(), "400 Bad Request").await;
        bail!("unsupported scheme");
    }
    if request.header("transfer-encoding").is_some() {
        // Only fixed-length bodies are forwarded; chunked uploads are never needed for media.
        respond(reader.get_mut(), "411 Length Required").await;
        bail!("chunked request bodies are not supported");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("URL without host"))?
        .to_owned();
    let port = url.port_or_known_default().unwrap_or(80);
    let mut upstream = open_upstream(state, reader.get_mut(), &host, port).await?;
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    let mut head = format!("{} {} {}\r\n", request.method, path, request.version);
    for (k, v) in &request.headers {
        // Hop-by-hop headers and our credential are not forwarded; the connection is closed
        // after one response, so pipelined requests can never reach this upstream.
        let skip = [
            "proxy-authorization",
            "proxy-connection",
            "connection",
            "keep-alive",
        ];
        if skip.iter().any(|s| k.eq_ignore_ascii_case(s)) {
            continue;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    upstream.write_all(head.as_bytes()).await?;

    let body_length: u64 = request
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if body_length > MAX_BODY_BYTES {
        bail!("request body too large");
    }
    let mut body = (&mut reader).take(body_length);
    tokio::time::timeout(HEADER_TIMEOUT * 15, tokio::io::copy(&mut body, &mut upstream))
        .await
        .context("request body timed out")??;
    // Only the response flows back; further client data is ignored, but the client closing
    // still ends the connection.
    pump(reader.into_inner(), upstream, false).await;
    Ok(())
}

/// Without traffic in either direction for this long, a connection is closed. Streams send
/// data continuously, so this only ends dead connections.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Copies data between client and upstream until either side closes or fails, or nothing
/// arrives from either side for [`IDLE_TIMEOUT`]. Only waiting for data counts as idle: a
/// write blocked by a paused (but connected) client is not. With `forward_client = false`
/// (plain HTTP), client data is discarded and a client half-close is allowed; a client that
/// is really gone is noticed when writing the response fails or by the idle timeout.
async fn pump(mut client: TcpStream, mut upstream: TcpStream, forward_client: bool) {
    enum Event {
        Client(std::io::Result<usize>),
        Upstream(std::io::Result<usize>),
        Idle,
    }
    let (mut client_read, mut client_write) = client.split();
    let (mut upstream_read, mut upstream_write) = upstream.split();
    let mut from_client = vec![0u8; 16 * 1024];
    let mut from_upstream = vec![0u8; 16 * 1024];
    let mut client_open = true;
    loop {
        let event = tokio::select! {
            n = client_read.read(&mut from_client), if client_open => Event::Client(n),
            n = upstream_read.read(&mut from_upstream) => Event::Upstream(n),
            _ = tokio::time::sleep(IDLE_TIMEOUT) => Event::Idle,
        };
        match event {
            Event::Idle | Event::Upstream(Ok(0) | Err(_)) => break,
            Event::Upstream(Ok(n)) => {
                if client_write.write_all(&from_upstream[..n]).await.is_err() {
                    break;
                }
            }
            Event::Client(Ok(0) | Err(_)) => {
                if forward_client {
                    break;
                }
                client_open = false;
            }
            Event::Client(Ok(n)) => {
                if forward_client && upstream_write.write_all(&from_client[..n]).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn local_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    let _ = s
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecret")
                        .await;
                });
            }
        });
        address
    }

    fn auth_header(proxy: &EgressProxy) -> String {
        let url = url::Url::parse(proxy.url()).unwrap();
        let credentials = format!("{}:{}", url.username(), url.password().unwrap());
        format!(
            "Proxy-Authorization: Basic {}\r\n",
            BASE64_STANDARD.encode(credentials)
        )
    }

    async fn send(proxy: &EgressProxy, raw: &[u8]) -> String {
        let address = url::Url::parse(proxy.url()).unwrap();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", address.port().unwrap()))
            .await
            .unwrap();
        let _ = stream.write_all(raw).await;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn refuses_private_targets_and_unauthenticated_use() {
        let target = local_server().await;
        let proxy = EgressProxy::start(NetworkPolicy::default(), vec![])
            .await
            .unwrap();
        let get = format!(
            "GET http://{target}/ HTTP/1.1\r\nHost: {target}\r\n{}\r\n",
            auth_header(&proxy)
        );
        assert!(
            send(&proxy, get.as_bytes()).await.starts_with("HTTP/1.1 403"),
            "loopback target refused"
        );
        let connect = format!("CONNECT {target} HTTP/1.1\r\n{}\r\n", auth_header(&proxy));
        assert!(send(&proxy, connect.as_bytes()).await.starts_with("HTTP/1.1 403"));
        let unauthenticated = format!("GET http://{target}/ HTTP/1.1\r\nHost: {target}\r\n\r\n");
        assert!(send(&proxy, unauthenticated.as_bytes())
            .await
            .starts_with("HTTP/1.1 407"));
        let wrong =
            format!("GET http://{target}/ HTTP/1.1\r\nProxy-Authorization: Basic d3Jvbmc6d3Jvbmc=\r\n\r\n");
        assert!(send(&proxy, wrong.as_bytes()).await.starts_with("HTTP/1.1 407"));
    }

    #[tokio::test]
    async fn oversized_heads_are_cut_off() {
        let proxy = EgressProxy::start(NetworkPolicy::default(), vec![])
            .await
            .unwrap();
        let flood = vec![b'a'; 64 * 1024]; // no newline at all
        assert_eq!(
            send(&proxy, &flood).await,
            "",
            "connection dropped without buffering it all"
        );
    }

    #[tokio::test]
    async fn forwards_one_request_without_credentials_or_keep_alive() {
        let target = local_server().await;
        let proxy = EgressProxy::start(
            NetworkPolicy {
                allow_private_networks: true,
            },
            vec![],
        )
        .await
        .unwrap();
        let get = format!(
            "GET http://{target}/x HTTP/1.1\r\nHost: {target}\r\nConnection: keep-alive\r\n{}\r\n",
            auth_header(&proxy)
        );
        let response = send(&proxy, get.as_bytes()).await;
        assert!(
            response.starts_with("HTTP/1.1 200") && response.ends_with("secret"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn exempt_targets_are_allowed() {
        let target = local_server().await;
        let proxy = EgressProxy::start(NetworkPolicy::default(), vec![target.to_string()])
            .await
            .unwrap();
        let get = format!(
            "GET http://{target}/ HTTP/1.1\r\nHost: {target}\r\n{}\r\n",
            auth_header(&proxy)
        );
        assert!(send(&proxy, get.as_bytes()).await.starts_with("HTTP/1.1 200"));
    }

    async fn ffmpeg_through_proxy(proxy: &EgressProxy, url: &str, no_proxy: bool) -> (bool, String) {
        let mut cmd = tokio::process::Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-i", url, "-f", "null", "-"]);
        if no_proxy {
            // A hostile environment: everything exempted from proxying.
            cmd.env("no_proxy", "*").env("NO_PROXY", "*");
        }
        proxy.apply(&mut cmd);
        let output = cmd.output().await.unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    #[tokio::test]
    async fn ffmpeg_cannot_reach_private_hosts_even_with_no_proxy_set() {
        let target = local_server().await;
        let proxy = EgressProxy::start(NetworkPolicy::default(), vec![])
            .await
            .unwrap();
        for no_proxy in [false, true] {
            let (ok, stderr) =
                ffmpeg_through_proxy(&proxy, &format!("http://{target}/a.mp3"), no_proxy).await;
            assert!(!ok);
            assert!(stderr.contains("403"), "no_proxy={no_proxy}: {stderr}");
        }
    }

    /// A client giving up on a silent upstream must free the connection immediately.
    #[tokio::test]
    async fn tunnels_end_when_the_client_leaves() {
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = silent.local_addr().unwrap();
        let accepted = Arc::new(tokio::sync::Notify::new());
        let closed = Arc::new(tokio::sync::Notify::new());
        {
            let (accepted, closed) = (accepted.clone(), closed.clone());
            tokio::spawn(async move {
                let (mut s, _) = silent.accept().await.unwrap();
                accepted.notify_one();
                let mut buf = [0u8; 1024];
                // Never answers; detects the proxy closing its side.
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                }
                closed.notify_one();
            });
        }
        let proxy = EgressProxy::start(
            NetworkPolicy {
                allow_private_networks: true,
            },
            vec![],
        )
        .await
        .unwrap();
        let port = url::Url::parse(proxy.url()).unwrap().port().unwrap();
        let mut client = TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();
        let request = format!("CONNECT {target} HTTP/1.1\r\n{}\r\n", auth_header(&proxy));
        client.write_all(request.as_bytes()).await.unwrap();
        accepted.notified().await;
        drop(client);
        tokio::time::timeout(Duration::from_secs(2), closed.notified())
            .await
            .expect("upstream closed by the proxy");
    }

    #[test]
    fn port_rules() {
        assert!(port_allowed(80) && port_allowed(443) && port_allowed(8000) && port_allowed(8443));
        assert!(!port_allowed(22) && !port_allowed(25) && !port_allowed(0));
    }
}
