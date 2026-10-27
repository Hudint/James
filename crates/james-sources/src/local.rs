//! Local files below configured media directories.
//!
//! Inputs: `local:<path>` (relative to a media directory) or an absolute path
//! inside one of them. Directories resolve to the contained audio files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use futures::StreamExt;
use james_core::source::{strip_scheme, MediaInput, SourceResolver, Track};

use crate::probe::probe;

pub use james_core::media::is_audio_path as is_audio_file;

/// Maximum number of tracks a directory resolves to.
pub const MAX_DIRECTORY_TRACKS: usize = 1000;
/// Files probed in parallel while resolving a directory (also bounded by the global tool limit).
const PROBE_CONCURRENCY: usize = 8;

pub struct LocalResolver {
    roots: Arc<Vec<PathBuf>>,
    ffprobe: String,
}

impl LocalResolver {
    pub const ID: &'static str = "local";

    pub fn new(roots: Vec<PathBuf>, ffprobe: impl Into<String>) -> Self {
        let roots = roots.into_iter().map(|r| r.canonicalize().unwrap_or(r)).collect();
        Self {
            roots: Arc::new(roots),
            ffprobe: ffprobe.into(),
        }
    }

    /// Resolves the input to a canonical path inside a media root. Canonicalising
    /// resolves `..` and symlinks, so the result cannot escape the roots.
    fn locate(roots: &[PathBuf], input: &str) -> anyhow::Result<PathBuf> {
        let candidates: Vec<PathBuf> = match strip_scheme(input, "local") {
            Some(rel) => roots
                .iter()
                .map(|r| r.join(rel.trim_start_matches('/')))
                .collect(),
            None => vec![PathBuf::from(input)],
        };
        for candidate in candidates {
            if let Ok(path) = candidate.canonicalize() {
                if roots.iter().any(|r| path.starts_with(r)) {
                    return Ok(path);
                }
            }
        }
        bail!("'{input}' is not a file inside a media directory")
    }

    async fn locate_async(&self, input: &str) -> anyhow::Result<PathBuf> {
        let (roots, input) = (self.roots.clone(), input.to_owned());
        tokio::task::spawn_blocking(move || Self::locate(&roots, &input)).await?
    }

    pub async fn track_for(&self, path: &Path) -> anyhow::Result<Track> {
        track_for(&self.ffprobe, path).await
    }
}

async fn track_for(ffprobe: &str, path: &Path) -> anyhow::Result<Track> {
    let meta = probe(ffprobe, path).await?;
    if !meta.has_audio {
        bail!("{} contains no audio", path.display());
    }
    let file_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Track {
        source: LocalResolver::ID.into(),
        locator: path.to_string_lossy().into_owned(),
        title: meta.title.unwrap_or(file_name),
        artist: meta.artist,
        album: meta.album,
        duration: meta.duration,
        thumbnail: None,
        is_stream: false,
    })
}

/// Recursively lists audio files (sorted by path), stopping after `limit` files.
/// Blocking: call from `spawn_blocking`.
pub fn walk(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
        for entry in entries {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() && is_audio_file(&path) => files.push(path),
                _ => {}
            }
        }
        if files.len() >= limit {
            break;
        }
    }
    files.sort();
    files.truncate(limit);
    files
}

#[async_trait]
impl SourceResolver for LocalResolver {
    fn id(&self) -> &str {
        Self::ID
    }

    /// Cheap syntactic check; the path itself is validated in `resolve`.
    fn accepts(&self, input: &str) -> bool {
        strip_scheme(input, "local").is_some()
            || (input.starts_with('/') && self.roots.iter().any(|r| Path::new(input).starts_with(r)))
    }

    async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
        let path = self.locate_async(input).await?;
        if !path.is_dir() {
            return Ok(vec![self.track_for(&path).await?]);
        }
        let dir = path.clone();
        let files = tokio::task::spawn_blocking(move || walk(&dir, MAX_DIRECTORY_TRACKS)).await?;
        let ffprobe = self.ffprobe.as_str();
        let tracks: Vec<Track> = futures::stream::iter(files)
            .map(|file| async move {
                let result = track_for(ffprobe, &file).await;
                if let Err(e) = &result {
                    tracing::debug!(file = %file.display(), error = %e, "skipping file");
                }
                result.ok()
            })
            .buffered(PROBE_CONCURRENCY)
            .filter_map(|t| async move { t })
            .collect()
            .await;
        if tracks.is_empty() {
            bail!("no audio files in '{input}'");
        }
        Ok(tracks)
    }

    /// Includes size and modification time, so a replaced file is analysed again.
    async fn cache_key(&self, track: &Track) -> Option<String> {
        // Same canonical path `open` plays, so `local:` locators share the key.
        let path = self.locate_async(&track.locator).await.ok()?;
        let meta = tokio::fs::metadata(&path).await.ok()?;
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(format!("{}#{}#{}", path.display(), meta.len(), mtime))
    }

    async fn open(&self, track: &Track) -> anyhow::Result<MediaInput> {
        // Locators may come from stored playlists or plugins: re-validate them.
        let path = self
            .locate_async(&track.locator)
            .await
            .map_err(|_| anyhow!("file '{}' is not available", track.locator))?;
        if !path.is_file() {
            bail!("file '{}' is not available", track.locator);
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

    fn track(locator: &str) -> Track {
        Track {
            source: LocalResolver::ID.into(),
            locator: locator.into(),
            title: String::new(),
            artist: None,
            album: None,
            duration: None,
            thumbnail: None,
            is_stream: false,
        }
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
        let media = dir.path().join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(dir.path().join("secret.mp3"), b"x").unwrap();
        let resolver = LocalResolver::new(vec![media.clone()], "ffprobe");
        assert!(resolver.resolve("local:../secret.mp3").await.is_err());
        assert!(!resolver.accepts("/etc/passwd"));
        // A crafted locator (e.g. from a stored playlist) must not escape the root either.
        let escaped = format!("{}/../secret.mp3", media.canonicalize().unwrap().display());
        assert!(resolver.open(&track(&escaped)).await.is_err());
    }

    #[test]
    fn walk_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("{i}.mp3")), b"x").unwrap();
        }
        assert_eq!(walk(dir.path(), 3).len(), 3);
    }
}
