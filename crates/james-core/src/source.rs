//! Media sources (yt-dlp, local library, streams, …).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A playable item, independent of where it comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Id of the resolver that produced this track.
    pub source: String,
    /// Resolver-specific locator (URL, library id, …), passed back to [`SourceResolver::open`].
    pub locator: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[serde(rename = "durationMs", with = "duration_ms", default)]
    pub duration: Option<Duration>,
    pub thumbnail: Option<String>,
    /// Live streams have no end and no seeking.
    pub is_stream: bool,
}

/// Hosts whose thumbnails viewers' browsers may load (avoids tracking pixels in
/// client-supplied tracks).
const THUMBNAIL_HOSTS: &[&str] = &["ytimg.com", "ggpht.com", "sndcdn.com", "bcbits.com"];
const MAX_TEXT: usize = 200;
const MAX_URL: usize = 2048;

impl Track {
    /// Rejects client-supplied tracks with oversized fields (before [`Track::sanitized`]).
    pub fn check_client_limits(&self) -> anyhow::Result<()> {
        let too_long = self.locator.len() > MAX_URL
            || self.source.len() > 64
            || self.thumbnail.as_ref().is_some_and(|t| t.len() > MAX_URL)
            || [Some(&self.title), self.artist.as_ref(), self.album.as_ref()]
                .into_iter()
                .flatten()
                .any(|s| s.len() > 4 * MAX_TEXT);
        if too_long {
            anyhow::bail!("track fields are too long");
        }
        Ok(())
    }

    /// Bounds a track received from an API client, plugin or website: texts are cut,
    /// thumbnails only from known media CDNs, durations at most a day. The locator is
    /// validated separately by the source when the track is opened.
    pub fn sanitized(mut self) -> Self {
        let cut = |s: &mut String| {
            if s.chars().count() > MAX_TEXT {
                *s = s.chars().take(MAX_TEXT).collect();
            }
        };
        cut(&mut self.title);
        for field in [&mut self.artist, &mut self.album].into_iter().flatten() {
            cut(field);
        }
        self.thumbnail = self.thumbnail.filter(|url| {
            url::Url::parse(url).is_ok_and(|u| {
                u.scheme() == "https"
                    && u.host_str().is_some_and(|h| {
                        THUMBNAIL_HOSTS
                            .iter()
                            .any(|allowed| h == *allowed || h.ends_with(&format!(".{allowed}")))
                    })
            })
        });
        self.duration = self.duration.filter(|d| d.as_secs() <= 24 * 3600);
        self
    }
}

/// (De)serialises an optional duration as integer milliseconds.
mod duration_ms {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(d) => serializer.serialize_some(&(d.as_millis() as u64)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<u64>::deserialize(deserializer)?.map(Duration::from_millis))
    }
}

/// What the decoder needs to read a track.
#[derive(Debug, Clone)]
pub enum MediaInput {
    File(PathBuf),
    Url {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// Media data written to stdout by a helper process, e.g. yt-dlp downloading a video
    /// (sites like YouTube only serve their streams correctly to their own downloader).
    /// Arguments are built by the source, never taken from user input.
    Process {
        program: String,
        args: Vec<String>,
        /// Proxy for the helper (egress proxy or a configured remote proxy).
        proxy: Option<String>,
        /// Files the process needs (e.g. a private cookie copy); deleted when the last
        /// handle is dropped, so a restart after seeking can still use them.
        temp_files: Vec<std::sync::Arc<TempFile>>,
    },
}

/// A file deleted when it is dropped (share it via `Arc` to keep it alive).
#[derive(Debug)]
pub struct TempFile(PathBuf);

impl TempFile {
    /// Takes ownership of an existing file.
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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

    /// Free-text search returning candidates to choose from (without queueing them).
    /// Sources that cannot search return an error.
    async fn search(&self, _query: &str, _limit: usize) -> anyhow::Result<Vec<Track>> {
        anyhow::bail!("source '{}' does not support search", self.id())
    }

    /// Saves the track's audio as a file in `dir` (an empty, private directory) for the local
    /// library and returns its path. Sources that cannot save tracks return an error.
    async fn download(&self, track: &Track, _dir: &std::path::Path) -> anyhow::Result<PathBuf> {
        anyhow::bail!("tracks from '{}' cannot be saved to the library", track.source)
    }

