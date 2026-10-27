//! yt-dlp source: YouTube, SoundCloud, Bandcamp and ~1800 other sites, plus search.
//!
//! Inputs: any http(s) URL not claimed by a higher-priority source, `yt:<query>`
//! / `search:<query>` or plain text (YouTube search), `sc:<query>` (SoundCloud search).

use std::time::Duration;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use james_core::source::{is_http_url, strip_scheme, MediaInput, SourceResolver, Track};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use james_core::source::TempFile;

use tokio::process::Command;

use crate::net::NetworkPolicy;

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
            // Piped into ffmpeg: progressive MP4 (index at the end) cannot be read from a pipe.
            format: "bestaudio[acodec=opus]/bestaudio[ext=webm]/bestaudio/best[ext!=mp4]/best".into(),
            playlist_limit: 500,
            timeout_secs: 60,
            search_fallback: true,
        }
    }
}

pub struct YtDlpResolver {
    config: YtDlpConfig,
    network: NetworkPolicy,
    egress_proxy: Option<String>,
    /// Private directory for per-run cookie copies.
    work_dir: PathBuf,
}

impl YtDlpResolver {
    pub const ID: &'static str = "ytdlp";

    /// `egress_proxy` is used unless an explicit proxy is configured (a remote proxy cannot
    /// reach the host's private network, so it needs no local policy enforcement).
    /// `work_dir` must be a private directory (cookie copies are stored there).
    pub fn new(
        config: YtDlpConfig,
        network: NetworkPolicy,
        egress_proxy: Option<String>,
        work_dir: PathBuf,
    ) -> Self {
        Self {
            config,
            network,
            egress_proxy,
            work_dir,
        }
    }

    /// Common arguments; `cookies` is a private copy of the cookie file (see [`cookie_copy`]).
    fn base_args(&self, cookies: Option<&Path>) -> Vec<String> {
        let mut args: Vec<String> = [
            "--no-warnings",
            "--no-progress",
            "--ignore-config",
            "--no-cache-dir",
        ]
        .map(String::from)
        .into();
        if let Some(cookies) = cookies {
            args.extend(["--cookies".into(), cookies.display().to_string()]);
        }
        if let Some(pot) = &self.config.pot_provider {
            args.extend([
                "--extractor-args".into(),
                format!("youtubepot-bgutilhttp:base_url={pot}"),
            ]);
        }
        args.extend(self.config.extra_args.iter().cloned());
        args
    }

    /// Arguments of a download run writing the media to stdout. Only HTTP-based protocols
    /// are allowed, so every connection passes the egress proxy.
    fn download_args(&self, cookies: Option<&Path>, locator: &str) -> Vec<String> {
        let mut args = self.base_args(cookies);
        args.extend(["-q", "--no-playlist", "--no-part", "-f"].map(String::from));
        args.push(restrict_protocols(&self.config.format));
        args.extend(["-o", "-", "--", locator].map(String::from));
        args
    }

    /// Proxies go through the environment (not visible in the process list); `no_proxy` and
    /// other inherited settings are removed so the egress proxy cannot be bypassed.
    fn proxy(&self) -> Option<String> {
        self.config.proxy.clone().or_else(|| self.egress_proxy.clone())
    }

    /// Runs yt-dlp with `args` (after the common ones) and returns stdout.
    async fn run(&self, args: &[String]) -> anyhow::Result<Vec<u8>> {
        let cookies = cookie_copy(self.config.cookies.as_deref(), &self.work_dir).await?;
        let mut cmd = Command::new(&self.config.binary);
        cmd.args(self.base_args(cookies.as_deref().map(TempFile::path)))
            .args(args);
        james_core::process::apply_proxy_env(&mut cmd, self.proxy().as_deref());
        crate::run(cmd, self.timeout()).await
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

    /// Searches without resolving stream URLs (fast); returns up to `limit` results.
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 25);
        let out = self
            .run(&[
                "-J".into(),
                "--flat-playlist".into(),
                "--".into(),
                format!("ytsearch{limit}:{query}"),
            ])
            .await?;
        Ok(parse_info(&serde_json::from_slice(&out)?))
    }

    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        let mut target = self.target(input);
        if is_http_url(&target) {
            target = self.network.check(&target).await?;
        }
        let out = self
            .run(&[
                "-J".into(),
                "--flat-playlist".into(),
                "--playlist-end".into(),
                self.config.playlist_limit.to_string(),
                "--".into(),
                target.clone(),
            ])
            .await?;
        let info: Value = serde_json::from_slice(&out).context("invalid yt-dlp output")?;
        let tracks = parse_info(&info);
        if tracks.is_empty() {
            return Err(anyhow!("nothing found for '{input}'"));
        }
        Ok(tracks)
    }

    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        // Locators may come from stored playlists or plugins: re-validate them.
        if !is_http_url(&track.locator) {
            return Err(anyhow!("invalid locator"));
        }
        let locator = self.network.check(&track.locator).await?;
        // yt-dlp downloads and pipes the media itself: sites like YouTube only serve streams
        // correctly to it (ranged requests, tokens). Its traffic goes through the proxy.
        let cookies = cookie_copy(self.config.cookies.as_deref(), &self.work_dir).await?;
        let args = self.download_args(cookies.as_deref().map(TempFile::path), &locator);
        Ok(MediaInput::Process {
            program: self.config.binary.clone(),
            args,
            proxy: self.proxy(),
            temp_files: cookies.into_iter().collect(),
        })
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

