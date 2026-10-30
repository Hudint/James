//! Local media library: search, scan, upload.

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use james_db::library::{CustomNames, LibraryTrack};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library", get(search))
        .route("/library/scan", post(scan))
        .route("/library/download", post(download))
        .route("/library/download/{job}", get(download_status))
        .route("/library/{id}", get(one))
        .route("/library/{id}/names", put(rename).delete(reset_names))
        .route(
            "/library/upload",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(super::MAX_UPLOAD)),
        )
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
struct SearchResult {
    total: i64,
    tracks: Vec<LibraryTrack>,
}

async fn search(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<SearchResult>> {
    auth.require("library.read", None)?;
    let tracks = state
        .db
        .search_library(&query.q, query.limit.clamp(1, 500), query.offset.max(0))
        .await?;
    Ok(Json(SearchResult {
        total: state.db.count_library(&query.q).await?,
        tracks,
    }))
}

const MAX_NAME: usize = 200;

/// A field that is missing (`None`), `null` (`Some(None)`) or a string.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// New names; a missing field (or one equal to the file's tag) follows the file's tags,
/// `null` or `""` for artist and album means none.
#[derive(Deserialize)]
struct NamesBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    artist: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    album: Option<Option<String>>,
}

/// Invisible and direction-changing characters would let names look like other text (zero-width
/// joiners stay: scripts and emoji need them).
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'
                | '\u{200E}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

/// Characters that show as empty space.
fn blank(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{115F}' | '\u{1160}' | '\u{2800}' | '\u{3164}' | '\u{FFA0}')
}

/// Trimmed, without hidden characters; `""` if nothing visible is left.
fn clean_name(value: &str, field: &str) -> ApiResult<String> {
    let clean: String = value.chars().filter(|c| !hidden(*c)).collect();
    let clean = clean.trim_matches(blank);
    if clean.chars().count() > MAX_NAME {
        return Err(ApiError::bad_request(format!(
            "{field} is longer than {MAX_NAME} characters"
        )));
    }
    Ok(clean.to_owned())
}

/// The value to store for an optional field: `None` keeps the file's tag (also when the value is
/// the tag, which is not cleaned or checked then).
fn custom_field(value: Option<Option<String>>, tag: Option<&str>, field: &str) -> ApiResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.as_deref().unwrap_or_default();
    if value.trim() == tag.unwrap_or_default().trim() {
        return Ok(None);
    }
    let clean = clean_name(value, field)?;
    Ok((clean != tag.unwrap_or_default()).then_some(clean))
}

/// Sets the names shown and played for a track (the file is not changed).
async fn rename(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<NamesBody>,
) -> ApiResult<Json<LibraryTrack>> {
    auth.require("library.manage", None)?;
    let current = state
        .db
        .library_track(id)
        .await?
        .ok_or_else(|| ApiError::not_found("track"))?;
    let title = custom_field(body.title.map(Some), Some(&current.tags.title), "title")?;
    if title.as_deref() == Some("") {
        return Err(ApiError::bad_request("the title must not be empty"));
    }
    let names = CustomNames {
        title,
        artist: custom_field(body.artist, current.tags.artist.as_deref(), "artist")?,
        album: custom_field(body.album, current.tags.album.as_deref(), "album")?,
    };
    store_names(&state, &auth, id, &names).await
}

