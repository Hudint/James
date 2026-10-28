//! Text to speech: providers (Piper, …) behind one registry.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::source::{MediaInput, TempFile};

/// Longest text spoken at once (characters).
pub const MAX_TEXT_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Voice {
    /// Unique id, e.g. `de_DE-thorsten-medium`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// BCP 47 language tag, e.g. `de-DE`.
    pub language: String,
    /// Provider id, e.g. `piper`.
    pub provider: String,
}

/// Synthesized speech, ready for the audio engine.
pub struct Speech {
    pub input: MediaInput,
    /// Kept alive until the speech was played (deleted afterwards).
    pub temp: Option<Arc<TempFile>>,
    /// Integrated loudness (LUFS), if the provider measured it.
    pub loudness: Option<f32>,
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    fn id(&self) -> &str;
    fn voices(&self) -> Vec<Voice>;
    async fn synthesize(&self, text: &str, voice: &str) -> anyhow::Result<Speech>;
}

/// All TTS providers; picks voices by id or language.
#[derive(Clone, Default)]
pub struct TtsRegistry {
    providers: Vec<Arc<dyn TtsProvider>>,
    default_voice: Option<String>,
}

/// `de_DE` / `de-de` → `de-DE`.
pub fn normalize_language(tag: &str) -> String {
    let mut parts = tag.split(['-', '_']);
    let language = parts.next().unwrap_or_default().to_ascii_lowercase();
    let rest: Vec<String> = parts
        .map(|p| {
            if p.len() == 2 {
                p.to_ascii_uppercase()
            } else {
                p.to_owned()
            }
        })
        .collect();
    std::iter::once(language)
        .chain(rest)
        .collect::<Vec<_>>()
        .join("-")
}

impl TtsRegistry {
    pub fn register(&mut self, provider: Arc<dyn TtsProvider>) {
        self.providers.push(provider);
    }

    /// Voice used when nothing else is asked for.
    pub fn set_default_voice(&mut self, voice: Option<String>) {
        self.default_voice = voice;
    }

    pub fn is_empty(&self) -> bool {
        self.voices().is_empty()
    }

    pub fn voices(&self) -> Vec<Voice> {
        let mut voices: Vec<Voice> = self.providers.iter().flat_map(|p| p.voices()).collect();
        voices.sort_by(|a, b| a.language.cmp(&b.language).then(a.id.cmp(&b.id)));
        voices
    }

    /// The voice for a request: an explicit voice id, else the best match for `language`
    /// (exact tag, then the same language family; the default voice wins ties), else the
    /// default voice.
    pub fn pick(&self, voice: Option<&str>, language: Option<&str>) -> anyhow::Result<Voice> {
        let voices = self.voices();
        if let Some(id) = voice.filter(|v| !v.is_empty()) {
            return voices
                .into_iter()
                .find(|v| v.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown voice '{id}'"));
        }
        let default = self
            .default_voice
            .as_deref()
            .and_then(|id| voices.iter().find(|v| v.id == id))
            .or_else(|| voices.first())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no text-to-speech voice is installed"))?;
        let Some(language) = language.filter(|l| !l.is_empty()) else {
            return Ok(default);
        };
        let wanted = normalize_language(language);
        let family = wanted.split('-').next().unwrap_or_default().to_owned();
        let score = |v: &Voice| {
            let tag = normalize_language(&v.language);
            if tag == wanted {
                2
            } else if tag.split('-').next() == Some(family.as_str()) {
                1
            } else {
                0
            }
        };
        let best = voices.iter().map(&score).max().unwrap_or(0);
        if best == 0 {
            return Ok(default);
        }
        if score(&default) == best {
            return Ok(default);
        }
        Ok(voices
            .into_iter()
            .find(|v| score(v) == best)
            .expect("a voice has the best score"))
    }

    /// Checks a request and picks its voice (fails fast, before the slow synthesis).
    pub fn prepare(
        &self,
        text: &str,
        voice: Option<&str>,
        language: Option<&str>,
    ) -> anyhow::Result<(String, Voice)> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "nothing to say");
        anyhow::ensure!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "text longer than {MAX_TEXT_CHARS} characters"
        );
        Ok((text.to_owned(), self.pick(voice, language)?))
    }

    /// Synthesizes a prepared request.
    pub async fn speak(&self, text: &str, voice: &Voice) -> anyhow::Result<Speech> {
        let provider = self
            .providers
            .iter()
            .find(|p| p.id() == voice.provider)
            .ok_or_else(|| anyhow::anyhow!("provider '{}' is missing", voice.provider))?;
        provider.synthesize(text, &voice.id).await
    }

    /// Synthesizes `text` with the picked voice.
    pub async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        language: Option<&str>,
    ) -> anyhow::Result<(Voice, Speech)> {
        let (text, voice) = self.prepare(text, voice, language)?;
        let speech = self.speak(&text, &voice).await?;
        Ok((voice, speech))
    }

    /// Whether a voice with this id is installed.
    pub fn has_voice(&self, id: &str) -> bool {
        self.providers
            .iter()
            .any(|p| p.voices().iter().any(|v| v.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Vec<(&'static str, &'static str)>);

    #[async_trait]
    impl TtsProvider for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn voices(&self) -> Vec<Voice> {
            self.0
                .iter()
                .map(|(id, language)| Voice {
                    id: (*id).into(),
                    name: (*id).into(),
                    language: (*language).into(),
                    provider: "fake".into(),
                })
                .collect()
        }
        async fn synthesize(&self, _: &str, _: &str) -> anyhow::Result<Speech> {
            Ok(Speech {
                input: MediaInput::File("x.wav".into()),
                temp: None,
                loudness: None,
            })
        }
    }

    fn registry(default: Option<&str>) -> TtsRegistry {
        let mut registry = TtsRegistry::default();
        registry.register(Arc::new(Fake(vec![
            ("thorsten", "de_DE"),
            ("kerstin", "de_DE"),
            ("lessac", "en_US"),
            ("alba", "en_GB"),
        ])));
        registry.set_default_voice(default.map(Into::into));
        registry
    }

    #[test]
    fn languages_are_normalized() {
        assert_eq!(normalize_language("de_DE"), "de-DE");
        assert_eq!(normalize_language("EN-us"), "en-US");
        assert_eq!(normalize_language("de"), "de");
    }

    #[test]
    fn voices_are_picked_by_id_language_and_default() {
        let r = registry(Some("kerstin"));
        assert_eq!(r.pick(None, None).unwrap().id, "kerstin");
        assert_eq!(
            r.pick(Some("lessac"), Some("de")).unwrap().id,
            "lessac",
            "explicit voice wins"
        );
        assert!(r.pick(Some("nope"), None).is_err());
        assert_eq!(
            r.pick(None, Some("de-DE")).unwrap().id,
            "kerstin",
            "default wins among equals"
        );
        assert_eq!(r.pick(None, Some("en-GB")).unwrap().id, "alba", "exact region");
        assert_eq!(r.pick(None, Some("en")).unwrap().id, "alba", "same family");
        assert_eq!(
            r.pick(None, Some("fr")).unwrap().id,
            "kerstin",
            "unknown language: default"
        );
        assert!(TtsRegistry::default().pick(None, None).is_err());
    }

    #[tokio::test]
    async fn text_limits() {
        let r = registry(None);
        assert!(r.synthesize("  ", None, None).await.is_err());
        assert!(r
            .synthesize(&"a".repeat(MAX_TEXT_CHARS + 1), None, None)
            .await
            .is_err());
        assert!(r.synthesize("hello", None, None).await.is_ok());
    }
}
