//! Built-in media sources.
//!
//! Default priority order (lower is asked first): [`local`] (0), [`stream`] (10),
//! [`ytdlp`] (100, catch-all for other URLs and free-text search).

pub mod egress;
pub mod local;
pub mod net;
pub mod probe;
pub mod stream;
pub mod ytdlp;

use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::process::Command;
use tokio::sync::Semaphore;

static TOOL_SLOTS: OnceLock<Semaphore> = OnceLock::new();
const DEFAULT_TOOL_SLOTS: usize = 8;

/// Limits how many external tools (yt-dlp, ffprobe) run at once across all instances.
/// Must be called before the first tool runs; later calls are ignored.
pub fn set_max_concurrent_tools(slots: usize) {
    let _ = TOOL_SLOTS.set(Semaphore::new(slots.max(1)));
}

fn tool_slots() -> &'static Semaphore {
    TOOL_SLOTS.get_or_init(|| Semaphore::new(DEFAULT_TOOL_SLOTS))
}

/// Runs a command and returns stdout, failing with stderr on a non-zero exit.
pub(crate) async fn run(mut cmd: Command, timeout: Duration) -> anyhow::Result<Vec<u8>> {
    let _slot = tool_slots().acquire().await.context("tool limiter closed")?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let program = format!("{:?}", cmd.as_std().get_program());
    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .with_context(|| format!("{program} timed out"))?
        .with_context(|| format!("failed to run {program}"))?;
    if !output.status.success() {
        bail!("{}", error_message(&output.stderr));
    }
    Ok(output.stdout)
}

/// The error lines of a tool's stderr (its warnings left out).
fn error_message(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let message = stderr
        .lines()
        .filter(|l| l.starts_with("ERROR") || !l.starts_with("WARNING"))
        .collect::<Vec<_>>()
        .join("\n");
    if message.trim().is_empty() {
        stderr.trim().to_owned()
    } else {
        message.trim().to_owned()
    }
}

/// Kills a process group (the tool and everything it started, e.g. ffmpeg) when dropped.
struct GroupGuard(Option<u32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall; the group id is the tool's pid (process_group(0)).
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Total size of the files directly in `dir`.
fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Runs a download writing into `dir` and returns its stdout. Unlike [`run`] it takes no
/// shared tool slot (downloads are limited by their caller) and watches the size on disk:
/// formats of unknown size are stopped at `max_bytes`. The tool and its children are killed on
/// timeout, when too large, and when the future is dropped.
pub(crate) async fn run_download(
    mut cmd: Command,
    timeout: Duration,
    dir: &std::path::Path,
    max_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let program = format!("{:?}", cmd.as_std().get_program());
    let mut child = cmd.spawn().with_context(|| format!("failed to run {program}"))?;
    let mut group = GroupGuard(child.id());
    let read = |pipe: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>| {
        tokio::spawn(async move {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut out).await;
            }
            out
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut check = tokio::time::interval(Duration::from_secs(2));
    let status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            _ = tokio::time::sleep_until(deadline) => bail!("{program} timed out"),
            _ = check.tick() => {
                let dir = dir.to_path_buf();
                // Some room for the container and the extracted copy next to the download.
                if tokio::task::spawn_blocking(move || dir_size(&dir)).await? > max_bytes.saturating_mul(2) {
                    bail!("the file is too large");
                }
            }
        }
    };
    // Children (ffmpeg) may still hold the pipes: read them within the deadline. The group
    // stays armed meanwhile (its id cannot be reused while a member lives).
    let (out, err) = tokio::time::timeout_at(deadline, async {
        (stdout.await.unwrap_or_default(), stderr.await.unwrap_or_default())
    })
    .await
    .with_context(|| format!("{program} timed out"))?;
    group.0 = None;
    if !status.success() {
        bail!("{}", error_message(&err));
    }
    Ok(out)
}
