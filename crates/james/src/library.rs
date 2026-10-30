//! Local library scanner: keeps the `library_tracks` index in sync with the media directories.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use futures::StreamExt;
use james_core::source::{SourceRegistry, Track};
use james_db::library::{CustomNames, LibraryEntry, LibraryTrack};
use james_db::Db;
use parking_lot::RwLock;
use tokio::sync::{Mutex, Semaphore};

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
    downloads: Option<Downloads>,
}

/// Saving tracks of other sources (YouTube, …) as library files.
struct Downloads {
    sources: Arc<RwLock<SourceRegistry>>,
    /// Where saved files go (inside a library root).
    dir: PathBuf,
    /// Private directory for downloads in progress (outside the library: never scanned).
    work_dir: PathBuf,
    /// Downloads at once (they are long and large).
    slots: Semaphore,
    running: parking_lot::Mutex<RunningSaves>,
    /// Space all saved files may take together.
    quota_bytes: u64,
}

/// Downloads running at once; further ones wait (up to [`DOWNLOAD_WAIT`]).
const MAX_DOWNLOADS: usize = 2;
const DOWNLOAD_WAIT: Duration = Duration::from_secs(120);

/// A directory deleted with everything in it when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A readable file name from track names: no path separators, control or reserved characters.
fn file_name_from(track: &Track) -> String {
    let name = match &track.artist {
        Some(artist) if !track.title.contains(artist.as_str()) => format!("{artist} - {}", track.title),
        _ => track.title.clone(),
    };
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    // File systems allow 255 bytes per name; room is left for the id and the extension.
    let mut clean = clean.trim_matches(['.', ' ']).to_owned();
    while clean.len() > 150 {
        clean.pop();
    }
    let clean = clean.trim_end_matches(['.', ' ']);
    if clean.is_empty() {
        "Download".into()
    } else {
        clean.to_owned()
    }
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
            downloads: None,
        }
    }

    /// Enables saving tracks of `sources` to `dir` (inside a library root); `work_dir` is a
    /// private directory outside the library.
    pub fn with_downloads(
        mut self,
        sources: Arc<RwLock<SourceRegistry>>,
        dir: PathBuf,
        work_dir: PathBuf,
        quota_bytes: u64,
    ) -> Self {
        let dir = dir.canonicalize().unwrap_or(dir);
        // Copies left over from a crash (they are no library files and only take space).
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let empty = entry.metadata().is_ok_and(|m| m.len() == 0);
                if (name.starts_with('.') && name.ends_with(".part"))
                    || (empty && james_core::media::is_audio_path(&entry.path()))
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        if !self.roots.iter().any(|root| dir.starts_with(root)) {
            tracing::warn!(dir = %dir.display(), "the folder for saved tracks is outside the media folders (a symlink?): saved tracks cannot be played");
        }
        self.downloads = Some(Downloads {
            sources,
            dir,
            work_dir,
            slots: Semaphore::new(MAX_DOWNLOADS),
            running: Default::default(),
            quota_bytes,
        });
        self
    }

    /// Resolves `input` to one track and saves it as a library file. `owner` (a user or
    /// client) can save one track at a time.
    async fn save_input(&self, input: &str, owner: &str) -> anyhow::Result<LibraryTrack> {
        let downloads = self
            .downloads
            .as_ref()
            .ok_or_else(|| anyhow!("saving to the library is not available"))?;
        // Before any work: one save per owner, and only a few at all.
        let mut running = Running::start(&downloads.running, owner)?;
        let registry = downloads.sources.read().clone();
        let mut tracks = registry.resolve(input).await?;
        if tracks.len() > 1 {
            bail!(
                "this is a playlist ({} tracks): save its tracks one at a time",
                tracks.len()
            );
        }
        let track = tracks.pop().ok_or_else(|| anyhow!("nothing found"))?;
        if track.source == james_sources::local::LocalResolver::ID {
            bail!("this is already a file in the library");
        }
        // Saved before (known id): no second download.
        if let Some(id) = youtube_id(&track.locator) {
            if let Some(existing) = find_saved(&downloads.dir, &id).await {
                return self.library_entry(&existing, &track).await;
            }
        }
        running.item(&track.locator)?;
        let _slot = tokio::time::timeout(DOWNLOAD_WAIT, downloads.slots.acquire())
            .await
            .map_err(|_| anyhow!("other tracks are being saved, please try again in a moment"))??;
        // Measured once it is this save's turn (earlier saves have finished by then).
        let dir = downloads.dir.clone();
        let used = tokio::task::spawn_blocking(move || {
            james_sources::local::walk(&dir, MAX_FILES)
                .iter()
                .filter_map(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .sum::<u64>()
        })
        .await?;
        if used >= downloads.quota_bytes {
            bail!("the space for saved tracks is used up: remove some in the library folder or raise ytdlp.download_quota_mb");
        }

        let temp = TempDir(
            downloads
                .work_dir
                .join(format!("download-{:016x}", rand::random::<u64>())),
        );
        let mut builder = tokio::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(&temp.0)
            .await
            .context("cannot create a download directory")?;
        let file = registry.download(&track, &temp.0).await?;

        // The source's id (e.g. the YouTube video id) keeps two tracks with the same name apart
        // and finds one saved before (also under another name).
        let id = file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let ext = file
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        tokio::fs::create_dir_all(&downloads.dir).await?;
        let target = match find_saved(&downloads.dir, &id).await {
            Some(existing) => existing,
            None => {
                // Within 255 bytes per name also with long ids (some sites): the readable part yields.
                let mut name = file_name_from(&track);
                while !name.is_empty() && name.len() + id.len() + ext.len() + 4 > 240 {
                    name.pop();
                }
                let target = downloads.dir.join(format!("{} [{id}].{ext}", name.trim_end()));
                let created = move_file(&file, &target).await?;
                // A file the library cannot use does not stay (it would only take space); one
                // that existed belongs to another save.
                return match self.library_entry(&target, &track).await {
                    Ok(entry) => Ok(entry),
                    Err(e) => {
                        if created {
                            let _ = tokio::fs::remove_file(&target).await;
                        }
                        Err(e)
                    }
                };
            }
        };
        self.library_entry(&target, &track).await
    }

    /// The library entry of a saved file (indexed now if needed), with the source's names for
    /// what the file's tags lack.
    async fn library_entry(&self, file: &Path, track: &Track) -> anyhow::Result<LibraryTrack> {
        let path = file.to_string_lossy().into_owned();
        if self.db.library_track_by_path(&path).await?.is_none() {
            let info = file_info(file).ok_or_else(|| anyhow!("the saved file is gone"))?;
            if self.index_files(vec![info]).await? == 0 {
                bail!("the saved file has no audio");
            }
        }
        let entry = self
            .db
            .library_track_by_path(&path)
            .await?
            .ok_or_else(|| anyhow!("the saved file is not in the library"))?;
        if entry.edited {
            return Ok(entry);
        }
        // Without a title tag the file name would be the title.
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let names = CustomNames {
            title: (entry.title == stem).then(|| track.title.clone()),
            artist: entry.artist.is_none().then(|| track.artist.clone()).flatten(),
            album: entry.album.is_none().then(|| track.album.clone()).flatten(),
        };
        if names.is_empty() {
            return Ok(entry);
        }
        Ok(self
            .db
            .rename_library_track(entry.id, &names)
            .await?
            .unwrap_or(entry))
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
        let missing: Vec<String> = known
            .keys()
            .filter(|p| !present.contains(*p))
            .filter(|p| available.iter().any(|r| Path::new(p).starts_with(r)))
            .cloned()
            .collect();
        // Files added while walking (uploads, saves) are not "removed".
        let removed: Vec<String> = tokio::task::spawn_blocking(move || {
            missing.into_iter().filter(|p| !Path::new(p).exists()).collect()
        })
        .await?;
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

/// The video id of a YouTube watch or short link (known before downloading).
fn youtube_id(locator: &str) -> Option<String> {
    let url = url::Url::parse(locator).ok()?;
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let id = match host {
        "youtube.com" | "music.youtube.com" => url.query_pairs().find(|(k, _)| k == "v")?.1.into_owned(),
        "youtu.be" => url.path().trim_start_matches('/').to_owned(),
        _ => return None,
    };
    // As yt-dlp names saved files: `<extractor>-<id>`.
    (!id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| format!("Youtube-{id}"))
}

/// A file saved before from the source item `id` (named `… [id].ext`).
async fn find_saved(dir: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!(" [{id}]");
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        // An empty file is a name reserved by a save still copying.
        let filled = entry.metadata().await.is_ok_and(|m| m.len() > 0);
        if stem.ends_with(&suffix) && james_core::media::is_audio_path(&path) && filled {
            return Some(path);
        }
    }
    None
}

/// Saves in progress: one per owner (user or client) and one per source item.
#[derive(Default)]
struct RunningSaves {
    owners: HashSet<String>,
    items: HashSet<String>,
}

/// Saves waiting or running at once (each holds yt-dlp work); more are refused at once. Chat
/// clients (whose identities are cheap to make) get fewer of them.
const MAX_PENDING_SAVES: usize = 8;
const MAX_CHAT_SAVES: usize = 2;

/// Marks a save as running until dropped: first for its owner (before any work is done), then
/// also for its item (once resolved).
struct Running<'a> {
    saves: &'a parking_lot::Mutex<RunningSaves>,
    owner: String,
    item: Option<String>,
}