/// Back to the names from the file's tags.
async fn reset_names(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<LibraryTrack>> {
    auth.require("library.manage", None)?;
    store_names(&state, &auth, id, &CustomNames::default()).await
}

async fn store_names(
    state: &AppState,
    auth: &Auth,
    id: i64,
    names: &CustomNames,
) -> ApiResult<Json<LibraryTrack>> {
    let track = state
        .db
        .rename_library_track(id, names)
        .await?
        .ok_or_else(|| ApiError::not_found("track"))?;
    tracing::info!(
        user = %auth.user.username,
        track = id,
        title = ?names.title,
        artist = ?names.artist,
        album = ?names.album,
        "library track renamed"
    );
    Ok(Json(track))
}

async fn one(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<LibraryTrack>> {
    auth.require("library.read", None)?;
    state
        .db
        .library_track(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("track"))
}

#[derive(Deserialize)]
struct DownloadRequest {
    /// URL or search (like playing), e.g. a YouTube link.
    input: String,
}

/// Outcome of a save that went on in the background, for its user to ask for.
enum SaveJob {
    Running,
    Done(LibraryTrack),
    Failed(String),
}

/// Background saves by job id: (user, created, state). Kept for [`JOB_TTL`].
type SaveJobs = parking_lot::Mutex<std::collections::HashMap<String, (i64, std::time::Instant, SaveJob)>>;
static JOBS: std::sync::LazyLock<SaveJobs> = std::sync::LazyLock::new(Default::default);
const JOB_TTL: std::time::Duration = std::time::Duration::from_secs(600);
const MAX_JOBS: usize = 1000;
/// Background saves one user may have running at once, and kept in all.
const MAX_USER_JOBS: usize = 3;
const MAX_USER_KEPT_JOBS: usize = 20;

/// Marks its job failed when dropped before [`JobGuard::finish`] (the save task panicked).
struct JobGuard(Option<String>);

impl JobGuard {
    fn finish(mut self, state: SaveJob) {
        if let Some(id) = self.0.take() {
            set_job(&id, state);
        }
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            set_job(&id, SaveJob::Failed("saving failed".into()));
        }
    }
}

fn set_job(id: &str, state: SaveJob) {
    if let Some(job) = JOBS.lock().get_mut(id) {
        job.2 = state;
    }
}

/// The state of a background save (`running`, `done` with the track, `failed` with the error).
async fn download_status(auth: Auth, Path(job): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    auth.require("library.download", None)?;
    let mut jobs = JOBS.lock();
    // Only its own user learns about a job.
    let Some((_, _, state)) = jobs.get(&job).filter(|(user, ..)| *user == auth.user.id) else {
        return Err(ApiError::not_found("job"));
    };
    let answer = match state {
        SaveJob::Running => return Ok(Json(json!({ "state": "running" }))),
        SaveJob::Done(track) => json!({ "state": "done", "track": track }),
        SaveJob::Failed(error) => json!({ "state": "failed", "error": error }),
    };
    // Told once: a finished job is gone afterwards.
    jobs.remove(&job);
    Ok(Json(answer))
}

/// How long a save request waits for the result; longer saves finish in the background
/// (reverse proxies cut requests after about a minute).
const SAVE_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// Saves a track of another source (YouTube, …) as a file in the library: `201` with its entry
/// (the existing one if it was saved before), or `202` when it takes longer and continues in
/// the background (`{pending, job}`: ask `GET /library/download/{job}` for the outcome).
async fn download(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<DownloadRequest>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    auth.require("library.download", None)?;
    state.actions.check(auth.user.id)?;
    let input = body.input.trim().to_owned();
    if input.is_empty() || input.len() > 2048 {
        return Err(ApiError::bad_request("enter a link or a search"));
    }
    let library = state.library.clone();
    let user = auth.user.username.clone();
    let owner = format!("user:{}", auth.user.id);
    let job = format!("{:016x}", rand::random::<u64>());
    {
        let mut jobs = JOBS.lock();
        jobs.retain(|_, (_, created, _)| created.elapsed() < JOB_TTL);
        let mine = jobs.values().filter(|(user, ..)| *user == auth.user.id);
        let running = mine
            .clone()
            .filter(|(_, _, state)| matches!(state, SaveJob::Running))
            .count();
        if jobs.len() >= MAX_JOBS || running >= MAX_USER_JOBS || mine.count() >= MAX_USER_KEPT_JOBS {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many saves at once, try again later",
            ));
        }
        jobs.insert(
            job.clone(),
            (auth.user.id, std::time::Instant::now(), SaveJob::Running),
        );
    }
    let guard = JobGuard(Some(job.clone()));
    // Its own task: a closed browser tab or a cut connection does not stop the save.
    let mut task = tokio::spawn(async move {
        let result = library.save(&input, &owner).await;
        guard.finish(match &result {
            Ok(track) => SaveJob::Done(track.clone()),
            Err(e) => SaveJob::Failed(crate::error::user_message(&format!("{e:#}"))),
        });
        match &result {
            // Titles come from websites: logged escaped. Errors may carry the input: debug only.
            Ok(track) => {
                tracing::info!(%user, track = track.id, title = ?track.title, "saved to the library")
            }
            Err(e) => tracing::debug!(%user, error = %format!("{e:#}"), "saving to the library failed"),
        }
        result
    });
    match tokio::time::timeout(SAVE_WAIT, &mut task).await {
        Ok(joined) => {
            JOBS.lock().remove(&job);
            let track = joined.map_err(|e| {
                tracing::error!(error = %e, "the save task failed");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            })??;
            Ok((StatusCode::CREATED, Json(track)).into_response())
        }
        Err(_) => Ok((StatusCode::ACCEPTED, Json(json!({ "pending": true, "job": job }))).into_response()),
    }
}

/// Starts a rescan in the background (a large library takes long); `202 Accepted`.
async fn scan(State(state): State<AppState>, auth: Auth) -> ApiResult<StatusCode> {
    auth.require("library.manage", None)?;
    let library = state.library.clone();
    tokio::spawn(async move {
        if let Err(e) = library.scan().await {
            tracing::warn!(error = %format!("{e:#}"), "library scan failed");
        }
    });
    Ok(StatusCode::ACCEPTED)
}

/// Keeps only safe characters of an uploaded file name.
fn sanitize_file_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?.trim();
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let clean = clean.trim_start_matches('.').to_owned();
    (!clean.is_empty() && clean.len() <= 200).then_some(clean)
}