/// Protocols yt-dlp may download with: HTTP based only, so every connection goes through the
/// egress proxy (yt-dlp would open `ftp://`, `rtmp://`, … media URLs directly).
const PROTOCOL_FILTER: &str = "[protocol~='^(https?|m3u8_native|http_dash_segments)$']";

/// Applies the protocol filter to the whole yt-dlp format expression (as a group, so it
/// also covers merges like `a+b` and nested groups).
pub fn restrict_protocols(format: &str) -> String {
    format!("({format}){PROTOCOL_FILTER}")
}

/// A private copy of the cookie file for one yt-dlp run. yt-dlp writes cookies back when it
/// exits, so concurrent runs on the shared file could corrupt it. Copies live in a private
/// work directory (0700, cleared at startup) and are deleted when the last handle is dropped.
async fn cookie_copy(source: Option<&Path>, work_dir: &Path) -> anyhow::Result<Option<Arc<TempFile>>> {
    let Some(source) = source else { return Ok(None) };
    let path = work_dir.join(format!("cookies-{:016x}.txt", rand::random::<u64>()));
    let content = tokio::fs::read(source)
        .await
        .with_context(|| format!("cannot read {}", source.display()))?;
    let mut options = tokio::fs::OpenOptions::new();
    // `create_new` never follows or reuses an existing path.
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .await
        .context("cannot create the cookie copy")?;
    // From here on the guard deletes the file, also if writing fails.
    let guard = Arc::new(TempFile::new(path));
    tokio::io::AsyncWriteExt::write_all(&mut file, &content).await?;
    Ok(Some(guard))
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

    #[tokio::test]
    async fn cookie_copies_are_private_and_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("cookies.txt");
        std::fs::write(&source, "# Netscape HTTP Cookie File\n").unwrap();
        let a = cookie_copy(Some(&source), dir.path()).await.unwrap().unwrap();
        let b = cookie_copy(Some(&source), dir.path()).await.unwrap().unwrap();
        assert_ne!(a.path(), b.path(), "one copy per run");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let path = a.path().to_path_buf();
        let shared = a.clone();
        drop(a);
        assert!(path.exists(), "kept while a handle exists");
        drop(shared);
        assert!(!path.exists(), "deleted with the last handle");
        assert!(cookie_copy(None, dir.path()).await.unwrap().is_none());
    }

    #[test]
    fn downloads_use_the_protocol_filter_and_end_options() {
        let r = YtDlpResolver::new(
            YtDlpConfig::default(),
            NetworkPolicy::default(),
            None,
            std::env::temp_dir(),
        );
        let args = r.download_args(None, "https://www.youtube.com/watch?v=x");
        let format = &args[args.iter().position(|a| a == "-f").unwrap() + 1];
        assert_eq!(
            format,
            &format!("({}){PROTOCOL_FILTER}", YtDlpConfig::default().format)
        );
        assert_eq!(
            &args[args.len() - 2..],
            ["--", "https://www.youtube.com/watch?v=x"]
        );
    }

    #[test]
    fn formats_are_limited_to_http_protocols() {
        assert_eq!(
            restrict_protocols("bv+ba/best"),
            format!("(bv+ba/best){PROTOCOL_FILTER}")
        );
    }

    #[test]
    fn input_mapping() {
        let r = YtDlpResolver::new(
            YtDlpConfig::default(),
            NetworkPolicy::default(),
            None,
            std::env::temp_dir(),
        );
        assert_eq!(r.target("yt: never gonna"), "ytsearch1:never gonna");
        assert_eq!(r.target("sc:lofi"), "scsearch1:lofi");
        assert_eq!(r.target("https://youtu.be/x"), "https://youtu.be/x");
        assert_eq!(r.target("some song"), "ytsearch1:some song");
        assert!(r.accepts("some song"));
        assert!(!r.accepts("file:///music"));
    }
}