impl<'a> Running<'a> {
    fn start(saves: &'a parking_lot::Mutex<RunningSaves>, owner: &str) -> anyhow::Result<Self> {
        let mut running = saves.lock();
        let chat = running.owners.iter().filter(|o| o.starts_with("client:")).count();
        if running.owners.len() >= MAX_PENDING_SAVES
            || (owner.starts_with("client:") && chat >= MAX_CHAT_SAVES)
        {
            bail!("many tracks are being saved right now, please try again in a few minutes");
        }
        if !running.owners.insert(owner.to_owned()) {
            bail!("you are saving a track already, please wait until it is done");
        }
        Ok(Self {
            saves,
            owner: owner.to_owned(),
            item: None,
        })
    }

    fn item(&mut self, item: &str) -> anyhow::Result<()> {
        let mut running = self.saves.lock();
        if !running.items.insert(item.to_owned()) {
            bail!("this track is being saved already");
        }
        self.item = Some(item.to_owned());
        Ok(())
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut running = self.saves.lock();
        running.owners.remove(&self.owner);
        if let Some(item) = &self.item {
            running.items.remove(item);
        }
    }
}

/// Moves a file without replacing an existing one, also across file systems (copied under a
/// name scans skip first). Returns whether it created `to` (`false`: it existed, and counts).
async fn move_file(from: &Path, to: &Path) -> anyhow::Result<bool> {
    // A hard link never replaces its target (unlike rename).
    match tokio::fs::hard_link(from, to).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(from).await;
            return Ok(true);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(_) => {}
    }
    // Another file system, or one without hard links: the name is reserved first (create_new
    // is atomic everywhere), then the copy renamed over the reservation.
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .await
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e).context("cannot store the saved file"),
    }
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let partial = to.with_file_name(format!(".{name}.{:08x}.part", rand::random::<u32>()));
    let copied = async {
        tokio::fs::copy(from, &partial).await?;
        tokio::fs::rename(&partial, to).await
    }
    .await;
    if let Err(e) = copied {
        let _ = tokio::fs::remove_file(&partial).await;
        let _ = tokio::fs::remove_file(to).await;
        return Err(e).context("cannot store the saved file");
    }
    Ok(true)
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

    async fn save(&self, input: &str, owner: &str) -> anyhow::Result<LibraryTrack> {
        self.save_input(input, owner).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: Option<&str>) -> Track {
        Track {
            source: "ytdlp".into(),
            locator: "https://youtu.be/x".into(),
            title: title.into(),
            artist: artist.map(Into::into),
            album: None,
            duration: None,
            thumbnail: None,
            is_stream: false,
        }
    }

    #[test]
    fn youtube_ids_are_read_from_links() {
        assert_eq!(
            youtube_id("https://www.youtube.com/watch?v=jNQXAC9IVRw").as_deref(),
            Some("Youtube-jNQXAC9IVRw")
        );
        assert_eq!(
            youtube_id("https://youtu.be/jNQXAC9IVRw?t=3").as_deref(),
            Some("Youtube-jNQXAC9IVRw")
        );
        assert_eq!(
            youtube_id("https://music.youtube.com/watch?v=a-b_c").as_deref(),
            Some("Youtube-a-b_c")
        );
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=../x"), None);
        assert_eq!(youtube_id("https://soundcloud.com/forss/flickermood"), None);
    }

    /// A source whose "download" is a short generated audio file without tags.
    struct FakeSource {
        downloads: std::sync::atomic::AtomicUsize,
        resolves: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl james_core::source::SourceResolver for FakeSource {
        fn id(&self) -> &str {
            "ytdlp"
        }
        fn accepts(&self, _: &str) -> bool {
            true
        }
        async fn resolve(&self, input: &str) -> anyhow::Result<Vec<Track>> {
            self.resolves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut one = track("Song", Some("Band"));
            one.locator = input.into();
            Ok(if input == "list" {
                vec![one.clone(), one]
            } else {
                vec![one]
            })
        }
        async fn open(&self, _: &Track) -> anyhow::Result<james_core::source::MediaInput> {
            bail!("not needed")
        }
        async fn download(&self, _: &Track, dir: &Path) -> anyhow::Result<PathBuf> {
            self.downloads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let file = dir.join("Generic-vid123.flac");
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "quiet",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=d=0.5",
                    "-map_metadata",
                    "-1",
                ])
                .arg(&file)
                .status()?;
            anyhow::ensure!(status.success(), "ffmpeg failed");
            Ok(file)
        }
    }

    #[tokio::test]
    async fn saved_tracks_land_in_the_library_once() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg missing: skipped");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        std::fs::create_dir_all(media.join("downloads")).unwrap();
        let db = Db::memory().await.unwrap();
        let source = Arc::new(FakeSource {
            downloads: Default::default(),
            resolves: Default::default(),
        });
        let mut registry = SourceRegistry::default();
        registry.register(0, source.clone());
        let scanner = LibraryScanner::new(
            db.clone(),
            vec![media.clone()],
            media.join("uploads"),
            "ffprobe".into(),
        )
        .with_downloads(
            Arc::new(RwLock::new(registry)),
            media.join("downloads"),
            work.path().to_path_buf(),
            u64::MAX,
        );

        let saved = scanner
            .save_input("https://example.com/v", "user:1")
            .await
            .unwrap();
        assert!(
            saved.path.ends_with("Band - Song [Generic-vid123].flac"),
            "{}",
            saved.path
        );
        assert_eq!(
            (saved.title.as_str(), saved.artist.as_deref()),
            ("Song", Some("Band")),
            "names of the source without tags"
        );
        let again = scanner
            .save_input("https://example.com/v", "user:1")
            .await
            .unwrap();
        assert_eq!(again.id, saved.id, "the same entry");
        assert_eq!(std::fs::read_dir(media.join("downloads")).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_dir(work.path()).unwrap().count(),
            0,
            "download directories removed"
        );
        assert!(scanner
            .save_input("list", "user:1")
            .await
            .unwrap_err()
            .to_string()
            .contains("playlist"));
        assert_eq!(source.downloads.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A YouTube video saved before is found before downloading.
        std::fs::copy(
            media.join("downloads/Band - Song [Generic-vid123].flac"),
            media.join("downloads/Old [Youtube-abc].flac"),
        )
        .unwrap();
        let old = scanner
            .save_input("https://youtu.be/abc", "user:1")
            .await
            .unwrap();
        assert!(old.path.ends_with("Old [Youtube-abc].flac"));
        assert_eq!(
            source.downloads.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "no download"
        );

        // One save per owner, refused before any work.
        let downloads = scanner.downloads.as_ref().unwrap();
        let held = Running::start(&downloads.running, "user:2").unwrap();
        let resolves = source.resolves.load(std::sync::atomic::Ordering::SeqCst);
        assert!(scanner.save_input("x", "user:2").await.is_err());
        assert_eq!(
            source.resolves.load(std::sync::atomic::Ordering::SeqCst),
            resolves
        );
        drop(held);
    }

    #[tokio::test]
    async fn saved_files_are_never_replaced_or_spoofed() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, to) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("t.flac"),
        );
        std::fs::write(&a, "first").unwrap();
        std::fs::write(&b, "second").unwrap();
        assert!(move_file(&a, &to).await.unwrap());
        assert!(!move_file(&b, &to).await.unwrap(), "an existing file counts");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "first");
        // A title imitating a YouTube id does not pass for that video.
        let named = format!(
            "{} [Generic-x].flac",
            file_name_from(&track("A [Youtube-abc]", None))
        );
        std::fs::write(dir.path().join(named), "x").unwrap();
        assert!(find_saved(dir.path(), "Youtube-abc").await.is_none());
        assert!(find_saved(dir.path(), "Generic-x").await.is_some());
    }

    #[test]
    fn file_names_are_safe_and_short() {
        assert_eq!(file_name_from(&track("Song", Some("Band"))), "Band - Song");
        assert_eq!(
            file_name_from(&track("Band - Song (Official)", Some("Band"))),
            "Band - Song (Official)"
        );
        assert_eq!(file_name_from(&track("../../etc/passwd", None)), "etc passwd");
        assert_eq!(file_name_from(&track("a/b\\c:d\u{0}e", None)), "a b c d e");
        assert_eq!(file_name_from(&track("...", None)), "Download");
        let long = file_name_from(&track(&"ä".repeat(200), None));
        assert!(
            long.len() <= 150 && long.chars().all(|c| c == 'ä'),
            "cut at a character boundary"
        );
    }
}
