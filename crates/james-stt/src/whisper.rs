//! Speech recognition with [faster-whisper](https://github.com/SYSTRAN/faster-whisper): one
//! long-running worker process keeps the model loaded (loading takes a second, recognising a
//! short sentence about as long as it was spoken on two cores).
//!
//! A single task owns the worker and handles one request after another, so a caller that gives
//! up (or is cancelled) never leaves the worker's pipes half written, and a worker that hangs is
//! replaced.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use base64::Engine as _;
use james_core::stt::{SttProvider, Transcript, MAX_SAMPLES, MAX_WAIT};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

const WORKER_SCRIPT: &str = include_str!("whisper_worker.py");
/// After the model failed to load, requests fail fast for this long.
const LOAD_BACKOFF: Duration = Duration::from_secs(60);
/// Utterances accepted at once (waiting or working); more are dropped (speech keeps coming).
/// Each listening instance has at most one utterance in recognition at a time (its listener waits
/// for the answer), so this is about how many instances can share the worker; requests that wait
/// longer than [`MAX_WAIT`] are skipped.
const MAX_REQUESTS: usize = 64;
/// Lines of the worker's error output kept for the log when it fails.
const STDERR_LINES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WhisperConfig {
    /// Python interpreter with the `faster-whisper` package installed.
    pub python: String,
    /// Directory of a faster-whisper (CTranslate2) model, e.g. `models/whisper-base`.
    pub model: PathBuf,
    /// CPU threads used for recognition.
    pub threads: usize,
    /// The model is unloaded after this long without speech.
    pub idle_timeout_secs: u64,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            python: "python3".into(),
            model: PathBuf::from("models/whisper-base"),
            threads: 2,
            idle_timeout_secs: 600,
        }
    }
}

/// How long the worker may take.
#[derive(Debug, Clone, Copy)]
struct Limits {
    load: Duration,
    transcribe: Duration,
}

const LIMITS: Limits = Limits {
    load: Duration::from_secs(120),
    transcribe: Duration::from_secs(60),
};

struct Request {
    pcm: Vec<i16>,
    language: Option<String>,
    prompt: Option<String>,
    /// Not started after this (the caller would not wait for the answer).
    deadline: Instant,
    reply: oneshot::Sender<anyhow::Result<Transcript>>,
}

enum Job {
    /// Load the model now (an instance will listen).
    Prepare,
    Transcribe(Request),
}

pub struct WhisperProvider {
    jobs: mpsc::Sender<Job>,
}

struct Worker {
    // Kept for kill-on-drop.
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

/// The worker could not recognise this utterance but is fine (no restart needed).
#[derive(Debug)]
struct Unrecognised(String);

impl std::fmt::Display for Unrecognised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "speech recognition failed: {}", self.0)
    }
}

impl std::error::Error for Unrecognised {}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    ready: bool,
}

impl WhisperProvider {
    /// Checks the model directory.
    pub fn new(config: WhisperConfig) -> anyhow::Result<Arc<Self>> {
        Self::with_limits(config, LIMITS)
    }

    fn with_limits(config: WhisperConfig, limits: Limits) -> anyhow::Result<Arc<Self>> {
        if !config.model.join("model.bin").is_file() {
            bail!("no Whisper model in {}", config.model.display());
        }
        let (jobs, rx) = mpsc::channel(MAX_REQUESTS);
        tokio::spawn(
            Owner {
                config,
                limits,
                worker: None,
                failed_at: None,
            }
            .run(rx),
        );
        Ok(Arc::new(Self { jobs }))
    }
}

/// The task that owns the worker process.
struct Owner {
    config: WhisperConfig,
    limits: Limits,
    worker: Option<Worker>,
    failed_at: Option<Instant>,
}

impl Owner {
    async fn run(mut self, mut jobs: mpsc::Receiver<Job>) {
        let idle = Duration::from_secs(self.config.idle_timeout_secs.max(30));
        loop {
            let job = match tokio::time::timeout(idle, jobs.recv()).await {
                Ok(Some(job)) => job,
                // The provider is gone.
                Ok(None) => return,
                Err(_) => {
                    if self.worker.take().is_some() {
                        tracing::info!("speech recognition model unloaded (unused)");
                    }
                    continue;
                }
            };
            match job {
                Job::Prepare => {
                    let _ = self.ready().await;
                }
                Job::Transcribe(request) => {
                    if Instant::now() >= request.deadline || request.reply.is_closed() {
                        continue;
                    }
                    let result = self.transcribe(&request).await;
                    let _ = request.reply.send(result);
                }
            }
        }
    }