async fn upload(
    State(state): State<AppState>,
    auth: Auth,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require("library.manage", None)?;
    let dir = state.library.upload_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
    let mut stored = Vec::new();
    let mut paths = Vec::new();
    let mut failure = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => {
                failure = Some(ApiError::bad_request(e.to_string()));
                break;
            }
        };
        match store_field(&dir, field).await {
            Ok(Some((name, path))) => {
                stored.push(name);
                paths.push(path);
            }
            Ok(None) => {}
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    // Files stored before a failure are indexed anyway, so a retry does not hit "already exists".
    if !paths.is_empty() {
        state.library.index(paths).await?;
    }
    match failure {
        Some(e) => Err(e),
        None => Ok((StatusCode::CREATED, Json(json!({ "files": stored })))),
    }
}

const STALE_PART: std::time::Duration = std::time::Duration::from_secs(3600);

/// Stores one multipart file; `None` for fields without a usable file name.
async fn store_field(
    dir: &std::path::Path,
    mut field: axum::extract::multipart::Field<'_>,
) -> ApiResult<Option<(String, std::path::PathBuf)>> {
    let Some(name) = field.file_name().and_then(sanitize_file_name) else {
        return Ok(None);
    };
    let path = dir.join(&name);
    if !james_core::media::is_audio_path(&path) {
        return Err(ApiError::bad_request(format!(
            "'{name}' is not a supported audio file"
        )));
    }
    if tokio::fs::try_exists(&path).await.unwrap_or(true) {
        return Err(ApiError::conflict(format!("'{name}' already exists")));
    }
    // Written under a temporary name (not an audio extension, so scans skip it) and renamed
    // when complete: a scan never sees half-written files.
    let partial = dir.join(format!(".{name}.part"));
    // A temporary file older than an hour is left over from a crash, not a running upload.
    if let Ok(meta) = tokio::fs::metadata(&partial).await {
        if meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > STALE_PART)
        {
            let _ = tokio::fs::remove_file(&partial).await;
        }
    }
    let Ok(mut file) = tokio::fs::File::create_new(&partial).await else {
        // Another upload of the same name is in progress: leave its file alone.
        return Err(ApiError::conflict(format!("'{name}' is already being uploaded")));
    };
    let result: anyhow::Result<()> = async {
        while let Some(chunk) = field.chunk().await? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        if tokio::fs::try_exists(&path).await? {
            anyhow::bail!("'{name}' already exists");
        }
        tokio::fs::rename(&partial, &path).await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(ApiError::bad_request(format!(
            "upload of '{name}' failed: {}",
            crate::error::user_message(&format!("{e:#}"))
        )));
    }
    Ok(Some((name, path)))
}

#[cfg(test)]
mod tests {
    use super::{clean_name, custom_field, sanitize_file_name};

    #[test]
    fn names() {
        assert_eq!(clean_name(" a\u{202E}b\u{200B}c\n ", "t").unwrap(), "abc");
        assert_eq!(
            clean_name("\u{3164} \u{061C}\u{E0041}\u{FE0F}\u{2800}", "t").unwrap(),
            ""
        );
        assert_eq!(clean_name("a\u{3164}b", "t").unwrap(), "a\u{3164}b");
        assert_eq!(clean_name(&"ä".repeat(200), "t").unwrap().chars().count(), 200);
        assert!(clean_name(&"ä".repeat(201), "t").is_err());
        let field =
            |v: Option<Option<&str>>, tag| custom_field(v.map(|v| v.map(String::from)), tag, "f").unwrap();
        assert_eq!(field(None, Some("Ann")), None, "missing: the file's tag");
        assert_eq!(field(Some(Some("Ann ")), Some("Ann")), None, "same as the tag");
        assert_eq!(field(Some(None), Some("Ann")).as_deref(), Some(""), "null: none");
        assert_eq!(field(Some(Some(" ")), Some("Ann")).as_deref(), Some(""));
        assert_eq!(field(Some(Some("")), None), None, "no tag, none wanted");
        assert_eq!(field(Some(Some("Bob")), None).as_deref(), Some("Bob"));
        let long = "x".repeat(300);
        assert_eq!(
            field(Some(Some(&long)), Some(&long)),
            None,
            "an unchanged tag is not checked"
        );
        assert_eq!(field(Some(Some("a\u{200D}b")), Some("a\u{200D}b")), None);
        assert_eq!(clean_name("👩\u{200D}💻", "t").unwrap(), "👩\u{200D}💻");
    }

    #[test]
    fn file_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            sanitize_file_name("C:\\music\\song.mp3").as_deref(),
            Some("song.mp3")
        );
        assert_eq!(sanitize_file_name(".hidden.mp3").as_deref(), Some("hidden.mp3"));
        assert_eq!(sanitize_file_name("..").as_deref(), None);
    }
}
