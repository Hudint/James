//! Media sources (yt-dlp, local library, streams, …).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A playable item, independent of where it comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    /// Id of the resolver that produced this track.
    pub source: String,
    /// Resolver-specific locator (URL, library id, …), passed back to [`SourceResolver::open`].
    pub locator: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub thumbnail: Option<String>,
    /// Live streams have no end and no seeking.
    pub is_stream: bool,
}

/// What the decoder needs to read a track.
#[derive(Debug, Clone)]
pub enum MediaInput {
    File(PathBuf),
    Url {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

#[async_trait]
pub trait SourceResolver: Send + Sync {
    /// Unique id, e.g. `"ytdlp"`, `"local"`, `"stream"`.
    fn id(&self) -> &str;

    /// Whether this resolver understands the user input (URL, `search:` prefix, …).
    /// Resolvers are asked in priority order; the first match wins.
    fn accepts(&self, input: &str) -> bool;

    /// Turns user input into one or more tracks (a playlist URL yields many).
    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>>;

    /// Produces a decoder input right before playback (URLs may expire).
    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput>;
}

/// Ordered set of resolvers; the first resolver accepting an input handles it.
#[derive(Default, Clone)]
pub struct SourceRegistry {
    resolvers: Vec<(i32, std::sync::Arc<dyn SourceResolver>)>,
}

impl SourceRegistry {
    /// Registers a resolver. Lower `priority` values are asked first.
    pub fn register(&mut self, priority: i32, resolver: std::sync::Arc<dyn SourceResolver>) {
        self.resolvers.retain(|(_, r)| r.id() != resolver.id());
        self.resolvers.push((priority, resolver));
        self.resolvers.sort_by_key(|(p, _)| *p);
    }

    pub fn unregister(&mut self, id: &str) {
        self.resolvers.retain(|(_, r)| r.id() != id);
    }

    pub fn get(&self, id: &str) -> Option<&std::sync::Arc<dyn SourceResolver>> {
        self.resolvers.iter().map(|(_, r)| r).find(|r| r.id() == id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.resolvers.iter().map(|(_, r)| r.id().to_owned()).collect()
    }

    pub async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        let input = input.trim();
        let resolver = self
            .resolvers
            .iter()
            .map(|(_, r)| r)
            .find(|r| r.accepts(input))
            .ok_or_else(|| anyhow::anyhow!("no source can handle '{input}'"))?;
        resolver.resolve(input).await
    }

    pub async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        self.get(&track.source)
            .ok_or_else(|| anyhow::anyhow!("source '{}' is not available", track.source))?
            .open(track)
            .await
    }
}

/// Strips `prefix:` from an input, case-insensitively.
pub fn strip_scheme<'a>(input: &'a str, scheme: &str) -> Option<&'a str> {
    let (head, rest) = input.split_once(':')?;
    head.eq_ignore_ascii_case(scheme).then_some(rest.trim())
}

pub fn is_http_url(input: &str) -> bool {
    let lower = input.get(..8).unwrap_or(input).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fake(&'static str, &'static str);

    #[async_trait]
    impl SourceResolver for Fake {
        fn id(&self) -> &str {
            self.0
        }
        fn accepts(&self, input: &str) -> bool {
            input.starts_with(self.1)
        }
        async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
            Ok(vec![Track {
                source: self.0.into(),
                locator: input.into(),
                title: input.into(),
                artist: None,
                album: None,
                duration: None,
                thumbnail: None,
                is_stream: false,
            }])
        }
        async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
            Ok(MediaInput::File(track.locator.clone().into()))
        }
    }

    #[tokio::test]
    async fn priority_order_and_replacement() {
        let mut registry = SourceRegistry::default();
        registry.register(10, Arc::new(Fake("catchall", "")));
        registry.register(0, Arc::new(Fake("special", "x")));
        assert_eq!(registry.resolve("xyz").await.unwrap()[0].source, "special");
        assert_eq!(registry.resolve("abc").await.unwrap()[0].source, "catchall");
        registry.unregister("special");
        assert_eq!(registry.resolve("xyz").await.unwrap()[0].source, "catchall");
        assert_eq!(registry.ids(), vec!["catchall"]);
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_scheme("YT: foo bar", "yt"), Some("foo bar"));
        assert_eq!(strip_scheme("https://x", "yt"), None);
        assert!(is_http_url("HTTPS://example.com"));
        assert!(!is_http_url("/music/a.mp3"));
    }
}
