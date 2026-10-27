//! Decoding via an ffmpeg child process to 48 kHz interleaved stereo f32.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use james_core::audio::{CHANNELS, FRAME_LEN, SAMPLE_RATE};
use james_core::source::MediaInput;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::{silent_frame, Frame};

/// Frames buffered ahead of playback (2 s).
const BUFFER_FRAMES: usize = 100;

pub struct Decoder {
    frames: mpsc::Receiver<Frame>,
    /// Resolves with the error output of ffmpeg once it exits.
    exit: Option<oneshot::Receiver<Result<(), String>>>,
    exit_status: Option<Result<(), String>>,
    _child: ChildGuard,
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.start_kill();
        }
    }
}

impl Decoder {
    pub fn spawn(ffmpeg: &str, input: &MediaInput, start: Duration) -> anyhow::Result<Self> {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
        match input {
            MediaInput::File(path) => {
                if !start.is_zero() {
                    cmd.arg("-ss").arg(format!("{:.3}", start.as_secs_f64()));
                }
                cmd.arg("-i").arg(path);
            }
            MediaInput::Url { url, headers } => {
                cmd.args([
                    "-reconnect",
                    "1",
                    "-reconnect_streamed",
                    "1",
                    "-reconnect_delay_max",
                    "5",
                ]);
                if !headers.is_empty() {
                    let joined: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                    cmd.arg("-headers").arg(joined);
                }
                if !start.is_zero() {
                    cmd.arg("-ss").arg(format!("{:.3}", start.as_secs_f64()));
                }
                cmd.arg("-i").arg(url);
            }
        }
        cmd.args([
            "-vn",
            "-sn",
            "-dn",
            "-af",
            "aresample=resampler=soxr:precision=28:dither_method=none",
        ])
        .args([
            "-ac",
            &CHANNELS.to_string(),
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

        let mut child = cmd.spawn().with_context(|| format!("failed to start {ffmpeg}"))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");

        let (frame_tx, frames) = mpsc::channel(BUFFER_FRAMES);
        let (exit_tx, exit) = oneshot::channel();

        tokio::spawn(async move {
            let mut reader = BufReader::with_capacity(FRAME_LEN * 4 * 4, stdout);
            let mut bytes = vec![0u8; FRAME_LEN * 4];
            loop {
                let filled = read_full(&mut reader, &mut bytes).await;
                if filled == 0 {
                    break;
                }
                let mut frame = silent_frame();
                for (sample, chunk) in frame.iter_mut().zip(bytes[..filled].chunks_exact(4)) {
                    *sample = f32::from_le_bytes(chunk.try_into().unwrap());
                }
                if frame_tx.send(frame).await.is_err() || filled < bytes.len() {
                    break;
                }
            }
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err).await;
            let _ = exit_tx.send(if err.trim().is_empty() {
                Ok(())
            } else {
                Err(err.trim().to_owned())
            });
        });

        Ok(Self {
            frames,
            exit: Some(exit),
            exit_status: None,
            _child: ChildGuard(Some(child)),
        })
    }

    /// Next frame if one is buffered. `Poll::Ready(None)` means end of input.
    pub fn try_next(&mut self) -> std::task::Poll<Option<Frame>> {
        use mpsc::error::TryRecvError;
        match self.frames.try_recv() {
            Ok(frame) => std::task::Poll::Ready(Some(frame)),
            Err(TryRecvError::Empty) => std::task::Poll::Pending,
            Err(TryRecvError::Disconnected) => std::task::Poll::Ready(None),
        }
    }

    pub fn buffered(&self) -> usize {
        self.frames.len()
    }

    pub fn is_finished(&self) -> bool {
        self.frames.is_closed() && self.frames.is_empty()
    }

    /// Error reported by ffmpeg, available once the input has ended.
    pub fn error(&mut self) -> Option<String> {
        if self.exit_status.is_none() {
            if let Some(rx) = &mut self.exit {
                if let Ok(status) = rx.try_recv() {
                    self.exit_status = Some(status);
                    self.exit = None;
                }
            }
        }
        match &self.exit_status {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        }
    }
}

async fn read_full<R: AsyncReadExt + Unpin>(reader: &mut R, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => filled += n,
        }
    }
    filled
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sine_file(dir: &std::path::Path, secs: u32) -> std::path::PathBuf {
        let path = dir.join("sine.flac");
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:sample_rate=44100:duration={secs}"))
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    #[tokio::test]
    async fn decodes_and_resamples_to_48k_stereo() {
        let dir = tempfile::tempdir().unwrap();
        let path = sine_file(dir.path(), 1);
        let mut decoder = Decoder::spawn("ffmpeg", &MediaInput::File(path), Duration::ZERO).unwrap();
        let mut frames = 0;
        loop {
            match decoder.try_next() {
                std::task::Poll::Ready(Some(frame)) => {
                    frames += 1;
                    // ffmpeg upmixes mono to identical channels
                    assert!((frame[100] - frame[101]).abs() < 1e-6);
                }
                std::task::Poll::Ready(None) => break,
                std::task::Poll::Pending => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
        assert_eq!(frames, 50, "1 s at 20 ms per frame");
        assert_eq!(decoder.error(), None);
    }

    #[tokio::test]
    async fn reports_errors() {
        let mut decoder = Decoder::spawn(
            "ffmpeg",
            &MediaInput::File("/nonexistent.mp3".into()),
            Duration::ZERO,
        )
        .unwrap();
        while !decoder.is_finished() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(decoder.error().unwrap().contains("No such file"));
    }
}
