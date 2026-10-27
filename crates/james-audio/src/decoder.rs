//! Decoding via an ffmpeg child process to 48 kHz interleaved stereo f32.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use james_core::audio::{CHANNELS, FRAME_LEN, SAMPLE_RATE};
use james_core::source::MediaInput;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, Command};
use tokio::sync::{mpsc, oneshot};

use crate::{silent_frame, Frame};

/// Frames buffered ahead of playback (2 s).
const BUFFER_FRAMES: usize = 100;
/// Network inputs fail after this long without data (ffmpeg `-rw_timeout`, in µs).
const NETWORK_TIMEOUT_US: &str = "15000000";
/// Protocols ffmpeg may use for URL inputs; excludes `file`, `concat`, `subfile`, … so a
/// crafted locator cannot read local files.
const URL_PROTOCOLS: &str = "http,https,tcp,tls,crypto,hls,httpproxy";
/// Keep at most this much of ffmpeg's error output.
const MAX_STDERR: usize = 4096;

/// How the decoder ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderExit {
    /// ffmpeg exited successfully.
    pub success: bool,
    /// Audio frames produced.
    pub frames: u64,
    /// Last lines of ffmpeg's error output (may be set even on success).
    pub stderr: String,
}

impl DecoderExit {
    /// Error message if the input failed: non-zero exit or no audio at all.
    pub fn error(&self) -> Option<String> {
        if self.success && self.frames > 0 {
            return None;
        }
        let detail = self.stderr.lines().last().unwrap_or("").trim();
        Some(match (self.success, detail.is_empty()) {
            (true, _) => "the input contains no audio".into(),
            (false, true) => "decoding failed".into(),
            (false, false) => detail.to_owned(),
        })
    }
}

pub struct Decoder {
    frames: mpsc::Receiver<Frame>,
    /// False while a helper input waits for a start slot (its time does not count as stall).
    source_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    exit: oneshot::Receiver<DecoderExit>,
    exited: Option<DecoderExit>,
}

/// Builds the ffmpeg arguments for an input (separated for testing).
pub fn ffmpeg_args(input: &MediaInput, start: Duration) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .map(String::from)
        .into();
    let seek = (!start.is_zero()).then(|| format!("{:.3}", start.as_secs_f64()));
    match input {
        MediaInput::Process { .. } => {
            args.extend(james_core::media::pipe_input_args());
            // Pipes cannot seek: as an output option, `-ss` decodes and discards up to the
            // position (as an input option it would make ffmpeg seek and fail on a pipe).
            if let Some(seek) = seek {
                args.extend(["-ss".into(), seek]);
            }
        }
        MediaInput::File(path) => {
            if let Some(seek) = seek {
                args.extend(["-ss".into(), seek]);
            }
            args.extend(james_core::media::local_input_args(path));
        }
        MediaInput::Url { url, headers } => {
            args.extend(
                [
                    "-protocol_whitelist",
                    URL_PROTOCOLS,
                    "-rw_timeout",
                    NETWORK_TIMEOUT_US,
                    "-reconnect",
                    "1",
                    "-reconnect_streamed",
                    "1",
                    "-reconnect_delay_max",
                    "5",
                ]
                .map(String::from),
            );
            if !headers.is_empty() {
                let joined: String = headers
                    .iter()
                    // Header injection: drop values containing line breaks.
                    .filter(|(k, v)| !k.contains(['\r', '\n']) && !v.contains(['\r', '\n']))
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect();
                args.extend(["-headers".into(), joined]);
            }
            if let Some(seek) = seek {
                args.extend(["-ss".into(), seek]);
            }
            args.extend(["-i".into(), url.clone()]);
        }
    }
    args.extend(
        [
            "-vn",
            "-sn",
            "-dn",
            "-af",
            "aresample=resampler=soxr:precision=28:dither_method=none",
        ]
        .map(String::from),
    );
    args.extend([
        "-ac".into(),
        CHANNELS.to_string(),
        "-ar".into(),
        SAMPLE_RATE.to_string(),
    ]);
    args.extend(["-f", "f32le", "pipe:1"].map(String::from));
    args
}

