//! Speech recognition of an instance: decodes what clients say, cuts it into utterances (a
//! pause ends one) and has them recognised one after another.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use audiopus::coder::Decoder;
use audiopus::{Channels, SampleRate};
use futures::future::BoxFuture;
use james_core::platform::{Client, PlatformId, VoicePacket};
use james_core::stt::{SttProvider, Transcript, MAX_SAMPLES, SAMPLE_RATE};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// A pause this long ends an utterance.
const PAUSE: Duration = Duration::from_millis(700);
/// Shorter sounds (a cough, a click) are not recognised.
const MIN_SAMPLES: usize = SAMPLE_RATE as usize * 2 / 5;
/// Samples of the longest Opus frame (120 ms) at 16 kHz.
const MAX_FRAME: usize = SAMPLE_RATE as usize * 120 / 1000;
/// Utterances waiting for recognition; more are dropped (speech keeps coming).
const QUEUE: usize = 4;
/// Speech of one client recognised per minute at most (one person talking non-stop cannot keep
/// recognition busy for everyone).
const SPEAKER_BUDGET: usize = SAMPLE_RATE as usize * 60;

struct Speaker {
    decoder: Decoder,
    samples: Vec<i16>,
    last: Instant,
    /// Talking without any pause for the longest utterance: music or noise, not speech. Ignored
    /// until the next pause.
    continuous: bool,
    /// Samples recognised in the current minute (see [`SPEAKER_BUDGET`]).
    used: usize,
    window: Instant,
}

impl Speaker {
    /// Whether these samples still fit this speaker's budget (and counts them).
    fn spend(&mut self, samples: usize) -> bool {
        if self.window.elapsed() >= Duration::from_secs(60) {
            self.window = Instant::now();
            self.used = 0;
        }
        self.used += samples;
        self.used <= SPEAKER_BUDGET
    }
}

/// One recognised utterance.
pub struct Heard {
    /// Who said it, as known when the utterance ended.
    pub client: Client,
    pub transcript: Transcript,
    /// When the utterance ended.
    pub at: Instant,
}

pub struct ListenerConfig {
    pub language: Option<String>,
    /// Words that are likely (e.g. the bot's name).
    pub prompt: Option<String>,
}

/// Looks up the client that spoke (`None`: gone, or a client whose voice is ignored).
pub type Resolve = Arc<dyn Fn(PlatformId) -> BoxFuture<'static, Option<Client>> + Send + Sync>;

