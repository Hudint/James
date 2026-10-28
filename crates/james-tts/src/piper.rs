//! Piper voices, one long-running worker process per voice in use (loading a model takes
//! seconds, synthesizing a sentence a fraction of one).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use james_core::source::{MediaInput, TempFile};
use james_core::tts::{normalize_language, Speech, TtsProvider, Voice};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const WORKER_SCRIPT: &str = include_str!("piper_worker.py");
/// Loading a model (first use of a voice).
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
const SYNTH_TIMEOUT: Duration = Duration::from_secs(30);
/// After a voice failed to load, requests fail fast for this long (no reload per request).
const LOAD_BACKOFF: Duration = Duration::from_secs(60);
/// Requests accepted at once (waiting or working) across all voices; more fail fast.
const MAX_REQUESTS: usize = 64;
/// Lines of Piper's error output kept for the log when a worker fails.
const STDERR_LINES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PiperConfig {
    /// Python interpreter with the `piper-tts` package installed.
    pub python: String,
    /// Directory with voices (`<name>.onnx` + `<name>.onnx.json`).
    pub voices_dir: PathBuf,
    /// Voice workers kept at the same time (each holds a model in memory).
    pub max_workers: usize,
    /// Workers unused for this long are stopped (the default voice stays loaded).
    pub idle_timeout_secs: u64,
}

impl Default for PiperConfig {
    fn default() -> Self {
        Self {
            python: "python3".into(),
            voices_dir: PathBuf::from("voices"),
            max_workers: 3,
            idle_timeout_secs: 600,
        }
    }
}

struct Model {
    voice: Voice,
    path: PathBuf,
}

struct Worker {
    // Kept for kill-on-drop.
    _child: Child,
    /// Counts this worker as loaded while it lives.
    _loaded: Loaded,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

/// One model loaded or loading, counted in [`PiperProvider::loaded`].
struct Loaded(Arc<std::sync::atomic::AtomicUsize>);

impl Loaded {
    /// Reserves a place if fewer than `limit` models are loaded or loading.
    fn reserve(count: &Arc<std::sync::atomic::AtomicUsize>, limit: usize) -> Option<Self> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut current = count.load(SeqCst);
        while current < limit {
            match count.compare_exchange(current, current + 1, SeqCst, SeqCst) {
                Ok(_) => return Some(Self(count.clone())),
                Err(actual) => current = actual,
            }
        }
        None
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

struct Slot {
    worker: Arc<tokio::sync::Mutex<Option<Worker>>>,
    last_used: Instant,
    /// The last failed start (requests fail fast for a while).
    failed_at: Option<Instant>,
}

impl Slot {
    /// Not used by a request right now (removing it then would start a second worker for
    /// the same voice) and not remembering a failed load (that would end the backoff).
    fn idle(&self) -> bool {
        Arc::strong_count(&self.worker) == 1
            && self.worker.try_lock().is_ok()
            && !self.failed_at.is_some_and(|t| t.elapsed() < LOAD_BACKOFF)
    }
}

pub struct PiperProvider {
    config: PiperConfig,
    script: PathBuf,
    work_dir: PathBuf,
    models: Vec<Model>,
    slots: parking_lot::Mutex<HashMap<String, Slot>>,
    default_voice: parking_lot::Mutex<Option<String>>,
    /// Requests in progress: at most `max_workers`, so a busy worker never forces another
    /// one to start beyond the limit (only the default voice's is kept in addition).
    permits: Arc<tokio::sync::Semaphore>,
    /// Workers alive (models in memory).
    loaded: Arc<std::sync::atomic::AtomicUsize>,
    /// Requests accepted (waiting or working), see [`MAX_REQUESTS`].
    requests: Arc<tokio::sync::Semaphore>,
    /// For requests that run on their own task.
    this: std::sync::Weak<Self>,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    ready: bool,
}

impl PiperProvider {
    /// Scans the voices directory. `work_dir` is a private directory for the WAV files.
    pub fn new(config: PiperConfig, work_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&work_dir)?;
        let script = work_dir.join("piper_worker.py");
        std::fs::write(&script, WORKER_SCRIPT).context("cannot write the Piper worker script")?;
        let models = scan(&config.voices_dir)?;
        let permits = Arc::new(tokio::sync::Semaphore::new(config.max_workers.max(1)));
        let provider = Arc::new_cyclic(|this| Self {
            config,
            script,
            work_dir,
            models,
            slots: parking_lot::Mutex::default(),
            default_voice: parking_lot::Mutex::default(),
            permits,
            requests: Arc::new(tokio::sync::Semaphore::new(MAX_REQUESTS)),
            loaded: Arc::default(),
            this: this.clone(),
        });
        tokio::spawn(reap(Arc::downgrade(&provider)));
        Ok(provider)
    }

