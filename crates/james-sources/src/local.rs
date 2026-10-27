//! Local files below configured media directories.
//!
//! Inputs: `local:<path>` (relative to a media directory) or an absolute path
//! inside one of them. Directories resolve to all contained audio files.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use james_core::source::{strip_scheme, MediaInput, SourceResolver, Track};

use crate::probe::probe;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "alac", "aiff", "ape", "wv", "mka",
    "webm",
];

pub struct LocalResolver {
    roots: Vec<PathBuf>,
    ffprobe: String,
}

impl LocalResolver {
    pub const ID: &'static str = "local";

    pub fn new(roots: Vec<PathBuf>, ffprobe: impl Into<String>) -> Self {
        let roots = roots.into_iter().map(|r| r.canonicalize().unwrap_or(r)).collect();
        Self {
            roots,
            ffprobe: ffprobe.into(),
        }
    }

    /// Resolves the input to a canonical path inside a media root (prevents `..` escapes).
    fn locate(&self, input: &str) -> anyhow::Result<PathBuf> {
        let candidates: Vec<PathBuf> = match strip_scheme(input, "local") {
            Some(rel) => self
                .roots
                .iter()
                .map(|r| r.join(rel.trim_start_matches('/')))
                .collect(),
            None => vec![PathBuf::from(input)],
        };
        for candidate in candidates {
            if let Ok(path) = candidate.canonicalize() {
                if self.roots.iter().any(|r| path.starts_with(r)) {
                    return Ok(path);
                }
            }
        }
        bail!("'{input}' is not a file inside a media directory")
    }

    pub async fn track_for(&self, path: &Path) -> anyhow::Result<Track> {
        let meta = probe(&self.ffprobe, path).await?;
        if !meta.has_audio {
            bail!("{} contains no audio", path.display());
        }
        let file_name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Track {
            source: Self::ID.into(),
            locator: path.to_string_lossy().into_owned(),
            title: meta.title.unwrap_or(file_name),
            artist: meta.artist,
            album: meta.album,
            duration: meta.duration,
            thumbnail: None,
            is_stream: false,
        })
    }
}

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Recursively lists audio files, sorted by path.
pub fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() && is_audio_file(&path) => files.push(path),
                _ => {}
            }
        }
    }
    files.sort();
    files
}

#[async_trait]
impl SourceResolver for LocalResolver {
    fn id(&self) -> &str {
        Self::ID
    }

    fn accepts(&self, input: &str) -> bool {
        strip_scheme(input, "local").is_some() || (input.starts_with('/') && self.locate(input).is_ok())
    }

    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        let path = self.locate(input)?;
        if path.is_dir() {
            let mut tracks = Vec::new();
            for file in walk(&path) {
                match self.track_for(&file).await {
                    Ok(track) => tracks.push(track),
                    Err(e) => tracing::debug!(file = %file.display(), error = %e, "skipping file"),
                }
            }
            if tracks.is_empty() {
                bail!("no audio files in '{input}'");
            }
            Ok(tracks)
        } else {
            Ok(vec![self.track_for(&path).await?])
        }
    }

    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        let path = PathBuf::from(&track.locator);
        if !self.roots.iter().any(|r| path.starts_with(r)) || !path.is_file() {
            return Err(anyhow!("file '{}' is no longer available", track.locator));
        }
        Ok(MediaInput::File(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(path: &Path, title: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=1",
            ])
            .args([
                "-metadata",
                &format!("title={title}"),
                "-metadata",
                "artist=Tester",
            ])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn resolves_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        make_file(&dir.path().join("album/01.flac"), "First");
        make_file(&dir.path().join("album/02.ogg"), "Second");
        std::fs::write(dir.path().join("album/cover.jpg"), b"x").unwrap();
        let resolver = LocalResolver::new(vec![dir.path().into()], "ffprobe");

        let tracks = resolver.resolve("local:album").await.unwrap();
        let titles: Vec<_> = tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["First", "Second"]);
        assert_eq!(tracks[0].artist.as_deref(), Some("Tester"));
        assert!(tracks[0].duration.unwrap().as_secs_f64() > 0.9);

        let single = resolver.resolve("local:/album/02.ogg").await.unwrap();
        assert_eq!(single[0].title, "Second");
        assert!(matches!(
            resolver.open(&single[0]).await.unwrap(),
            MediaInput::File(_)
        ));
    }

    #[tokio::test]
    async fn rejects_paths_outside_roots() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = LocalResolver::new(vec![dir.path().into()], "ffprobe");
        assert!(resolver.resolve("local:../../etc/passwd").await.is_err());
        assert!(!resolver.accepts("/etc/passwd"));
    }
}