    /// The loaded worker (loaded now if needed).
    async fn ready(&mut self) -> anyhow::Result<&mut Worker> {
        if self.worker.is_none() {
            if self.failed_at.is_some_and(|t| t.elapsed() < LOAD_BACKOFF) {
                bail!("the Whisper model failed to load, try again later");
            }
            match self.start().await {
                Ok(worker) => {
                    self.worker = Some(worker);
                    self.failed_at = None;
                }
                Err(e) => {
                    self.failed_at = Some(Instant::now());
                    tracing::warn!(error = %format!("{e:#}"), "cannot load the Whisper model");
                    return Err(e);
                }
            }
        }
        Ok(self.worker.as_mut().expect("loaded"))
    }

    async fn transcribe(&mut self, request: &Request) -> anyhow::Result<Transcript> {
        self.ready().await?;
        // Loading may have taken long: the caller may be gone by now.
        let left = request.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || request.reply.is_closed() {
            bail!("speech recognition took too long");
        }
        let limit = self.limits.transcribe.min(left);
        let worker = self.worker.as_mut().expect("loaded");
        let result = tokio::time::timeout(
            limit,
            worker.transcribe(
                &request.pcm,
                request.language.as_deref(),
                request.prompt.as_deref(),
            ),
        )
        .await
        .unwrap_or_else(|_| Err(anyhow!("speech recognition took too long")));
        // A worker that failed (rather than an utterance it could not recognise) is out of step
        // with its requests or hangs: it is replaced.
        if result.as_ref().is_err_and(|e| !e.is::<Unrecognised>()) {
            self.worker = None;
        }
        result
    }

    async fn start(&self) -> anyhow::Result<Worker> {
        let mut child = Command::new(&self.config.python)
            // Isolated mode: nothing from the environment or the script's directory is imported.
            .arg("-I")
            .args(["-X", "utf8", "-u"])
            // The script is passed inline: nothing on disk that could be changed.
            .arg("-c")
            .arg(WORKER_SCRIPT)
            .arg(&self.config.model)
            .arg(self.config.threads.clamp(1, 64).to_string())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            // The model is local: never download anything.
            .env("HF_HUB_OFFLINE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("cannot start Whisper ({})", self.config.python))?;
        let stdin = child.stdin.take().expect("piped");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
        let stderr = Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::new()));
        tokio::spawn(collect_stderr(
            child.stderr.take().expect("piped"),
            stderr.clone(),
        ));
        let ready = tokio::time::timeout(self.limits.load, async {
            // Libraries may print before the worker is ready: wait for its line.
            while let Some(line) = stdout.next_line().await? {
                if serde_json::from_str::<Answer>(&line).is_ok_and(|a| a.ready) {
                    return Ok(());
                }
            }
            Err(anyhow!("Whisper exited while loading the model"))
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("the Whisper model did not load in time")));
        if let Err(e) = ready {
            // Give the error output a moment to arrive; its end says why (e.g. a missing module).
            tokio::time::sleep(Duration::from_millis(100)).await;
            let output: Vec<String> = stderr.lock().iter().cloned().collect();
            let reason: String = output
                .join(" | ")
                .chars()
                .rev()
                .take(1000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(if reason.is_empty() { e } else { e.context(reason) });
        }
        tracing::info!(model = %self.config.model.display(), "speech recognition model loaded");
        Ok(Worker {
            _child: child,
            stdin,
            stdout,
            next_id: 1,
        })
    }
}

impl Worker {
    async fn transcribe(
        &mut self,
        pcm: &[i16],
        language: Option<&str>,
        prompt: Option<&str>,
    ) -> anyhow::Result<Transcript> {
        let id = self.next_id;
        self.next_id += 1;
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut line = serde_json::to_string(&serde_json::json!({
            "id": id,
            "audio": base64::engine::general_purpose::STANDARD.encode(bytes),
            "language": language,
            "prompt": prompt,
        }))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        loop {
            let line = self
                .stdout
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("Whisper exited"))?;
            let Ok(answer) = serde_json::from_str::<Answer>(&line) else {
                continue;
            };
            match answer.id {
                Some(answered) if answered == id => {}
                None if answer.error.is_some() => bail!("Whisper is out of sync"),
                _ => continue,
            }
            if let Some(error) = answer.error {
                return Err(Unrecognised(error).into());
            }
            return Ok(Transcript {
                text: answer.text.unwrap_or_default(),
                language: answer.language,
            });
        }
    }
}

