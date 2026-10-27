//! yt-dlp source: YouTube, SoundCloud, Bandcamp and ~1800 other sites, plus search.
//!
//! Inputs: any http(s) URL not claimed by a higher-priority source, `yt:<query>`
//! / `search:<query>` or plain text (YouTube search), `sc:<query>` (SoundCloud search).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use james_core::source::{is_http_url, strip_scheme, MediaInput, SourceResolver, Track};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YtDlpConfig {
    pub binary: String,
    /// Netscape cookie file, required for YouTube on many datacenter IPs.
    pub cookies: Option<PathBuf>,
    /// Proxy URL, e.g. `socks5://user:pass@host:1080`.
    pub proxy: Option<String>,
    /// Base URL of a bgutil PO token provider (e.g. `http://bgutil:4416`).
    pub pot_provider: Option<String>,
    /// Additional raw arguments.
    pub extra_args: Vec<String>,
    /// Audio format selection.
    pub format: String,
    /// Maximum playlist entries resolved from one URL.
    pub playlist_limit: usize,
    pub timeout_secs: u64,
    /// Accept free text (no prefix, no URL) as YouTube search.
    pub search_fallback: bool,
}

impl Default for YtDlpConfig {
    fn default() -> Self {
        Self {
            binary: "yt-dlp".into(),
            cookies: None,
            proxy: None,
            pot_provider: None,
            extra_args: Vec::new(),
            format: "bestaudio[acodec=opus]/bestaudio/best".into(),
            playlist_limit: 500,
            timeout_secs: 60,
            search_fallback: true,
        }
    }
}

pub struct YtDlpResolver {
    config: YtDlpConfig,
}

impl YtDlpResolver {
    pub const ID: &'static str = "ytdlp";

    pub fn new(config: YtDlpConfig) -> Self {
        Self { config }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.config.binary);
        cmd.args([
            "--no-warnings",
            "--no-progress",
            "--ignore-config",
            "--no-cache-dir",
        ]);
        if let Some(cookies) = &self.config.cookies {
            cmd.arg("--cookies").arg(cookies);
        }
        if let Some(proxy) = &self.config.proxy {
            cmd.arg("--proxy").arg(proxy);
        }
        if let Some(pot) = &self.config.pot_provider {
            cmd.arg("--extractor-args")
                .arg(format!("youtubepot-bgutilhttp:base_url={pot}"));
        }
        cmd.args(&self.config.extra_args);
        cmd
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs)
    }

    /// Maps user input to a yt-dlp URL/search expression.
    fn target(&self, input: &str) -> String {
        if let Some(q) = strip_scheme(input, "yt").or_else(|| strip_scheme(input, "search")) {
            format!("ytsearch1:{q}")
        } else if let Some(q) = strip_scheme(input, "sc") {
            format!("scsearch1:{q}")
        } else if is_http_url(input) {
            input.to_owned()
        } else {
            format!("ytsearch1:{input}")
        }
    }

    /// Searches without resolving stream URLs (fast); returns up to `limit` results.
    pub async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let mut cmd = self.command();
        cmd.args(["-J", "--flat-playlist"])
            .arg(format!("ytsearch{limit}:{query}"));
        let out = crate::run(cmd, self.timeout()).await?;
        Ok(parse_info(&serde_json::from_slice(&out)?))
    }
}

#[async_trait]
impl SourceResolver for YtDlpResolver {
    fn id(&self) -> &str {
        Self::ID
    }

