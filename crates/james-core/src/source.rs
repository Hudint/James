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
