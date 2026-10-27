//! Metadata extraction with ffprobe.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub has_audio: bool,
}

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    streams: Vec<Stream>,
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct Format {
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

pub async fn probe(ffprobe: &str, path: &Path) -> anyhow::Result<Metadata> {
    let mut cmd = Command::new(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]);
    cmd.args(james_core::media::local_input_args(path));
    james_core::process::apply_proxy_env(&mut cmd, None);
    let out = crate::run(cmd, Duration::from_secs(30)).await?;
    let parsed: Output = serde_json::from_slice(&out)?;
    let mut tags = std::collections::HashMap::new();
    // Container tags take precedence over stream tags (e.g. Ogg stores them per stream).
    for stream in &parsed.streams {
        for (k, v) in &stream.tags {
            tags.insert(k.to_ascii_lowercase(), v.clone());
        }
    }
    if let Some(format) = &parsed.format {
        for (k, v) in &format.tags {
            tags.insert(k.to_ascii_lowercase(), v.clone());
        }
    }
    let tag = |name: &str| {
        tags.get(name)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    Ok(Metadata {
        title: tag("title"),
        artist: tag("artist").or_else(|| tag("album_artist")),
        album: tag("album"),
        duration: parsed
            .format
            .and_then(|f| f.duration)
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(Duration::from_secs_f64),
        has_audio: parsed
            .streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("audio")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DASH manifest disguised as MP3 must not make ffprobe fetch its URLs (SSRF).
    #[tokio::test]
    async fn playlist_formats_in_disguise_fetch_nothing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evil.mp3");
        std::fs::write(
            &path,
            format!(
                r#"<?xml version="1.0"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT10S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011"><Period><AdaptationSet mimeType="audio/mp4"><Representation id="a" bandwidth="1000"><BaseURL>http://{address}/leak</BaseURL></Representation></AdaptationSet></Period></MPD>"#
            ),
        )
        .unwrap();
        assert!(probe("ffprobe", &path).await.is_err());
        let connected = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(connected.is_err(), "ffprobe must not connect anywhere");
    }
}
