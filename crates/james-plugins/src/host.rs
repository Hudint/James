//! Node.js processes of the plugin system.
//!
//! - **Discovery** runs in a short-lived process that may only read the plugin directory; its
//!   connection cannot call the plugin API.
//! - **Every plugin** runs in its own process. James binds the plugin id to that process'
//!   connection, so a plugin can never act as another one. Node's permission model limits
//!   file access to the plugin's own directory and data directory (unless it holds `fs`) and
//!   child processes and native addons to plugins holding `process`.
//!
//! Each process gets one end of a Unix socket pair as file descriptor 3 (no socket file on
//! disk). Output goes to the log, one line at a time with a length limit.

use std::ffi::OsString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::ipc::{self, Handler, Peer, PROTOCOL_VERSION};
use crate::manager::{DiscoveryError, HostHandler, PluginManager, PluginSettings};
use crate::manifest::Manifest;

/// Time a process gets for its initial requests (loading runs the plugin's top-level code).
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest line of process output that is logged.
const MAX_LINE: usize = 4000;

#[derive(Deserialize)]
pub(crate) struct Discovery {
    pub plugins: Vec<Manifest>,
    #[serde(default)]
    pub errors: Vec<DiscoveryError>,
}

/// The directory of the host package (`plugin-host/`), readable by every process.
fn host_dir(settings: &PluginSettings) -> PathBuf {
    let script = &settings.host_script;
    script
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| script.parent().unwrap_or(Path::new("/")))
        .to_path_buf()
}

/// Refuses host locations that would let every plugin read too much (`/`, a directory
/// containing the data directory).
pub(crate) fn check_host_dir(settings: &PluginSettings) -> anyhow::Result<()> {
    let dir = host_dir(settings);
    anyhow::ensure!(
        // Every plugin may read the host directory: never `/` or anything holding the data.
        dir.parent().is_some() && !settings.data_dir.starts_with(&dir),
        "the plugin host must live in its own directory (plugins.host = .../plugin-host/src/main.ts), not {}",
        dir.display()
    );
    Ok(())
}

/// `--allow-fs-read=<path>` / `--allow-fs-write=<path>` flags, one per path (Node 24 does not
/// accept lists).
fn allow(flag: &str, paths: &[&Path]) -> Vec<OsString> {
    paths
        .iter()
        .map(|path| {
            let mut arg = OsString::from(flag);
            arg.push("=");
            arg.push(path.as_os_str());
            arg
        })
        .collect()
}

/// Runs the discovery process and returns what it found.
pub(crate) async fn discover(settings: &PluginSettings) -> anyhow::Result<Discovery> {
    check_host_dir(settings)?;
    let host = host_dir(settings);
    // Discovery runs no plugin code itself: single files are read by probe processes of their
    // own (see plugin-host/src/probe.ts), which it starts.
    let mut args = vec![
        OsString::from("--permission"),
        OsString::from("--allow-child-process"),
    ];
    args.extend(allow("--allow-fs-read", &[&host, &settings.plugins_dir]));
    let (mut child, stream) = spawn(settings, args, "discovery", &settings.plugins_dir)?;
    let (peer, connection) = ipc::connect(stream, Arc::new(Refuse), 4);
    let connection = tokio::spawn(connection);
    let result = async {
        init(&peer).await?;
        let params = json!({ "pluginsDir": settings.plugins_dir });
        let value = peer.request("discover", params, START_TIMEOUT).await?;
        Ok::<_, anyhow::Error>(serde_json::from_value::<Discovery>(value)?)
    };
    let result = tokio::select! {
        result = result => result,
        status = child.wait() => Err(anyhow::anyhow!("the discovery process exited ({})", status?)),
    };
    connection.abort();
    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}

