//! Built-in media sources.
//!
//! Default priority order (lower is asked first): [`local`] (0), [`stream`] (10),
//! [`ytdlp`] (100, catch-all for other URLs and free-text search).

pub mod local;
pub mod probe;
pub mod stream;
pub mod ytdlp;

use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::process::Command;

/// Runs a command and returns stdout, failing with stderr on a non-zero exit.
pub(crate) async fn run(mut cmd: Command, timeout: Duration) -> anyhow::Result<Vec<u8>> {
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .filter(|l| l.starts_with("ERROR") || !l.starts_with("WARNING"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{}",
            if message.trim().is_empty() {
                stderr.trim().to_owned()
            } else {
                message.trim().to_owned()
            }
        );
    }
    Ok(output.stdout)
}