/// Runs until `voice` closes; recognised utterances go to `heard`.
pub async fn listen(
    provider: Arc<dyn SttProvider>,
    config: ListenerConfig,
    resolve: Resolve,
    mut voice: mpsc::Receiver<VoicePacket>,
    heard: mpsc::UnboundedSender<Heard>,
) {
    let mut speakers: HashMap<PlatformId, Speaker> = HashMap::new();
    let (utterances_tx, mut utterances) = mpsc::channel::<(Client, Vec<i16>, Instant)>(QUEUE);
    // Recognised one after another, in the order spoken; when it cannot keep up, utterances are
    // dropped (see `flush`). Stops with the listener (queued utterances are dropped).
    let recogniser = AbortOnDrop(tokio::spawn(tracing::Instrument::in_current_span(async move {
        let failures = james_core::logs::RateLimiter::new(3, Duration::from_secs(60));
        while let Some((client, samples, at)) = utterances.recv().await {
            let result = tokio::time::timeout(
                james_core::stt::MAX_WAIT.saturating_sub(at.elapsed()),
                provider.transcribe(samples, config.language.clone(), config.prompt.clone()),
            )
            .await;
            match result {
                Ok(Ok(transcript)) if !only_prompt(&transcript.text, config.prompt.as_deref()) => {
                    let _ = heard.send(Heard {
                        client,
                        transcript,
                        at,
                    });
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) if failures.allow("failed") => {
                    tracing::warn!(error = %format!("{e:#}"), "speech recognition failed")
                }
                Ok(Err(_)) => {}
                Err(_) => tracing::debug!("speech recognition too slow, an utterance was dropped"),
            }
        }
    })));
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            packet = voice.recv() => {
                let Some(packet) = packet else { break };
                let speaker = match speakers.entry(packet.client.clone()) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let Ok(decoder) = Decoder::new(SampleRate::Hz16000, Channels::Mono) else { continue };
                        e.insert(Speaker {
                            decoder,
                            samples: Vec::new(),
                            last: Instant::now(),
                            continuous: false,
                            used: 0,
                            window: Instant::now(),
                        })
                    }
                };
                speaker.last = Instant::now();
                if packet.data.is_empty() {
                    // The client stopped talking.
                    let samples = std::mem::take(&mut speaker.samples);
                    speaker.continuous = false;
                    if speaker.spend(samples.len()) {
                        flush(&resolve, packet.client, samples, &utterances_tx).await;
                    }
                    continue;
                }
                if speaker.continuous {
                    continue;
                }
                let start = speaker.samples.len();
                speaker.samples.resize(start + MAX_FRAME, 0);
                let decoded = audiopus::packet::Packet::try_from(&packet.data[..])
                    .and_then(|input| {
                        let output = audiopus::MutSignals::try_from(&mut speaker.samples[start..])?;
                        speaker.decoder.decode(Some(input), output, false)
                    });
                // Not Opus (TeamSpeak sometimes sends short other packets): skipped.
                speaker.samples.truncate(start + decoded.unwrap_or(0));
                if speaker.samples.len() + MAX_FRAME > MAX_SAMPLES {
                    // No pause in the longest utterance: music (e.g. another bot) or noise.
                    speaker.samples.clear();
                    speaker.continuous = true;
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let mut ended = Vec::new();
                for (client, speaker) in &mut speakers {
                    if now.duration_since(speaker.last) >= PAUSE {
                        let samples = std::mem::take(&mut speaker.samples);
                        if !samples.is_empty() && speaker.spend(samples.len()) {
                            ended.push((client.clone(), samples));
                        }
                        speaker.continuous = false;
                    }
                }
                for (client, samples) in ended {
                    flush(&resolve, client, samples, &utterances_tx).await;
                }
                // Clients that went quiet long ago (or left) do not keep a decoder.
                speakers.retain(|_, s| now.duration_since(s.last) < Duration::from_secs(60));
            }
        }
    }
    drop(utterances_tx);
    let mut recogniser = recogniser;
    let _ = (&mut recogniser.0).await;
}

/// Aborts the task when dropped (the listener was stopped).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Queues an utterance for recognition, with who said it looked up now (client ids are reused).
async fn flush(
    resolve: &Resolve,
    id: PlatformId,
    samples: Vec<i16>,
    utterances: &mpsc::Sender<(Client, Vec<i16>, Instant)>,
) {
    if samples.len() < MIN_SAMPLES || utterances.capacity() == 0 {
        if samples.len() >= MIN_SAMPLES {
            tracing::debug!("speech recognition is behind, an utterance was dropped");
        }
        return;
    }
    let Some(client) = resolve(id).await else { return };
    if utterances.try_send((client, samples, Instant::now())).is_err() {
        tracing::debug!("speech recognition is behind, an utterance was dropped");
    }
}