    fn accepts(&self, input: &str) -> bool {
        is_http_url(input)
            || ["yt", "search", "sc"]
                .iter()
                .any(|s| strip_scheme(input, s).is_some())
            || (self.config.search_fallback && !input.is_empty() && !input.contains("://"))
    }

    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        let mut cmd = self.command();
        cmd.args(["-J", "--flat-playlist", "--playlist-end"])
            .arg(self.config.playlist_limit.to_string())
            .arg(self.target(input));
        let out = crate::run(cmd, self.timeout()).await?;
        let info: Value = serde_json::from_slice(&out).context("invalid yt-dlp output")?;
        let tracks = parse_info(&info);
        if tracks.is_empty() {
            return Err(anyhow!("nothing found for '{input}'"));
        }
        Ok(tracks)
    }

    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        let mut cmd = self.command();
        cmd.args(["-J", "--no-playlist", "-f", &self.config.format])
            .arg(&track.locator);
        let out = crate::run(cmd, self.timeout()).await?;
        let info: Value = serde_json::from_slice(&out).context("invalid yt-dlp output")?;
        let url = info
            .get("url")
            .and_then(Value::as_str)
            .or_else(|| {
                // Some extractors report the chosen format only in `requested_formats`.
                info.pointer("/requested_formats/0/url").and_then(Value::as_str)
            })
            .ok_or_else(|| anyhow!("yt-dlp returned no stream URL"))?
            .to_owned();
        let headers = info
            .get("http_headers")
            .and_then(Value::as_object)
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(MediaInput::Url { url, headers })
    }
}

/// Converts yt-dlp JSON (single video or flat playlist) into tracks.
pub fn parse_info(info: &Value) -> Vec<Track> {
    match info.get("_type").and_then(Value::as_str) {
        Some("playlist") => info
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(parse_entry).collect())
            .unwrap_or_default(),
        _ => parse_entry(info).into_iter().collect(),
    }
}

fn parse_entry(entry: &Value) -> Option<Track> {
    let str_field = |name: &str| entry.get(name).and_then(Value::as_str).map(str::to_owned);
    let locator = str_field("webpage_url")
        .or_else(|| str_field("url"))
        .or_else(|| {
            // Flat YouTube entries may only carry an id.
            let id = str_field("id")?;
            (str_field("ie_key").as_deref() == Some("Youtube"))
                .then(|| format!("https://www.youtube.com/watch?v={id}"))
        })?;
    let thumbnail = str_field("thumbnail").or_else(|| {
        entry
            .get("thumbnails")
            .and_then(Value::as_array)
            .and_then(|t| t.last())
            .and_then(|t| t.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let is_live = entry.get("is_live").and_then(Value::as_bool).unwrap_or(false)
        || str_field("live_status").as_deref() == Some("is_live");
    Some(Track {
        source: YtDlpResolver::ID.into(),
        title: str_field("title").unwrap_or_else(|| locator.clone()),
        artist: str_field("artist")
            .or_else(|| str_field("uploader"))
            .or_else(|| str_field("channel")),
        album: str_field("album"),
        duration: entry
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|d| *d > 0.0)
            .map(Duration::from_secs_f64),
        thumbnail,
        is_stream: is_live,
        locator,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_single_video() {
        let tracks = parse_info(&json!({
            "id": "abc", "title": "Song", "uploader": "Artist", "duration": 213.5,
            "webpage_url": "https://www.youtube.com/watch?v=abc",
            "thumbnail": "https://i.ytimg.com/abc.jpg"
        }));
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.title, "Song");
        assert_eq!(t.artist.as_deref(), Some("Artist"));
        assert_eq!(t.duration, Some(Duration::from_secs_f64(213.5)));
        assert_eq!(t.locator, "https://www.youtube.com/watch?v=abc");
        assert!(!t.is_stream);
    }

    #[test]
    fn parses_flat_playlist() {
        let tracks = parse_info(&json!({
            "_type": "playlist", "title": "List",
            "entries": [
                {"_type": "url", "ie_key": "Youtube", "id": "a1", "url": "https://www.youtube.com/watch?v=a1", "title": "One"},
                {"_type": "url", "ie_key": "Youtube", "id": "a2", "title": "Two", "live_status": "is_live"},
                {"_type": "url", "title": "No locator"}
            ]
        }));
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1].locator, "https://www.youtube.com/watch?v=a2");
        assert!(tracks[1].is_stream);
    }

    #[test]
    fn input_mapping() {
        let r = YtDlpResolver::new(YtDlpConfig::default());
        assert_eq!(r.target("yt: never gonna"), "ytsearch1:never gonna");
        assert_eq!(r.target("sc:lofi"), "scsearch1:lofi");
        assert_eq!(r.target("https://youtu.be/x"), "https://youtu.be/x");
        assert_eq!(r.target("some song"), "ytsearch1:some song");
        assert!(r.accepts("some song"));
        assert!(!r.accepts("file:///music"));
    }
}