impl Decoder {
    /// `http_proxy` routes network inputs through the egress proxy.
    pub fn spawn(
        ffmpeg: &str,
        input: &MediaInput,
        start: Duration,
        http_proxy: Option<&str>,
    ) -> anyhow::Result<Self> {
        if let MediaInput::Url { url, .. } = input {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                anyhow::bail!("unsupported URL scheme");
            }
        }
        let mut cmd = Command::new(ffmpeg);
        // Network inputs only reach the internet through the egress proxy.
        james_core::process::apply_proxy_env(&mut cmd, http_proxy);
        let is_process = matches!(input, MediaInput::Process { .. });
        let mut child = cmd
            .args(ffmpeg_args(input, start))
            .stdin(if is_process { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to start {ffmpeg}"))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        // A helper process (yt-dlp) downloads the media; its output is copied into ffmpeg.
        let source_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(!is_process));
        let helper = match input {
            MediaInput::Process {
                program,
                args,
                proxy,
                temp_files,
            } => Some(tokio::spawn(run_helper(
                source_active.clone(),
                Helper {
                    program: program.clone(),
                    args: args.clone(),
                    proxy: proxy.clone(),
                },
                child.stdin.take().expect("stdin is piped"),
                temp_files.clone(),
            ))),
            _ => None,
        };

        let (frame_tx, frames) = mpsc::channel(BUFFER_FRAMES);
        let (exit_tx, exit) = oneshot::channel();

        // Drained concurrently, so a chatty ffmpeg can never block on a full stderr pipe.
        let stderr_task = tokio::spawn(read_stderr(stderr));

        tokio::spawn(async move {
            let mut reader = BufReader::with_capacity(FRAME_LEN * 4 * 4, stdout);
            let mut bytes = vec![0u8; FRAME_LEN * 4];
            let mut produced = 0u64;
            loop {
                let filled = tokio::select! {
                    n = read_full(&mut reader, &mut bytes) => n,
                    // Consumer dropped (skip/stop): stop reading immediately.
                    _ = frame_tx.closed() => break,
                };
                if filled == 0 {
                    break;
                }
                let mut frame = silent_frame();
                for (sample, chunk) in frame.iter_mut().zip(bytes[..filled].chunks_exact(4)) {
                    *sample = f32::from_le_bytes(chunk.try_into().expect("chunk of 4"));
                }
                if frame_tx.send(frame).await.is_err() {
                    break;
                }
                produced += 1;
                if filled < bytes.len() {
                    break;
                }
            }
            let _ = child.start_kill();
            let status = child.wait().await;
            let mut stderr = stderr_task.await.unwrap_or_default();
            let mut success = status.is_ok_and(|s| s.success());
            if let Some(mut helper) = helper {
                // The helper ends right after ffmpeg; its result explains failed downloads.
                // If it does not end in time it is aborted, which kills its process group.
                match tokio::time::timeout(HELPER_EXIT_GRACE + Duration::from_secs(12), &mut helper).await {
                    Ok(Ok(exit)) if !exit.success => {
                        // The helper's message explains failures before or instead of decoding
                        // (e.g. "Video unavailable"); a decode error of ffmpeg is kept otherwise.
                        if (success || produced == 0) && !exit.stderr.is_empty() {
                            stderr = exit.stderr;
                        }
                        success = false;
                    }
                    Ok(_) => {}
                    Err(_) => helper.abort(),
                }
            }
            let _ = exit_tx.send(DecoderExit {
                success,
                frames: produced,
                stderr,
            });
        });

        Ok(Self {
            frames,
            source_active,
            exit,
            exited: None,
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

    /// Whether the source delivers (helper inputs wait for a start slot first).
    pub fn source_active(&self) -> bool {
        self.source_active.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn buffered(&self) -> usize {
        self.frames.len()
    }

    pub fn is_finished(&self) -> bool {
        self.frames.is_closed() && self.frames.is_empty()
    }

    /// Exit information, available shortly after the input has ended.
    pub fn exit(&mut self) -> Option<&DecoderExit> {
        if self.exited.is_none() {
            self.exited = self.exit.try_recv().ok();
        }
        self.exited.as_ref()
    }

    /// Waits for the exit information (after [`Self::is_finished`]).
    pub async fn wait_exit(&mut self) -> DecoderExit {
        if let Some(exit) = self.exit() {
            return exit.clone();
        }
        let exit = (&mut self.exit).await.unwrap_or(DecoderExit {
            success: false,
            frames: 0,
            stderr: "decoder task ended unexpectedly".into(),
        });
        self.exited = Some(exit.clone());
        exit
    }
}

async fn read_stderr(stderr: ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut kept = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > MAX_STDERR {
                    kept.drain(..kept.len() - MAX_STDERR);
                }
            }
        }
    }
    String::from_utf8_lossy(&kept).trim().to_owned()
}

/// Concurrent helper starts (yt-dlp extraction is CPU and memory heavy); a slot is held
/// until the helper delivers data, then released for the rest of the playback.
static HELPER_STARTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
/// Time a helper has to deliver its first bytes (extraction, PO tokens, …).
const HELPER_FIRST_DATA: Duration = Duration::from_secs(60);

/// How long a helper that closed its output may take to exit before it is killed.
const HELPER_EXIT_GRACE: Duration = Duration::from_secs(5);

/// Waits until the child `pid` has exited but leaves it unreaped, so its pid (and with it the
/// process group id) stays reserved for the following `killpg`.
async fn wait_exit_without_reaping(pid: u32) {
    #[cfg(unix)]
    {
        let _ = tokio::task::spawn_blocking(move || {
            // SAFETY: plain syscall on our own child; WNOWAIT keeps it waitable.
            unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                while libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                ) == -1
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                }
            }
        })
        .await;
    }
    #[cfg(not(unix))]
    let _ = pid;
}

