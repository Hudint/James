//! Internet radio and direct streams (Icecast/Shoutcast, HLS, direct audio URLs, M3U/PLS).
//!
//! Inputs: `stream:<url>` / `radio:<url>`, or http(s) URLs that look like a
//! stream or audio file (by extension).

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use james_core::media::AUDIO_EXTENSIONS;
use james_core::source::{is_http_url, strip_scheme, MediaInput, SourceResolver, Track};

use crate::net::NetworkPolicy;

const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "pls"];
const HLS_EXTENSIONS: &[&str] = &["m3u8"];
/// Playlist files larger than this are rejected (they are a few hundred bytes in practice).
const MAX_PLAYLIST_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct StreamResolver {
    http: reqwest::Client,
    network: NetworkPolicy,
}

impl StreamResolver {
    pub const ID: &'static str = "stream";

    /// All requests go through `egress_proxy` when given (see [`crate::egress`]).
    pub fn new(network: NetworkPolicy, egress_proxy: Option<&str>) -> Self {
        let redirect_policy = network.clone();
        let mut builder = reqwest::Client::builder();
        // Fail closed: without the proxy only the first hop would be checked.
        builder = builder.no_proxy();
        if let Some(proxy) = egress_proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy).expect("valid egress proxy URL"));
        }
        let http = builder
            .user_agent(concat!("James/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 || !redirect_policy.allows_redirect(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .expect("valid http client");
        Self { http, network }
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
            REQUEST_TIMEOUT,
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

    /// Downloads a playlist file with a size cap and timeout.
    async fn fetch_playlist(&self, url: &str) -> anyhow::Result<String> {
        let response = tokio::time::timeout(REQUEST_TIMEOUT, self.http.get(url).send())
            .await
            .context("playlist request timed out")?
            .context("playlist request failed")?
            .error_for_status()
            .map_err(|e| {
                anyhow!(
                    "playlist request failed with status {}",
                    e.status().unwrap_or_default()
                )
            })?;
        if let Some(content_type) = response.headers().get(reqwest::header::CONTENT_TYPE) {
            let content_type = content_type.to_str().unwrap_or("").to_ascii_lowercase();
            if content_type.starts_with("audio/")
                && !content_type.contains("mpegurl")
                && !content_type.contains("scpls")
            {
                bail!("the URL is an audio stream, not a playlist");
            }
        }
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        let read = async {
            while let Some(chunk) = chunks.next().await {
                body.extend_from_slice(&chunk?);
                if body.len() > MAX_PLAYLIST_BYTES {
                    bail!("playlist file is too large");
                }
            }
            Ok(())
        };
        tokio::time::timeout(REQUEST_TIMEOUT, read)
            .await
            .context("playlist download timed out")??;
        Ok(String::from_utf8_lossy(&body).into_owned())
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
        let raw = Self::url_of(input).ok_or_else(|| anyhow!("not a stream URL"))?;
        let mut url = self.network.check(raw).await?;
        if extension(&url).is_some_and(|e| PLAYLIST_EXTENSIONS.contains(&e.as_str())) {
            let body = self.fetch_playlist(&url).await?;
            url = parse_playlist(&body)
                .into_iter()
                .next()
                .context("playlist contains no stream URL")?;
            url = self.network.check(&url).await?;
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

    /// Live streams have no stable loudness worth caching.
    async fn cache_key(&self, track: &Track) -> Option<String> {
        (!track.is_stream).then(|| track.locator.clone())
    }

    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        // Locators may come from stored playlists or plugins: re-validate them.
        let url = self.network.check(&track.locator).await?;
        Ok(MediaInput::Url {
            url,
            headers: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_streams_only() {
        let r = StreamResolver::new(NetworkPolicy::default(), None);
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

    #[tokio::test]
    async fn tampered_locators_fail_to_open() {
        let r = StreamResolver::new(NetworkPolicy::default(), None);
        let track = Track {
            source: StreamResolver::ID.into(),
            locator: "http://169.254.169.254/latest/meta-data".into(),
            title: "x".into(),
            artist: None,
            album: None,
            duration: None,
            thumbnail: None,
            is_stream: true,
        };
        assert!(r.open(&track).await.is_err());
    }

    #[tokio::test]
    async fn refuses_local_targets() {
        let r = StreamResolver::new(NetworkPolicy::default(), None);
        let err = r
            .resolve("radio: http://127.0.0.1:8087/live.m3u")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("private"), "{err}");
    }
}