    /// Loads the default voice in the background, so the first message is quick.
    pub fn warm_up(self: &Arc<Self>, voice: &str) {
        *self.default_voice.lock() = Some(voice.to_owned());
        let this = self.clone();
        let voice = voice.to_owned();
        tokio::spawn(async move {
            if let Err(e) = this.worker(&voice).await {
                tracing::warn!(%voice, error = %format!("{e:#}"), "cannot load the Piper voice");
            }
        });
    }

    fn model(&self, voice: &str) -> anyhow::Result<&Model> {
        self.models
            .iter()
            .find(|m| m.voice.id == voice)
            .ok_or_else(|| anyhow!("unknown voice '{voice}'"))
    }

    /// The worker slot of a voice (created on demand; the least recently used idle worker
    /// is stopped when too many run, never the default voice's).
    fn slot(&self, voice: &str) -> anyhow::Result<Arc<tokio::sync::Mutex<Option<Worker>>>> {
        let keep = self.default_voice.lock().clone();
        let mut slots = self.slots.lock();
        if !slots.contains_key(voice) && slots.len() >= self.config.max_workers.max(1) {
            let oldest = slots
                .iter()
                .filter(|(k, s)| Some(*k) != keep.as_ref() && s.idle())
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                slots.remove(&oldest);
            }
        }
        let slot = slots.entry(voice.to_owned()).or_insert_with(|| Slot {
            worker: Arc::default(),
            last_used: Instant::now(),
            failed_at: None,
        });
        if slot.failed_at.is_some_and(|t| t.elapsed() < LOAD_BACKOFF) {
            bail!("the voice '{voice}' failed to load, try again later");
        }
        slot.last_used = Instant::now();
        Ok(slot.worker.clone())
    }

    /// Stops idle workers beyond `max_workers` (least recently used first, never the default
    /// voice's).
    fn trim(&self) {
        let keep = self.default_voice.lock().clone();
        let mut slots = self.slots.lock();
        while slots.len() > self.config.max_workers.max(1) {
            let Some(oldest) = slots
                .iter()
                .filter(|(k, s)| Some(*k) != keep.as_ref() && s.idle())
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            slots.remove(&oldest);
        }
    }

    fn load_failed(&self, voice: &str) {
        if let Some(slot) = self.slots.lock().get_mut(voice) {
            slot.failed_at = Some(Instant::now());
        }
    }