struct Helper {
    program: String,
    args: Vec<String>,
    proxy: Option<String>,
}

struct HelperExit {
    success: bool,
    stderr: String,
}

/// Kills the helper's whole process group (yt-dlp may run ffmpeg for HLS) when dropped.
struct GroupGuard(Option<u32>);

impl GroupGuard {
    /// After the leader was reaped the id may be reused: never signal it again.
    fn disarm(&mut self) {
        self.0 = None;
    }

    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall; the group id is the helper's pid (process_group(0)).
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs the helper and copies its stdout into ffmpeg's stdin. Dropping the future (abort)
/// kills the helper group; temp files are released when the last handle goes away.
async fn run_helper(
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    helper: Helper,
    mut ffmpeg_stdin: tokio::process::ChildStdin,
    _temp_files: Vec<std::sync::Arc<james_core::source::TempFile>>,
) -> HelperExit {
    let failed = |stderr: String| HelperExit {
        success: false,
        stderr,
    };
    let Ok(slot) = HELPER_STARTS.acquire().await else {
        return failed("shutting down".into());
    };
    let mut cmd = Command::new(&helper.program);
    james_core::process::apply_proxy_env(&mut cmd, helper.proxy.as_deref());
    cmd.args(&helper.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return failed(format!("failed to start {}: {e}", helper.program)),
    };
    let mut group = GroupGuard(child.id());
    active.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let stderr_task = tokio::spawn(read_stderr(child.stderr.take().expect("stderr is piped")));

    let mut slot = Some(slot);
    let mut buffer = vec![0u8; 64 * 1024];
    let started = tokio::time::Instant::now();
    // True when the helper closed its output itself (end of the download).
    let mut finished = false;
    loop {
        let read = if slot.is_some() {
            match tokio::time::timeout_at(started + HELPER_FIRST_DATA, stdout.read(&mut buffer)).await {
                Ok(read) => read,
                Err(_) => return failed("the download did not start in time".into()),
            }
        } else {
            stdout.read(&mut buffer).await
        };
        match read {
            Ok(0) => {
                finished = true;
                break;
            }
            Err(_) => break,
            Ok(n) => {
                // Data flows: the start slot is free for the next helper.
                slot = None;
                if ffmpeg_stdin.write_all(&buffer[..n]).await.is_err() {
                    break; // ffmpeg ended (stop, skip or decode error)
                }
            }
        }
    }
    drop(ffmpeg_stdin);
    // After a normal end give the helper a moment to exit on its own (yt-dlp may still be
    // cleaning up); killing it right away would report a finished download as failed.
    if finished {
        if let Some(pid) = child.id() {
            let _ = tokio::time::timeout(HELPER_EXIT_GRACE, wait_exit_without_reaping(pid)).await;
        }
    }
    // Stop everything still running in the group (on skip/stop also the download itself).
    // This happens before reaping: until then the leader, even if it already exited, holds
    // the group id, so the signal cannot hit an unrelated group that reused the pid.
    group.kill();
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    group.disarm();
    // Leftover children could keep stderr open; never wait for it indefinitely.
    let stderr = match tokio::time::timeout(Duration::from_secs(2), stderr_task).await {
        Ok(Ok(stderr)) => stderr,
        _ => String::new(),
    };
    let success = matches!(status, Ok(Ok(s)) if s.success());
    // Prefer yt-dlp's own error line over generic noise.
    let message = stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .map(str::to_owned)
        .unwrap_or(stderr);
    HelperExit {
        success,
        stderr: message,
    }
}

async fn read_full<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> usize {
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

    async fn drain(decoder: &mut Decoder) -> u64 {
        let mut frames = 0;
        loop {
            match decoder.try_next() {
                std::task::Poll::Ready(Some(frame)) => {
                    frames += 1;
                    // ffmpeg upmixes mono to identical channels
                    assert!((frame[100] - frame[101]).abs() < 1e-6);
                }
                std::task::Poll::Ready(None) => break frames,
                std::task::Poll::Pending => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    }

    #[tokio::test]
    async fn decodes_and_resamples_to_48k_stereo() {
        let dir = tempfile::tempdir().unwrap();
        let path = sine_file(dir.path(), 1);
        let mut decoder = Decoder::spawn("ffmpeg", &MediaInput::File(path), Duration::ZERO, None).unwrap();
        assert_eq!(drain(&mut decoder).await, 50, "1 s at 20 ms per frame");
        let exit = decoder.wait_exit().await;
        assert_eq!((exit.success, exit.frames, exit.error()), (true, 50, None));
    }

    #[tokio::test]
    async fn reports_errors() {
        let mut decoder = Decoder::spawn(
            "ffmpeg",
            &MediaInput::File("/nonexistent.mp3".into()),
            Duration::ZERO,
            None,
        )
        .unwrap();
        assert_eq!(drain(&mut decoder).await, 0);
        let exit = decoder.wait_exit().await;
        assert!(!exit.success);
        assert!(exit.error().unwrap().contains("No such file"), "{exit:?}");
    }

    #[tokio::test]
    async fn file_names_with_colons_are_not_protocols() {
        let dir = tempfile::tempdir().unwrap();
        let source = sine_file(dir.path(), 1);
        let tricky = dir.path().join("concat:x.flac");
        std::fs::rename(&source, &tricky).unwrap();
        let mut decoder = Decoder::spawn("ffmpeg", &MediaInput::File(tricky), Duration::ZERO, None).unwrap();
        assert_eq!(drain(&mut decoder).await, 50);
    }

    fn webm_file(dir: &std::path::Path, secs: u32) -> std::path::PathBuf {
        let path = dir.join("tone.webm");
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={secs}"))
            .args(["-c:a", "libopus"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    fn helper(script: &str, temp_files: Vec<std::sync::Arc<james_core::source::TempFile>>) -> MediaInput {
        MediaInput::Process {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            proxy: None,
            temp_files,
        }
    }

    #[tokio::test]
    async fn decodes_from_a_helper_process() {
        let dir = tempfile::tempdir().unwrap();
        let file = webm_file(dir.path(), 2);
        let input = helper(&format!("cat '{}'", file.display()), vec![]);
        let mut decoder = Decoder::spawn("ffmpeg", &input, Duration::ZERO, None).unwrap();
        assert_eq!(drain(&mut decoder).await, 100, "2 s of audio");
        assert!(decoder.wait_exit().await.error().is_none());
    }

    /// yt-dlp may keep running briefly after its output ended; that is still a success.
    #[tokio::test]
    async fn helpers_that_exit_after_closing_their_output_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let file = webm_file(dir.path(), 1);
        let input = helper(
            &format!("cat '{}'; exec >&-; sleep 1; exit 0", file.display()),
            vec![],
        );
        let mut decoder = Decoder::spawn("ffmpeg", &input, Duration::ZERO, None).unwrap();
        assert_eq!(drain(&mut decoder).await, 50);
        let exit = decoder.wait_exit().await;
        assert!(exit.error().is_none(), "{exit:?}");
    }

    /// Pipes cannot seek: the position must be reached by decoding, not by failing.
    #[tokio::test]
    async fn seeks_within_a_helper_stream() {
        let dir = tempfile::tempdir().unwrap();
        let file = webm_file(dir.path(), 4);
        let input = helper(&format!("cat '{}'", file.display()), vec![]);
        let mut decoder = Decoder::spawn("ffmpeg", &input, Duration::from_secs(3), None).unwrap();
        let frames = drain(&mut decoder).await;
        assert!(
            (48..=52).contains(&frames),
            "about 1 s left after seeking to 3 s, got {frames} frames"
        );
    }

    #[tokio::test]
    async fn helper_failures_are_reported() {
        let input = helper("echo 'ERROR: [youtube] x: Video unavailable' >&2; exit 1", vec![]);
        let mut decoder = Decoder::spawn("ffmpeg", &input, Duration::ZERO, None).unwrap();
        assert_eq!(drain(&mut decoder).await, 0);
        let exit = decoder.wait_exit().await;
        assert!(!exit.success);
        assert!(exit.error().unwrap().contains("Video unavailable"), "{exit:?}");
    }

    #[tokio::test]
    async fn dropping_the_decoder_kills_the_helper_and_removes_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = webm_file(dir.path(), 1);
        let marker = dir.path().join("cookies.txt");
        std::fs::write(&marker, "x").unwrap();
        let temp = std::sync::Arc::new(james_core::source::TempFile::new(marker.clone()));
        // Streams forever (like a live download) until killed.
        let input = helper(
            &format!("while true; do cat '{}'; done", file.display()),
            vec![temp],
        );
        let decoder = Decoder::spawn("ffmpeg", &input, Duration::ZERO, None).unwrap();
        while decoder.buffered() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(input);
        drop(decoder);
        for _ in 0..100 {
            if !marker.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("temp file still exists: the helper was not stopped");
    }

    /// A DASH manifest disguised as MP3 must not make ffmpeg fetch anything (SSRF).
    #[tokio::test]
    async fn playlist_formats_in_disguise_are_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evil.mp3");
        std::fs::write(
            &path,
            format!(r#"<?xml version="1.0"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT10S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011"><Period><AdaptationSet mimeType="audio/mp4"><Representation id="a" bandwidth="1000"><BaseURL>http://{address}/leak</BaseURL></Representation></AdaptationSet></Period></MPD>"#),
        )
        .unwrap();
        let mut decoder = Decoder::spawn("ffmpeg", &MediaInput::File(path), Duration::ZERO, None).unwrap();
        assert_eq!(drain(&mut decoder).await, 0);
        assert!(!decoder.wait_exit().await.success);
        let connected = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(connected.is_err(), "ffmpeg must not connect anywhere");
    }

    #[test]
    fn url_inputs_are_restricted() {
        let input = MediaInput::Url {
            url: "https://example.com/a".into(),
            headers: [
                ("X-Evil".to_string(), "a\r\nInjected: 1".to_string()),
                ("Accept".into(), "*/*".into()),
            ]
            .into(),
        };
        let args = ffmpeg_args(&input, Duration::ZERO);
        assert!(
            !args.iter().any(|a| a.contains("james:")),
            "no credentials on the command line"
        );
        let whitelist = args.iter().position(|a| a == "-protocol_whitelist").unwrap();
        assert!(!args[whitelist + 1].contains("file"));
        let headers = &args[args.iter().position(|a| a == "-headers").unwrap() + 1];
        assert_eq!(headers, "Accept: */*\r\n");
        assert!(Decoder::spawn(
            "ffmpeg",
            &MediaInput::Url {
                url: "file:///etc/passwd".into(),
                headers: Default::default()
            },
            Duration::ZERO,
            None
        )
        .is_err());
    }
}