    /// Key under which analysis results (loudness) of the track are cached; it must change
    /// when the content changes. `None` disables caching.
    async fn cache_key(&self, track: &Track) -> Option<String> {
        Some(track.locator.clone())
    }
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
        // Metadata from external sites is bounded like client input.
        Ok(resolver
            .resolve(input)
            .await?
            .into_iter()
            .map(Track::sanitized)
            .collect())
    }

    /// Searches with the first registered source that supports search.
    pub async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let mut last = anyhow::anyhow!("no source supports search");
        for (_, resolver) in &self.resolvers {
            match resolver.search(query, limit).await {
                Ok(tracks) => return Ok(tracks.into_iter().map(Track::sanitized).collect()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    pub async fn cache_key(&self, track: &Track) -> Option<String> {
        self.get(&track.source)?.cache_key(track).await
    }

    pub async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        self.get(&track.source)
            .ok_or_else(|| anyhow::anyhow!("source '{}' is not available", track.source))?
            .open(track)
            .await
    }

    /// See [`SourceResolver::download`].
    pub async fn download(&self, track: &Track, dir: &std::path::Path) -> anyhow::Result<PathBuf> {
        self.get(&track.source)
            .ok_or_else(|| anyhow::anyhow!("source '{}' is not available", track.source))?
            .download(track, dir)
            .await
    }
}

/// Input for YouTube commands (`!yt`, `!ytdl`): links and inputs with a source prefix
/// (`yt:`, `sc:`, `search:`) as they are, other text as a YouTube search.
pub fn youtube_input(input: &str) -> String {
    let input = input.trim();
    let prefixed = ["yt", "sc", "search"]
        .iter()
        .any(|p| strip_scheme(input, p).is_some());
    if is_http_url(input) || prefixed {
        input.to_owned()
    } else {
        format!("yt:{input}")
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

    #[test]
    fn youtube_commands_search_youtube_unless_told_otherwise() {
        assert_eq!(
            youtube_input("queen: bohemian rhapsody"),
            "yt:queen: bohemian rhapsody"
        );
        assert_eq!(youtube_input("https://youtu.be/x"), "https://youtu.be/x");
        assert_eq!(youtube_input("sc:forss"), "sc:forss");
        assert_eq!(youtube_input(" yt:song "), "yt:song");
    }

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
    fn track_json_uses_camel_case_and_milliseconds() {
        let track = Track {
            source: "s".into(),
            locator: "l".into(),
            title: "t".into(),
            artist: None,
            album: None,
            duration: Some(std::time::Duration::from_millis(1500)),
            thumbnail: None,
            is_stream: true,
        };
        let json = serde_json::to_value(&track).unwrap();
        assert_eq!(json["durationMs"], 1500);
        assert_eq!(json["isStream"], true);
        assert_eq!(serde_json::from_value::<Track>(json).unwrap(), track);
        let minimal: Track = serde_json::from_value(serde_json::json!({
            "source": "s", "locator": "l", "title": "t", "artist": null, "album": null,
            "thumbnail": null, "isStream": false
        }))
        .unwrap();
        assert_eq!(minimal.duration, None);
    }

    #[test]
    fn client_tracks_are_sanitized() {
        let track = Track {
            source: "ytdlp".into(),
            locator: "https://www.youtube.com/watch?v=x".into(),
            title: "t".repeat(500),
            artist: Some("a".into()),
            album: None,
            duration: Some(std::time::Duration::from_secs(10 * 24 * 3600)),
            thumbnail: Some("https://attacker.example/pixel.gif".into()),
            is_stream: false,
        }
        .sanitized();
        assert_eq!(track.title.chars().count(), 200);
        assert_eq!(track.thumbnail, None);
        assert_eq!(track.duration, None);
        let ok = Track {
            thumbnail: Some("https://i.ytimg.com/vi/x/hq.jpg".into()),
            ..track
        }
        .sanitized();
        assert!(ok.thumbnail.is_some());
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_scheme("YT: foo bar", "yt"), Some("foo bar"));
        assert_eq!(strip_scheme("https://x", "yt"), None);
        assert!(is_http_url("HTTPS://example.com"));
        assert!(!is_http_url("/music/a.mp3"));
    }
}
