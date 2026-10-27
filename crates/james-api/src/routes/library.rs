//! Local media library: search, scan, upload.

use axum::extract::{Multipart, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use james_db::library::LibraryTrack;
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
    use super::sanitize_file_name;

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
