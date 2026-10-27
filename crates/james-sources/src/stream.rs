//! Internet radio and direct streams (Icecast/Shoutcast, HLS, direct audio URLs, M3U/PLS).
//!
//! Inputs: `stream:<url>` / `radio:<url>`, or http(s) URLs that look like a
//! stream or audio file (by extension).

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use james_core::source::{is_http_url, strip_scheme, MediaInput, SourceResolver, Track};

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "aac", "m4a", "ogg", "oga", "opus", "flac", "wav"];
const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "pls"];
const HLS_EXTENSIONS: &[&str] = &["m3u8"];

pub struct StreamResolver {
    http: reqwest::Client,
}

impl Default for StreamResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamResolver {
    pub const ID: &'static str = "stream";

    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("James/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("valid http client");
        Self { http }
    }

    fn url_of(input: &str) -> Option<&str> {
        strip_scheme(input, "stream")
            .or_else(|| strip_scheme(input, "radio"))
            .or_else(|| {
                let ext = extension(input)?;
                let known = AUDIO_EXTENSIONS
                    .iter()
                    .chain(PLAYLIST_EXTENSIONS)
                    .chain(HLS_EXTENSIONS);
                (is_http_url(input) && known.into_iter().any(|e| *e == ext)).then_some(input)
            })
    }

    /// Reads the station name from Icecast/Shoutcast headers without downloading the stream.
    async fn station_name(&self, url: &str) -> Option<String> {
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            self.http.get(url).header("Icy-MetaData", "1").send(),
        )
        .await
        .ok()?
        .ok()?;
        let name = response
            .headers()
            .get("icy-name")?
            .to_str()
            .ok()?
            .trim()
            .to_owned();
        (!name.is_empty()).then_some(name)
    }
}

fn extension(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_owned();
    let (_, ext) = path.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

/// Extracts stream URLs from M3U or PLS playlist content.
pub fn parse_playlist(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            let candidate = match line.split_once('=') {
                Some((key, value)) if key.to_ascii_lowercase().starts_with("file") => value.trim(),
                _ => line,
            };
            is_http_url(candidate).then(|| candidate.to_owned())
        })
        .collect()
}

#[async_trait]
impl SourceResolver for StreamResolver {
    fn id(&self) -> &str {
        Self::ID
    }

    fn accepts(&self, input: &str) -> bool {
        Self::url_of(input).is_some_and(is_http_url)
    }

    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        let mut url = Self::url_of(input)
            .ok_or_else(|| anyhow!("not a stream URL"))?
            .to_owned();
        if extension(&url).is_some_and(|e| PLAYLIST_EXTENSIONS.contains(&e.as_str())) {
            let body = self
                .http
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            url = parse_playlist(&body)
                .into_iter()
                .next()
                .context("playlist contains no stream URL")?;
        }
        let is_file = extension(&url).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()));
        let name = self.station_name(&url).await;
        let title = name.clone().unwrap_or_else(|| {
            url::Url::parse(&url)
                .ok()
                .and_then(|u| u.path_segments()?.next_back().map(str::to_owned))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| url.clone())
        });
        Ok(vec![Track {
            source: Self::ID.into(),
            locator: url,
            title,
            artist: None,
            album: None,
            duration: None,
            thumbnail: None,
            // Icecast streams report a station name; plain files do not.
            is_stream: name.is_some() || !is_file,
        }])
    }

    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        Ok(MediaInput::Url {
            url: track.locator.clone(),
            headers: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_streams_only() {
        let r = StreamResolver::new();
        assert!(r.accepts("https://radio.example.com/live.mp3"));
        assert!(r.accepts("https://radio.example.com/list.pls?x=1"));
        assert!(r.accepts("radio: https://radio.example.com/live"));
        assert!(!r.accepts("https://www.youtube.com/watch?v=x"));
        assert!(!r.accepts("radio: not a url"));
    }

    #[test]
    fn parses_m3u_and_pls() {
        let m3u = "#EXTM3U\n#EXTINF:-1,Station\nhttp://a.example/stream\n";
        assert_eq!(parse_playlist(m3u), vec!["http://a.example/stream"]);
        let pls =
            "[playlist]\nNumberOfEntries=2\nFile1=http://b.example/1\nTitle1=X\nFile2=http://b.example/2\n";
        assert_eq!(
            parse_playlist(pls),
            vec!["http://b.example/1", "http://b.example/2"]
        );
    }
}