/// Runs one plugin process until it exits. `loaded` is called once the plugin is loaded
/// (with the connection to it); the returned text describes why the process ended.
pub(crate) async fn run_plugin(
    manager: &Arc<PluginManager>,
    manifest: &Manifest,
    permissions: &[String],
    generation: u64,
    loaded: impl FnOnce(Peer),
) -> anyhow::Result<String> {
    let settings = &manager.settings;
    let data_dir = settings.data_dir.join(&manifest.id);
    std::fs::create_dir_all(&data_dir).with_context(|| format!("cannot create {}", data_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700));
    }
    let plugin_paths = manifest
        .paths
        .as_ref()
        .context("the plugin host did not report the plugin's files")?;
    let host = host_dir(settings);
    let mut args = vec![OsString::from("--permission"), OsString::from("--allow-worker")];
    if permissions.iter().any(|p| p == "fs") {
        args.push(OsString::from("--allow-fs-read=*"));
        args.push(OsString::from("--allow-fs-write=*"));
    } else {
        let mut readable: Vec<&Path> = vec![&host, &data_dir];
        readable.extend(plugin_paths.readable.iter().map(PathBuf::as_path));
        args.extend(allow("--allow-fs-read", &readable));
        args.extend(allow("--allow-fs-write", &[&data_dir]));
    }
    if permissions.iter().any(|p| p == "process") {
        args.push(OsString::from("--allow-child-process"));
        args.push(OsString::from("--allow-addons"));
    }

    let (mut child, stream) = spawn(settings, args, &manifest.id, &data_dir)?;
    let handler = Arc::new(HostHandler::new(manager.clone(), manifest.id.clone(), generation));
    // At most 16 calls of a plugin are processed at once (backpressure on its socket).
    let (peer, connection) = ipc::connect(stream, handler, 16);
    let mut connection = tokio::spawn(connection);

    let start = async {
        init(&peer).await?;
        peer.request(
            "plugin.load",
            json!({
                "id": manifest.id,
                "permissions": permissions,
                "manifest": manifest,
                "dir": plugin_paths.dir,
                "entry": plugin_paths.entry,
                "dataDir": data_dir,
            }),
            START_TIMEOUT,
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {
        result = start => result?,
        status = child.wait() => anyhow::bail!("the plugin process exited while loading ({})", status?),
    }
    loaded(peer);
    let reason = tokio::select! {
        status = child.wait() => format!("the plugin process exited ({})", status?),
        _ = &mut connection => "the plugin process closed the connection".to_string(),
    };
    connection.abort();
    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(reason)
}

async fn init(peer: &Peer) -> anyhow::Result<()> {
    let init = peer
        .request(
            "init",
            json!({ "protocol": PROTOCOL_VERSION, "version": env!("CARGO_PKG_VERSION") }),
            START_TIMEOUT,
        )
        .await?;
    let protocol = init.get("protocol").and_then(Value::as_u64);
    anyhow::ensure!(
        protocol == Some(u64::from(PROTOCOL_VERSION)),
        "the plugin host speaks protocol {protocol:?}, expected {PROTOCOL_VERSION}"
    );
    Ok(())
}

/// Connection handler of the discovery process: it may not call anything.
struct Refuse;

#[async_trait]
impl Handler for Refuse {
    async fn request(&self, method: &str, _: Value) -> Result<Value, String> {
        Err(format!("'{method}' is not available during discovery"))
    }
    async fn event(&self, name: &str, data: Value) {
        if name == "log" {
            tracing::info!(target: "plugin", plugin = "discovery", "{}", crate::manager::log_text(&data));
        }
    }
}

/// Starts a host process in `cwd`. Node's permission model lets worker threads read their
/// working directory tree, so it must be a directory the process may read anyway (never James'
/// own working directory with the database).
fn spawn(
    settings: &PluginSettings,
    args: Vec<OsString>,
    label: &str,
    cwd: &Path,
) -> anyhow::Result<(Child, tokio::net::UnixStream)> {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().context("cannot create socket pair")?;
    let socket: OwnedFd = theirs.into();
    let mut cmd = Command::new(&settings.node);
    cmd.arg("--disable-warning=ExperimentalWarning")
        // The permission model warns about --allow-worker on every start.
        .arg("--disable-warning=SecurityWarning")
        // fetch() and the global HTTP agent honour HTTP(S)_PROXY (the egress policy).
        .arg("--use-env-proxy")
        .args(args)
        .arg(&settings.host_script)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NODE_ENV", "production")
        .env("JAMES_IPC_FD", "3")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    james_core::process::apply_proxy_env(&mut cmd, settings.http_proxy.as_deref());
    let fd = socket.as_raw_fd();
    // SAFETY: only async-signal-safe calls (dup2) between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot start Node.js ({})", settings.node))?;
    // The child holds its own copy now.
    drop(socket);
    forward_output(child.stdout.take(), label.to_owned(), false);
    forward_output(child.stderr.take(), label.to_owned(), true);
    ours.set_nonblocking(true)?;
    Ok((child, tokio::net::UnixStream::from_std(ours)?))
}

fn forward_output(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    label: String,
    stderr: bool,
) {
    let Some(stream) = stream else { return };
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            line.clear();
            // Bounded: a line without newline cannot grow without limit.
            let read = (&mut reader)
                .take(MAX_LINE as u64)
                .read_until(b'\n', &mut line)
                .await;
            match read {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = crate::manager::sanitize(&String::from_utf8_lossy(&line));
            if stderr {
                tracing::warn!(target: "plugin-host", plugin = %label, "{text}");
            } else {
                tracing::info!(target: "plugin-host", plugin = %label, "{text}");
            }
        }
    });
}