    async fn start(&self, model: &Model) -> anyhow::Result<Worker> {
        // Counted from the start of loading: concurrent loads cannot exceed the limit (the
        // default voice's worker may come on top).
        let loaded = Loaded::reserve(&self.loaded, self.config.max_workers.max(1) + 1)
            .ok_or_else(|| anyhow!("text to speech is busy, try again later"))?;
        let mut child = Command::new(&self.config.python)
            // Isolated mode: nothing from the environment or the script's directory is
            // imported.
            .arg("-I")
            // -I ignores PYTHON* variables: UTF-8 and unbuffered output as options.
            .args(["-X", "utf8", "-u"])
            .arg(&self.script)
            .arg(&model.path)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("cannot start Piper ({})", self.config.python))?;
        let stdin = child.stdin.take().expect("piped");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
        let stderr = Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::new()));
        tokio::spawn(collect_stderr(
            child.stderr.take().expect("piped"),
            stderr.clone(),
        ));
        let ready = tokio::time::timeout(LOAD_TIMEOUT, async {
            // Libraries may print before the worker is ready: wait for its line.
            while let Some(line) = stdout.next_line().await? {
                if serde_json::from_str::<Answer>(&line).is_ok_and(|a| a.ready) {
                    return Ok(());
                }
            }
            Err(anyhow!("Piper exited while loading the voice"))
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("the Piper voice did not load in time")));
        if let Err(e) = ready {
            // Give the error output a moment to arrive.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let output: Vec<String> = stderr.lock().iter().cloned().collect();
            tracing::warn!(voice = %model.voice.id, output = %output.join("\n"), "Piper failed to load the voice");
            return Err(e);
        }
        Ok(Worker {
            _child: child,
            _loaded: loaded,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// The started worker of `voice`, locked for one request.
    async fn worker(&self, voice: &str) -> anyhow::Result<tokio::sync::OwnedMutexGuard<Option<Worker>>> {
        let model = self.model(voice)?;
        let mut guard = self.slot(voice)?.lock_owned().await;
        if guard.is_none() {
            match self.start(model).await {
                Ok(worker) => *guard = Some(worker),
                // Busy is no failure of the voice (no backoff for it).
                Err(e) if e.to_string().contains("busy") => return Err(e),
                Err(e) => {
                    self.load_failed(voice);
                    return Err(e);
                }
            }
        }
        Ok(guard)
    }
}

impl Worker {
    async fn synthesize(&mut self, text: &str, output: &Path) -> anyhow::Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            serde_json::to_string(&serde_json::json!({ "id": id, "text": text, "output": output }))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        loop {
            let line = self
                .stdout
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("Piper exited"))?;
            let Ok(answer) = serde_json::from_str::<Answer>(&line) else {
                continue;
            };
            match answer.id {
                Some(answered) if answered == id => {}
                // An answer without id: the worker could not read a request (out of sync).
                None if answer.error.is_some() => bail!("Piper is out of sync"),
                _ => continue,
            }
            return match answer.error {
                Some(error) => Err(anyhow!("Piper failed: {error}")),
                None if answer.ok => Ok(()),
                None => Err(anyhow!("unexpected answer of Piper")),
            };
        }
    }
}

#[async_trait]
impl TtsProvider for PiperProvider {
    fn id(&self) -> &str {
        "piper"
    }

    fn voices(&self) -> Vec<Voice> {
        self.models.iter().map(|m| m.voice.clone()).collect()
    }

    async fn synthesize(&self, text: &str, voice: &str) -> anyhow::Result<Speech> {
        let output = self
            .work_dir
            .join(format!("tts-{:016x}.wav", rand::random::<u64>()));
        let temp = Arc::new(TempFile::new(output.clone()));
        let this = self
            .this
            .upgrade()
            .ok_or_else(|| anyhow!("text to speech is shutting down"))?;
        let accepted = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("text to speech is busy, try again later"))?;
        let (text, voice) = (text.to_owned(), voice.to_owned());
        let task_temp = temp.clone();
        // The request runs on its own task: a cancelled request (hush) neither interrupts
        // loading the voice or a half-written line, nor leaves the WAV file behind (dropping
        // the TempFile after the worker wrote it removes it).
        let request = tokio::spawn(async move {
            let _accepted = accepted;
            // Nobody waits for the result any more (hush): no work for it.
            let abandoned = || Arc::strong_count(&task_temp) == 1;
            let busy = || anyhow!("text to speech is busy, try again later");
            let model = this.model(&voice)?;
            // Waiting for the voice's worker costs no permit; one is needed to work.
            let mut guard = tokio::time::timeout(LOAD_TIMEOUT, this.slot(&voice)?.lock_owned())
                .await
                .map_err(|_| busy())?;
            let _permit = tokio::time::timeout(LOAD_TIMEOUT, this.permits.clone().acquire_owned())
                .await
                .map_err(|_| busy())??;
            if abandoned() {
                bail!("cancelled");
            }
            if guard.is_none() {
                // Make room first: no more models in memory than allowed (the default voice's
                // may come on top).
                this.trim();
                match this.start(model).await {
                    Ok(worker) => *guard = Some(worker),
                    // Busy is no failure of the voice (no backoff for it).
                    Err(e) if e.to_string().contains("busy") => return Err(e),
                    Err(e) => {
                        this.load_failed(&voice);
                        return Err(e);
                    }
                }
            }
            let worker = guard.as_mut().expect("started");
            let result = tokio::time::timeout(SYNTH_TIMEOUT, worker.synthesize(&text, &output))
                .await
                .context("Piper did not answer in time")
                .and_then(|r| r);
            if result.is_err() {
                // A failed or hung worker is replaced on the next request.
                *guard = None;
            }
            drop(task_temp);
            result
        });
        request.await.context("Piper request failed")??;
        let path = temp.path().to_path_buf();
        let loudness = tokio::task::spawn_blocking(move || wav_loudness(&path))
            .await
            .ok()
            .flatten();
        Ok(Speech {
            input: MediaInput::File(temp.path().to_path_buf()),
            temp: Some(temp),
            loudness,
        })
    }
}