/// Whisper tends to "hear" its prompt (the wake word) in silence.
fn only_prompt(text: &str, prompt: Option<&str>) -> bool {
    let words = |t: &str| -> String {
        t.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let text = words(text);
    text.is_empty() || prompt.is_some_and(|p| words(p) == text)
}

/// The command in `text` if it starts with the wake word (or something close to it starting
/// with the same letter: recognition often gets names slightly wrong, e.g. "Janus" or "Ja es"
/// for "James"), e.g. "James, play the next song." → `play the next song`.
pub fn wake_command(text: &str, wake_word: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let wake: String = wake_word
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("")
        .to_lowercase();
    if wake.is_empty() {
        return None;
    }
    let spoken_words = wake_word.split_whitespace().count();
    // Recognition may split a name ("Ja es" for "James") or join words: the first one to a
    // few words, joined, are compared with the wake word.
    (1..=spoken_words + 1)
        .filter(|&n| n < words.len())
        .find(|&n| similar(&wake, &words[..n].concat()))
        .map(|n| words[n..].join(" "))
}

/// Spoken words for the built-in commands (German; English ones are the command names).
const SPOKEN: &[(&str, &str)] = &[
    ("spiel", "play"),
    ("spiele", "play"),
    ("weiter", "skip"),
    ("nächster", "skip"),
    ("nächstes", "skip"),
    ("überspringen", "skip"),
    ("zurück", "prev"),
    ("stopp", "stop"),
    ("anhalten", "pause"),
    ("fortsetzen", "resume"),
    ("lautstärke", "volume"),
    ("leiser", "volume -10"),
    ("lauter", "volume +10"),
];

/// The chat command for a spoken one (without the prefix): a known spoken word becomes the
/// command; "bitte"/"please" at either end and "auf"/"um"/"to"/"by" before a number are left out.
pub fn spoken_command(command: &str) -> String {
    let filler = |w: &&str| matches!(*w, "bitte" | "please");
    let mut words: Vec<&str> = command.split_whitespace().collect();
    while words.len() > 1 && words.first().is_some_and(filler) {
        words.remove(0);
    }
    while words.len() > 1 && words.last().is_some_and(filler) {
        words.pop();
    }
    let Some((first, rest)) = words.split_first() else {
        return String::new();
    };
    let number = rest
        .iter()
        .skip_while(|w| matches!(**w, "auf" | "um" | "to" | "by"))
        .find(|w| w.parse::<u8>().is_ok());
    match (*first, number) {
        // "lauter 20" = 20 louder, "lautstärke auf 50" = volume 50.
        ("lauter", Some(amount)) => return format!("volume +{amount}"),
        ("leiser", Some(amount)) => return format!("volume -{amount}"),
        ("lautstärke" | "volume", Some(amount)) => return format!("volume {amount}"),
        _ => {}
    }
    match SPOKEN.iter().find(|(spoken, _)| spoken == first) {
        // A spoken word that already is a whole command ("lauter") takes no more words.
        Some((_, command)) if command.contains(' ') => (*command).to_owned(),
        Some((_, command)) => std::iter::once(*command)
            .chain(rest.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
        None => words.join(" "),
    }
}

/// Same word, allowing one recognition error (two in longer words); the first letter must match.
fn similar(expected: &str, heard: &str) -> bool {
    let allowed = match expected.chars().count() {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    };
    expected.chars().next() == heard.chars().next() && edit_distance(expected, heard) <= allowed
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let current = row[j + 1];
            row[j + 1] = (previous + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            previous = current;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use audiopus::coder::Encoder;
    use audiopus::Application;

    /// Records the length of every utterance it is asked to recognise.
    #[derive(Default)]
    struct Recorder(parking_lot::Mutex<Vec<usize>>);

    #[async_trait::async_trait]
    impl SttProvider for Recorder {
        fn id(&self) -> &str {
            "recorder"
        }
        async fn transcribe(
            &self,
            pcm: Vec<i16>,
            _: Option<String>,
            _: Option<String>,
        ) -> anyhow::Result<Transcript> {
            self.0.lock().push(pcm.len());
            Ok(Transcript {
                text: "heard".into(),
                language: None,
            })
        }
    }

    /// 20 ms Opus frames of a tone (speech-like for the segmenter: it only sees packets).
    fn frames(seconds: f32) -> Vec<Vec<u8>> {
        let encoder = Encoder::new(SampleRate::Hz16000, Channels::Mono, Application::Voip).unwrap();
        let count = (seconds * 50.0) as usize;
        (0..count)
            .map(|i| {
                let pcm: Vec<i16> = (0..320)
                    .map(|n| ((((i * 320 + n) as f32) * 0.05).sin() * 8000.0) as i16)
                    .collect();
                let mut out = vec![0u8; 400];
                let len = encoder.encode(&pcm, &mut out).unwrap();
                out.truncate(len);
                out
            })
            .collect()
    }

    /// Feeds each speaker's packets (in real time order is not needed: the pause decides),
    /// waits for the pause and returns the recognised utterance lengths.
    async fn segment(speech: Vec<(&str, Vec<Vec<u8>>, bool)>) -> (Vec<usize>, usize) {
        let recorder = Arc::new(Recorder::default());
        let (voice_tx, voice_rx) = mpsc::channel(4096);
        let (heard_tx, mut heard_rx) = mpsc::unbounded_channel();
        let resolve: Resolve = Arc::new(|id| {
            Box::pin(async move {
                Some(Client {
                    id,
                    ..Default::default()
                })
            })
        });
        let config = ListenerConfig {
            language: None,
            prompt: None,
        };
        let task = tokio::spawn(listen(recorder.clone(), config, resolve, voice_rx, heard_tx));
        for (client, packets, end) in speech {
            for data in packets {
                voice_tx
                    .send(VoicePacket {
                        client: client.into(),
                        data,
                    })
                    .await
                    .unwrap();
            }
            if end {
                voice_tx
                    .send(VoicePacket {
                        client: client.into(),
                        data: Vec::new(),
                    })
                    .await
                    .unwrap();
            }
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        drop(voice_tx);
        task.await.unwrap();
        let mut heard = 0;
        while heard_rx.try_recv().is_ok() {
            heard += 1;
        }
        let lengths = recorder.0.lock().clone();
        (lengths, heard)
    }

    #[tokio::test(start_paused = true)]
    async fn utterances_end_at_a_pause() {
        let (lengths, heard) = segment(vec![("1", frames(1.0), false), ("2", frames(0.6), true)]).await;
        let mut lengths = lengths;
        lengths.sort();
        assert_eq!(lengths, [9600, 16000], "one utterance per speaker");
        assert_eq!(heard, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn short_sounds_and_music_are_not_recognised() {
        // A click, and sound without any pause for longer than the longest utterance.
        let (lengths, _) = segment(vec![("1", frames(0.2), true), ("2", frames(16.0), true)]).await;
        assert!(lengths.is_empty(), "{lengths:?}");
    }

    #[test]
    fn wake_word() {
        assert_eq!(wake_command("Janes, skip", "James").as_deref(), Some("skip"));
        assert_eq!(wake_command("James skip", "james").as_deref(), Some("skip"));
        assert_eq!(
            wake_command("Hey James skip", "hey james").as_deref(),
            Some("skip")
        );
        assert_eq!(
            wake_command("Ja, es weiter.", "James").as_deref(),
            Some("weiter"),
            "a name split in two"
        );
        assert_eq!(
            wake_command("James", "James"),
            None,
            "nothing after the wake word"
        );
        assert_eq!(
            wake_command("Bob skip", "Bot"),
            None,
            "short names must match exactly"
        );
        for talk in [
            "Games are fun",
            "Jens, stopp mal",
            "Jana, weiter",
            "Ja das weiter",
            "Names matter here",
            "Hallo zusammen",
        ] {
            assert_eq!(wake_command(talk, "James"), None, "{talk}");
        }
        assert_eq!(spoken_command("bitte spiel thunderstruck"), "play thunderstruck");
        assert_eq!(
            spoken_command("spiel bitte bitte baby"),
            "play bitte bitte baby",
            "titles keep their words"
        );
        assert_eq!(spoken_command("leiser 20"), "volume -20");
        assert_eq!(spoken_command("lauter"), "volume +10");
        assert_eq!(spoken_command("lauter bitte"), "volume +10");
        assert_eq!(spoken_command("leiser um 20"), "volume -20");
        assert_eq!(spoken_command("lautstärke auf 50"), "volume 50");
        assert_eq!(spoken_command("weiter bitte"), "skip");
        assert!(only_prompt("James.", Some("James")));
        assert!(!only_prompt("James, weiter", Some("James")));
        assert_eq!(spoken_command("weiter"), "skip");
        assert_eq!(spoken_command("volume 30"), "volume 30");
    }
}
