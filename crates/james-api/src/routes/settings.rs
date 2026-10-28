//! Server settings managed in the web interface (administrators only).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/settings/youtube-cookies",
        get(cookie_status).put(upload_cookies).delete(remove_cookies),
    )
}

/// The cookies grant access to a YouTube account: administrators only.
fn admin(auth: &Auth) -> ApiResult<()> {
    if auth.user.is_admin {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

/// Whether cookies are stored (never their content).
async fn cookie_status(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    admin(&auth)?;
    let settings = &state.youtube_cookies;
    let uploaded = match tokio::fs::read_to_string(&settings.path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::error!(error = %e, "cannot read the YouTube cookies");
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the stored cookies cannot be read",
            ));
        }
        Ok(content) => {
            let changed = tokio::fs::metadata(&settings.path)
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let cookies = filter_cookies(&content).map(|c| c.1).unwrap_or(0);
            Some(json!({ "cookies": cookies, "changedAt": changed }))
        }
    };
    Ok(Json(
        json!({ "configured": settings.configured, "uploaded": uploaded }),
    ))
}

#[derive(Deserialize)]
struct CookieUpload {
    content: String,
}

/// Stores a Netscape cookie file (as browser extensions export it); yt-dlp uses it from the
/// next request on.
async fn upload_cookies(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<CookieUpload>,
) -> ApiResult<Json<Value>> {
    admin(&auth)?;
    let (content, count, dropped) = filter_cookies(&body.content).map_err(ApiError::bad_request)?;
    if count == 0 {
        return Err(ApiError::bad_request(
            "the file contains no YouTube cookies (export them while signed in to youtube.com)",
        ));
    }
    write_private(&state.youtube_cookies.path, content.as_bytes())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "cannot store the YouTube cookies");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "cannot store the cookies")
        })?;
    tracing::info!(user = %auth.user.username, cookies = count, dropped, "YouTube cookies uploaded");
    Ok(Json(json!({ "cookies": count, "dropped": dropped })))
}

async fn remove_cookies(State(state): State<AppState>, auth: Auth) -> ApiResult<StatusCode> {
    admin(&auth)?;
    match tokio::fs::remove_file(&state.youtube_cookies.path).await {
        Ok(()) => {
            tracing::info!(user = %auth.user.username, "YouTube cookies removed");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StatusCode::NO_CONTENT),
        Err(e) => Err(anyhow::Error::from(e).into()),
    }
}

/// The YouTube cookies of a Netscape cookie file, as a file yt-dlp reads (with the
/// header it requires), and how many cookies of other sites were left out (they would be sent
/// to whatever site a user plays from). Errors describe the first malformed line.
fn filter_cookies(content: &str) -> Result<(String, usize, usize), String> {
    let mut kept = String::from("# Netscape HTTP Cookie File\n");
    let (mut count, mut dropped) = (0, 0);
    for (number, raw) in content.lines().enumerate() {
        let raw = raw.trim_end_matches('\r');
        // `#HttpOnly_` marks a cookie line; other `#` lines are comments.
        let line = match raw.strip_prefix("#HttpOnly_") {
            Some(rest) => rest,
            None if raw.trim().is_empty() || raw.starts_with('#') => continue,
            None => raw,
        };
        let fields: Vec<&str> = line.split('\t').collect();
        let flag = |f: &str| f == "TRUE" || f == "FALSE";
        if fields.len() != 7
            || !flag(fields[1])
            || !flag(fields[3])
            || fields[4].is_empty()
            || !fields[4].bytes().all(|b| b.is_ascii_digit())
        {
            return Err(format!(
                "line {} is not in the Netscape cookie format (7 tab-separated fields)",
                number + 1
            ));
        }
        let domain = fields[0].trim_start_matches('.');
        // YouTube's own session cookies; Google's would also open Drive and other services.
        if domain == "youtube.com" || domain.ends_with(".youtube.com") {
            count += 1;
            kept.push_str(raw);
            kept.push('\n');
        } else {
            dropped += 1;
        }
    }
    Ok((kept, count, dropped))
}

/// Writes a file readable only by James, replacing the old one at once (never half written).
async fn write_private(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension(format!("{:016x}.tmp", rand::random::<u64>()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let result = async {
        let mut file = options.open(&temp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, content).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_youtube_cookies_are_kept_in_the_format_yt_dlp_reads() {
        let file = ".youtube.com\tTRUE\t/\tTRUE\t1893456000\tSID\tabc\n\
                    #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1893456000\tHSID\tdef\n\
                    .google.com\tTRUE\t/\tTRUE\t1893456000\tSID\tgoogle\n\
                    bank.example\tFALSE\t/\tFALSE\t0\tsession\tsecret\n\
                    \n";
        let (kept, count, dropped) = filter_cookies(file).unwrap();
        assert_eq!((count, dropped), (2, 2), "Google and other sites are dropped");
        assert!(
            kept.starts_with("# Netscape HTTP Cookie File\n"),
            "yt-dlp requires the header"
        );
        assert!(kept.contains("#HttpOnly_.youtube.com"));
        assert!(!kept.contains("google.com"));
        assert!(!kept.contains("bank.example"));
        assert!(filter_cookies("SID=abc; HSID=def")
            .unwrap_err()
            .contains("line 1"));
        assert!(
            filter_cookies(".youtube.com\tTRUE\t/\tTRUE\tsoon\tSID\tabc").is_err(),
            "expiry must be a number"
        );
        assert_eq!(
            filter_cookies("notyoutube.com\tTRUE\t/\tTRUE\t0\ta\tb\n")
                .unwrap()
                .1,
            0,
            "look-alike domains do not count"
        );
    }
}