/// Stops workers unused for longer than the idle timeout and any beyond `max_workers` (not the
/// default voice), and removes leftover audio files.
async fn reap(provider: std::sync::Weak<PiperProvider>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let Some(provider) = provider.upgrade() else {
            return;
        };
        let idle = Duration::from_secs(provider.config.idle_timeout_secs);
        let keep = provider.default_voice.lock().clone();
        sweep(&provider.work_dir, Duration::from_secs(30 * 60));
        provider.slots.lock().retain(|voice, slot| {
            Some(voice) == keep.as_ref() || slot.last_used.elapsed() < idle || !slot.idle()
        });
        provider.trim();
    }
}

/// Removes audio files left behind (e.g. by a crash) that no speech can still be using.
fn sweep(dir: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e > age));
        if name.starts_with("tts-") && name.ends_with(".wav") && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Keeps the last lines of a worker's error output (for the log when it fails).
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
        lines.push_back(line.chars().take(500).collect());
    }
}

/// Integrated loudness (LUFS) of a 16-bit PCM WAV file as Piper writes it.
fn wav_loudness(path: &Path) -> Option<f32> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut channels, mut rate, mut bits, mut data) = (0u16, 0u32, 0u16, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = bytes.get(at + 8..(at + 8).saturating_add(len).min(bytes.len()))?;
        match id {
            b"fmt " if body.len() >= 16 => {
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
                bits = u16::from_le_bytes([body[14], body[15]]);
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let data = data?;
    if bits != 16 || channels == 0 || rate == 0 {
        return None;
    }
    let samples: Vec<i16> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let mut meter = ebur128::EbuR128::new(u32::from(channels), rate, ebur128::Mode::I).ok()?;
    meter.add_frames_i16(&samples).ok()?;
    meter
        .loudness_global()
        .ok()
        .map(|l| l as f32)
        .filter(|l| l.is_finite())
}

#[derive(Deserialize)]
struct ModelConfig {
    #[serde(default)]
    language: Option<ModelLanguage>,
    #[serde(default)]
    dataset: Option<String>,
    #[serde(default)]
    audio: Option<ModelAudio>,
}

#[derive(Deserialize)]
struct ModelLanguage {
    code: String,
    #[serde(default)]
    name_english: Option<String>,
}

#[derive(Deserialize)]
struct ModelAudio {
    #[serde(default)]
    quality: Option<String>,
}

/// Voices in `dir`: every `<name>.onnx` with a `<name>.onnx.json` next to it.
fn scan(dir: &Path) -> anyhow::Result<Vec<Model>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut models = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "onnx") {
            continue;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let config_path = path.with_extension("onnx.json");
        let config: ModelConfig = match std::fs::read(&config_path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice(&bytes)?))
        {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(voice = %id, error = %format!("{e:#}"), "skipping a Piper voice without a valid config");
                continue;
            }
        };
        let language = config
            .language
            .as_ref()
            .map(|l| normalize_language(&l.code))
            .unwrap_or_else(|| normalize_language(id.split('-').next().unwrap_or_default()));
        let speaker = config.dataset.clone().unwrap_or_else(|| id.clone());
        let quality = config.audio.and_then(|a| a.quality).unwrap_or_default();
        let language_name = config.language.and_then(|l| l.name_english).unwrap_or_default();
        let name = [speaker, language_name, quality]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        models.push(Model {
            voice: Voice {
                id,
                name,
                language,
                provider: "piper".into(),
            },
            path,
        });
    }
    models.sort_by(|a, b| a.voice.id.cmp(&b.voice.id));
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voices_are_read_from_their_configs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("de_DE-thorsten-medium.onnx"), "").unwrap();
        std::fs::write(
            dir.path().join("de_DE-thorsten-medium.onnx.json"),
            r#"{"language":{"code":"de_DE","name_english":"German"},"dataset":"thorsten","audio":{"quality":"medium"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.onnx"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        let models = scan(dir.path()).unwrap();
        assert_eq!(models.len(), 1, "voices without a config are skipped");
        assert_eq!(
            models[0].voice,
            Voice {
                id: "de_DE-thorsten-medium".into(),
                name: "thorsten · German · medium".into(),
                language: "de-DE".into(),
                provider: "piper".into(),
            }
        );
        assert!(scan(&dir.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn loaded_models_are_capped_while_loading() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a = Loaded::reserve(&count, 2).unwrap();
        let _b = Loaded::reserve(&count, 2).unwrap();
        assert!(Loaded::reserve(&count, 2).is_none(), "full");
        drop(a);
        assert!(
            Loaded::reserve(&count, 2).is_some(),
            "a failed load gives its place back"
        );
    }

    #[test]
    fn loudness_of_a_wav_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        // 1 s of a 1 kHz sine at half scale, mono 22.05 kHz.
        let rate = 22_050u32;
        let samples: Vec<u8> = (0..rate)
            .flat_map(|i| {
                let v = (f32::sin(i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / rate as f32) * 16_384.0)
                    as i16;
                v.to_le_bytes()
            })
            .collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        wav.extend_from_slice(&samples);
        std::fs::write(&path, &wav).unwrap();
        let lufs = wav_loudness(&path).unwrap();
        // A half-scale sine is about -6 dBFS RMS -3 dB; K-weighting adds little at 1 kHz.
        assert!((-12.0..-6.0).contains(&lufs), "{lufs}");
        std::fs::write(&path, b"not a wav").unwrap();
        assert_eq!(wav_loudness(&path), None);
    }

    /// Real synthesis; runs when `JAMES_PIPER_PYTHON` and `JAMES_PIPER_VOICES` are set.
    #[tokio::test]
    async fn synthesizes_with_piper() {
        let (Ok(python), Ok(voices)) = (
            std::env::var("JAMES_PIPER_PYTHON"),
            std::env::var("JAMES_PIPER_VOICES"),
        ) else {
            eprintln!("JAMES_PIPER_PYTHON / JAMES_PIPER_VOICES not set, skipping");
            return;
        };
        let work = tempfile::tempdir().unwrap();
        let provider = PiperProvider::new(
            PiperConfig {
                python,
                voices_dir: voices.into(),
                ..Default::default()
            },
            work.path().to_path_buf(),
        )
        .unwrap();
        let voice = provider
            .voices()
            .first()
            .expect("a voice is installed")
            .id
            .clone();
        let first = provider
            .synthesize("Hallo, ich bin James.", &voice)
            .await
            .unwrap();
        let path = match &first.input {
            MediaInput::File(path) => path.clone(),
            _ => panic!("a file"),
        };
        assert!(std::fs::metadata(&path).unwrap().len() > 10_000, "audio written");
        let lufs = first.loudness.expect("loudness measured");
        assert!((-40.0..0.0).contains(&lufs), "{lufs}");
        // The model stays loaded: the second request is fast.
        let started = Instant::now();
        provider.synthesize("Noch ein Satz.", &voice).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "took {:?}",
            started.elapsed()
        );
        drop(first);
        assert!(!path.exists(), "the file is removed once the speech is dropped");

        // A cancelled request leaves no file behind and does not block the worker.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(20),
            provider.synthesize("Dieser Satz wird abgebrochen, bevor er fertig ist.", &voice),
        )
        .await;
        assert!(cancelled.is_err(), "cancelled");
        let started = Instant::now();
        provider.synthesize("Weiter.", &voice).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        let left: Vec<_> = std::fs::read_dir(work.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".wav"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}