#[async_trait]
impl SttProvider for WhisperProvider {
    fn id(&self) -> &str {
        "whisper"
    }

    fn prepare(&self) {
        let _ = self.jobs.try_send(Job::Prepare);
    }

    async fn transcribe(
        &self,
        pcm: Vec<i16>,
        language: Option<String>,
        prompt: Option<String>,
    ) -> anyhow::Result<Transcript> {
        if pcm.len() > MAX_SAMPLES {
            bail!("the utterance is too long");
        }
        let (reply, answer) = oneshot::channel();
        let request = Request {
            pcm,
            language,
            prompt,
            // Callers wait about this long at most (see the speech listener).
            deadline: Instant::now() + MAX_WAIT,
            reply,
        };
        self.jobs
            .try_send(Job::Transcribe(request))
            .map_err(|_| anyhow!("speech recognition is busy"))?;
        answer.await.map_err(|_| anyhow!("speech recognition stopped"))?
    }
}

async fn collect_stderr(
    stderr: tokio::process::ChildStderr,
    lines: Arc<parking_lot::Mutex<std::collections::VecDeque<String>>>,
) {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let mut lines = lines.lock();
        if lines.len() >= STDERR_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for Python that runs `script` (bash) as the worker.
    fn fake(dir: &std::path::Path, script: &str) -> WhisperConfig {
        let python = dir.join("python");
        std::fs::write(&python, format!("#!/bin/bash\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let model = dir.join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.bin"), b"").unwrap();
        WhisperConfig {
            python: python.display().to_string(),
            model,
            ..Default::default()
        }
    }

    const SHORT: Limits = Limits {
        load: Duration::from_secs(5),
        transcribe: Duration::from_millis(500),
    };

    #[tokio::test]
    async fn answers_and_recovers_from_a_hanging_worker() {
        let dir = tempfile::tempdir().unwrap();
        // Answers each request with its id; the first worker (no marker file yet) hangs on its
        // first request.
        let marker = dir.path().join("started");
        let script = format!(
            r#"echo noise; echo '{{"ready": true}}'
first=0; [ -e {m} ] || first=1; touch {m}
while read -r line; do
  id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
  [ "$first" = 1 ] && sleep 30
  echo "{{\"id\": $id, \"text\": \"hallo\", \"language\": \"de\"}}"
done"#,
            m = marker.display()
        );
        let provider = WhisperProvider::with_limits(fake(dir.path(), &script), SHORT).unwrap();
        let hung = provider.transcribe(vec![0; 16000], None, None).await;
        assert!(hung.is_err(), "the hanging worker times out");
        let heard = provider
            .transcribe(vec![0; 16000], Some("de".into()), None)
            .await
            .unwrap();
        assert_eq!(
            (heard.text.as_str(), heard.language.as_deref()),
            ("hallo", Some("de"))
        );
    }

    #[tokio::test]
    async fn a_cancelled_caller_does_not_break_the_next_request() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"echo '{"ready": true}'
while read -r line; do
  id=$(echo "$line" | sed -E 's/.*"id":([0-9]+).*/\1/')
  sleep 0.2
  echo "{\"id\": $id, \"text\": \"ok $id\"}"
done"#;
        let provider = WhisperProvider::with_limits(fake(dir.path(), script), SHORT).unwrap();
        // 15 s of audio is larger than a pipe buffer; the caller gives up while it is sent.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(50),
            provider.transcribe(vec![1; MAX_SAMPLES], None, None),
        )
        .await;
        assert!(cancelled.is_err());
        let next = provider.transcribe(vec![0; 16000], None, None).await.unwrap();
        assert!(next.text.starts_with("ok "), "{}", next.text);
    }

    #[tokio::test]
    async fn a_model_that_fails_to_load_backs_off() {
        let dir = tempfile::tempdir().unwrap();
        let provider = WhisperProvider::with_limits(fake(dir.path(), "exit 1"), SHORT).unwrap();
        let first = provider.transcribe(vec![0; 100], None, None).await.unwrap_err();
        assert!(first.to_string().contains("exited"), "{first}");
        let second = provider.transcribe(vec![0; 100], None, None).await.unwrap_err();
        assert!(second.to_string().contains("try again later"), "{second}");
    }
}
