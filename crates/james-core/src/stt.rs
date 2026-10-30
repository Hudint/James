//! Speech to text: providers (Whisper, …) that recognise what clients say in a channel.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

/// Audio handed to providers: 16 kHz mono, signed 16-bit samples.
pub const SAMPLE_RATE: u32 = 16_000;
/// Longest a caller waits for a recognition (queueing included): later it is too late to act on.
pub const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Languages Whisper recognises (ISO 639-1).
pub const LANGUAGES: &[&str] = &[
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "cs", "cy", "da", "de",
    "el", "en", "es", "et", "eu", "fa", "fi", "fo", "fr", "gl", "gu", "ha", "he", "hi", "hr", "ht", "hu",
    "hy", "id", "is", "it", "ja", "jw", "ka", "kk", "km", "kn", "ko", "la", "lb", "ln", "lo", "lt", "lv",
    "mg", "mi", "mk", "ml", "mn", "mr", "ms", "mt", "my", "ne", "nl", "nn", "no", "oc", "pa", "pl", "ps",
    "pt", "ro", "ru", "sa", "sd", "si", "sk", "sl", "sn", "so", "sq", "sr", "su", "sv", "sw", "ta", "te",
    "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz", "vi", "yi", "yo", "zh",
];

/// Longest utterance recognised at once (15 s); sound without a pause for longer is not speech
/// (music, a microphone that always transmits) and is not recognised.
pub const MAX_SAMPLES: usize = SAMPLE_RATE as usize * 15;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transcript {
    pub text: String,
    /// Detected (or requested) language, e.g. `de`.
    pub language: Option<String>,
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    fn id(&self) -> &str;
    /// Gets ready in the background (e.g. loads the model) because an instance will listen.
    fn prepare(&self) {}
    /// Recognises one utterance. `language` (ISO 639-1, e.g. `de`) skips detection; `prompt`
    /// makes words such as the bot's name more likely.
    async fn transcribe(
        &self,
        pcm: Vec<i16>,
        language: Option<String>,
        prompt: Option<String>,
    ) -> anyhow::Result<Transcript>;
}

/// All speech recognition providers.
#[derive(Clone, Default)]
pub struct SttRegistry {
    providers: Vec<Arc<dyn SttProvider>>,
}

impl SttRegistry {
    pub fn register(&mut self, provider: Arc<dyn SttProvider>) {
        self.providers.push(provider);
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The provider with this id, or the first one (`None`).
    pub fn get(&self, id: Option<&str>) -> Option<Arc<dyn SttProvider>> {
        match id {
            Some(id) => self.providers.iter().find(|p| p.id() == id).cloned(),
            None => self.providers.first().cloned(),
        }
    }
}
