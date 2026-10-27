//! Local library scanner: keeps the `library_tracks` index in sync with the media directories.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use futures::StreamExt;
use james_db::library::LibraryEntry;
use james_db::Db;
use tokio::sync::Mutex;

/// Upper bound for indexed files (protects against accidentally pointing at `/`).
const MAX_FILES: usize = 200_000;
const PROBE_CONCURRENCY: usize = 4;

pub struct LibraryScanner {
    db: Db,
    roots: Vec<PathBuf>,
    upload_dir: PathBuf,
    ffprobe: String,
    /// Only one scan at a time.
    lock: Mutex<()>,
    /// Another scan was requested while one was running.
    rescan: std::sync::atomic::AtomicBool,
}

/// A file with the attributes used for change detection.
struct FileInfo {
    path: PathBuf,
    size: i64,
    mtime: i64,
}

fn file_info(path: &Path) -> Option<FileInfo> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    Some(FileInfo {
        path: path.to_path_buf(),
        size: meta.len() as i64,
        mtime,
    })
}

impl LibraryScanner {
    pub fn new(db: Db, roots: Vec<PathBuf>, upload_dir: PathBuf, ffprobe: String) -> Self {
        // Same canonical paths as the local source, so library entries are playable as-is.
        let roots = roots.into_iter().map(|r| r.canonicalize().unwrap_or(r)).collect();
        Self {
            db,
            roots,
            upload_dir,
            ffprobe,
            lock: Mutex::new(()),
            rescan: Default::default(),
        }
    }

    /// Reads metadata of `files` and stores them in the index.
    async fn index_files(&self, files: Vec<FileInfo>) -> anyhow::Result<usize> {
        let ffprobe = self.ffprobe.as_str();
        let entries: Vec<LibraryEntry> = futures::stream::iter(files)
            .map(|file| async move {
                let meta = james_sources::probe::probe(ffprobe, &file.path).await.ok()?;
                let stem = file
                    .path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                meta.has_audio.then(|| LibraryEntry {
                    path: file.path.to_string_lossy().into_owned(),
                    title: meta.title.unwrap_or(stem),
                    artist: meta.artist,
                    album: meta.album,
                    duration_ms: meta.duration.map(|d| d.as_millis() as i64),
                    size: file.size,
                    mtime: file.mtime,
                })
            })
            .buffer_unordered(PROBE_CONCURRENCY)
            .filter_map(|e| async move { e })
            .collect()
            .await;
        for entry in &entries {
            self.db.upsert_library_track(entry).await?;
        }
        Ok(entries.len())
    }

    async fn full_scan(&self) -> anyhow::Result<(usize, usize)> {
        // Whoever holds the lock scans until no further request arrived; a request that
        // finds the lock taken only sets the flag. The flag is re-checked after unlocking so
        // a request racing with the end of a scan is never lost.
        let mut total = (0, 0);
        loop {
            let Ok(guard) = self.lock.try_lock() else {
                self.rescan.store(true, std::sync::atomic::Ordering::SeqCst);
                tracing::info!("library scan already running, queued another pass");
                return Ok(total);
            };
            self.rescan.store(false, std::sync::atomic::Ordering::SeqCst);
            let (updated, removed) = self.scan_once().await?;
            total = (total.0 + updated, total.1 + removed);
            drop(guard);
            if !self.rescan.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(total);
            }
        }
    }

    async fn scan_once(&self) -> anyhow::Result<(usize, usize)> {
        let roots = self.roots.clone();
        let files: Vec<FileInfo> = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            for root in roots {
                let remaining = MAX_FILES.saturating_sub(files.len());
                files.extend(
                    james_sources::local::walk(&root, remaining)
                        .iter()
                        .filter_map(|p| file_info(p)),
                );
            }
            files
        })
        .await?;

        let known: HashMap<String, (i64, i64)> = self
            .db
            .library_index()
            .await?
            .into_iter()
            .map(|(p, size, mtime)| (p, (size, mtime)))
            .collect();
        let present: HashSet<String> = files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        // Entries under a root that is currently unavailable (e.g. an unmounted share) are kept.
        let roots = self.roots.clone();
        let available: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .filter(|r| std::fs::read_dir(r).is_ok())
                .collect()
        })
        .await?;
        let removed: Vec<String> = known
            .keys()
            .filter(|p| !present.contains(*p))
            .filter(|p| available.iter().any(|r| Path::new(p).starts_with(r)))
            .cloned()
            .collect();
        self.db.remove_library_paths(&removed).await?;

        let changed: Vec<FileInfo> = files
            .into_iter()
            .filter(|f| known.get(f.path.to_string_lossy().as_ref()) != Some(&(f.size, f.mtime)))
            .collect();
        let updated = self.index_files(changed).await?;
        tracing::info!(updated, removed = removed.len(), "library scan finished");
        Ok((updated, removed.len()))
    }
}

#[async_trait]
impl james_api::LibraryService for LibraryScanner {
    async fn scan(&self) -> anyhow::Result<(usize, usize)> {
        self.full_scan().await
    }

    async fn index(&self, files: Vec<PathBuf>) -> anyhow::Result<()> {
        let roots = self.roots.clone();
        let infos = tokio::task::spawn_blocking(move || {
            files
                .iter()
                .filter_map(|p| p.canonicalize().ok())
                .filter(|p| roots.iter().any(|r| p.starts_with(r)))
                .filter_map(|p| file_info(&p))
                .collect::<Vec<_>>()
        })
        .await?;
        self.index_files(infos).await?;
        Ok(())
    }

    fn upload_dir(&self) -> PathBuf {
        self.upload_dir.clone()
    }
}
