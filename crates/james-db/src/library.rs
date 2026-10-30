//! Local library index and loudness cache.

use serde::{Deserialize, Serialize};

use crate::{now, Db, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryTrack {
    pub id: i64,
    pub path: String,
    /// Names as shown and played: the edited ones, else the file's.
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub size: i64,
    pub mtime: i64,
    pub added_at: i64,
    /// The names were edited (see [`Db::rename_library_track`]).
    pub edited: bool,
    /// Names from the file's tags (or its file name).
    pub tags: TrackNames,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TrackNames {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Data for inserting or updating a library entry.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryEntry {
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub size: i64,
    pub mtime: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    path: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    duration_ms: Option<i64>,
    size: i64,
    mtime: i64,
    added_at: i64,
    custom_title: Option<String>,
    custom_artist: Option<String>,
    custom_album: Option<String>,
}

/// Names stored for a track, per field: `None` = the file's tag, `Some("")` (artist and album)
/// = explicitly none.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CustomNames {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl CustomNames {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.artist.is_none() && self.album.is_none()
    }
}

fn custom(value: Option<String>, tag: &Option<String>) -> Option<String> {
    match value {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(v),
        None => tag.clone(),
    }
}

fn map(r: Row) -> LibraryTrack {
    let tags = TrackNames {
        title: r.title,
        artist: r.artist,
        album: r.album,
    };
    LibraryTrack {
        id: r.id,
        path: r.path,
        edited: r.custom_title.is_some() || r.custom_artist.is_some() || r.custom_album.is_some(),
        title: r.custom_title.unwrap_or_else(|| tags.title.clone()),
        artist: custom(r.custom_artist, &tags.artist),
        album: custom(r.custom_album, &tags.album),
        duration_ms: r.duration_ms,
        size: r.size,
        mtime: r.mtime,
        added_at: r.added_at,
        tags,
    }
}

const COLUMNS: &str =
    "id, path, title, artist, album, duration_ms, size, mtime, added_at, custom_title, custom_artist, custom_album";
/// Search matches the shown names, the file's tags and the path.
const MATCHES: &str = "custom_title LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
    OR custom_artist LIKE ?1 ESCAPE '\\' OR artist LIKE ?1 ESCAPE '\\'
    OR custom_album LIKE ?1 ESCAPE '\\' OR album LIKE ?1 ESCAPE '\\'
    OR path LIKE ?1 ESCAPE '\\'";
const EDITED: &str = "(custom_title IS NOT NULL OR custom_artist IS NOT NULL OR custom_album IS NOT NULL)";

fn like_pattern(query: &str) -> String {
    format!(
        "%{}%",
        query
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

impl Db {
    pub async fn upsert_library_track(&self, entry: &LibraryEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO library_tracks (path, title, artist, album, duration_ms, size, mtime, added_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (path) DO UPDATE SET title = excluded.title, artist = excluded.artist,
                album = excluded.album, duration_ms = excluded.duration_ms, size = excluded.size,
                mtime = excluded.mtime",
        )
        .bind(&entry.path)
        .bind(&entry.title)
        .bind(&entry.artist)
        .bind(&entry.album)
        .bind(entry.duration_ms)
        .bind(entry.size)
        .bind(entry.mtime)
        .bind(now())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// (path, size, mtime) of all indexed files, for change detection while scanning.
    pub async fn library_index(&self) -> Result<Vec<(String, i64, i64)>> {
        Ok(sqlx::query_as("SELECT path, size, mtime FROM library_tracks")
            .fetch_all(self.pool())
            .await?)
    }

    pub async fn remove_library_paths(&self, paths: &[String]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        for path in paths {
            sqlx::query("DELETE FROM library_tracks WHERE path = ?")
                .bind(path)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn library_track(&self, id: i64) -> Result<Option<LibraryTrack>> {
        Ok(
            sqlx::query_as::<_, Row>(&format!("SELECT {COLUMNS} FROM library_tracks WHERE id = ?"))
                .bind(id)
                .fetch_optional(self.pool())
                .await?
                .map(map),
        )
    }

    pub async fn library_track_by_path(&self, path: &str) -> Result<Option<LibraryTrack>> {
        Ok(
            sqlx::query_as::<_, Row>(&format!("SELECT {COLUMNS} FROM library_tracks WHERE path = ?"))
                .bind(path)
                .fetch_optional(self.pool())
                .await?
                .map(map),
        )
    }

    /// Searches names and path (case-insensitive substring match).
    pub async fn search_library(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<LibraryTrack>> {
        Ok(sqlx::query_as::<_, Row>(&format!(
            "SELECT {COLUMNS} FROM library_tracks WHERE {MATCHES}
             ORDER BY NULLIF(COALESCE(custom_artist, artist), '') COLLATE NOCASE,
                NULLIF(COALESCE(custom_album, album), '') COLLATE NOCASE, path LIMIT ?2 OFFSET ?3"
        ))
        .bind(like_pattern(query))
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(map)
        .collect())
    }

    /// Number of tracks matching a search (see [`Db::search_library`]).
    pub async fn count_library(&self, query: &str) -> Result<i64> {
        Ok(
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM library_tracks WHERE {MATCHES}"))
                .bind(like_pattern(query))
                .fetch_one(self.pool())
                .await?,
        )
    }

    /// Sets the stored names of a track (all `None` = the file's tags again); `None` for an
    /// unknown id. Rescans keep them.
    pub async fn rename_library_track(&self, id: i64, names: &CustomNames) -> Result<Option<LibraryTrack>> {
        Ok(sqlx::query_as::<_, Row>(&format!(
            "UPDATE library_tracks SET custom_title = ?, custom_artist = ?, custom_album = ? WHERE id = ?
             RETURNING {COLUMNS}"
        ))
        .bind(&names.title)
        .bind(&names.artist)
        .bind(&names.album)
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .map(map))
    }

    /// Names to play the file at `path` with, if any were edited.
    pub async fn library_names(&self, path: &str) -> Result<Option<TrackNames>> {
        Ok(sqlx::query_as::<_, Row>(&format!(
            "SELECT {COLUMNS} FROM library_tracks WHERE path = ? AND {EDITED}"
        ))
        .bind(path)
        .fetch_optional(self.pool())
        .await?
        .map(map)
        .map(|t| TrackNames {
            title: t.title,
            artist: t.artist,
            album: t.album,
        }))
    }

    pub async fn library_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM library_tracks")
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn loudness(&self, source: &str, locator: &str) -> Result<Option<f32>> {
        let value: Option<f64> =
            sqlx::query_scalar("SELECT lufs FROM loudness WHERE source = ? AND locator = ?")
                .bind(source)
                .bind(locator)
                .fetch_optional(self.pool())
                .await?;
        Ok(value.map(|v| v as f32))
    }

    pub async fn save_loudness(&self, source: &str, locator: &str, lufs: f32) -> Result<()> {
        sqlx::query(
            "INSERT INTO loudness (source, locator, lufs, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (source, locator) DO UPDATE SET lufs = excluded.lufs, updated_at = excluded.updated_at",
        )
        .bind(source)
        .bind(locator)
        .bind(f64::from(lufs))
        .bind(now())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, title: &str, artist: &str) -> LibraryEntry {
        LibraryEntry {
            path: path.into(),
            title: title.into(),
            artist: Some(artist.into()),
            album: None,
            duration_ms: Some(1000),
            size: 10,
            mtime: 1,
        }
    }

    #[tokio::test]
    async fn library_search_and_loudness() {
        let db = Db::memory().await.unwrap();
        db.upsert_library_track(&entry("/m/a.mp3", "Alpha", "Zed"))
            .await
            .unwrap();
        db.upsert_library_track(&entry("/m/b_100%.mp3", "Beta", "Ann"))
            .await
            .unwrap();
        db.upsert_library_track(&entry("/m/a.mp3", "Alpha 2", "Zed"))
            .await
            .unwrap();
        assert_eq!(db.library_count().await.unwrap(), 2);
        assert_eq!(
            db.search_library("alpha", 10, 0).await.unwrap()[0].title,
            "Alpha 2"
        );
        assert_eq!(
            db.search_library("100%", 10, 0).await.unwrap().len(),
            1,
            "LIKE wildcards are escaped"
        );
        assert_eq!(db.count_library("alpha").await.unwrap(), 1);
        assert_eq!(
            db.search_library("", 10, 0).await.unwrap()[0].artist.as_deref(),
            Some("Ann")
        );

        let id = db.search_library("beta", 10, 0).await.unwrap()[0].id;
        let names = CustomNames {
            title: Some("Gamma".into()),
            artist: Some(String::new()),
            album: None,
        };
        let track = db.rename_library_track(id, &names).await.unwrap().unwrap();
        assert!(db.rename_library_track(999, &names).await.unwrap().is_none());
        assert!(track.edited);
        assert_eq!((track.title.as_str(), track.artist.as_deref()), ("Gamma", None));
        assert_eq!(track.tags.title, "Beta");
        assert_eq!(db.search_library("gamma", 10, 0).await.unwrap().len(), 1);
        assert_eq!(
            db.count_library("beta").await.unwrap(),
            1,
            "the file's tags still match"
        );
        assert_eq!(db.count_library("ann").await.unwrap(), 1);
        // Sorted with the tracks without an artist.
        assert_eq!(db.search_library("", 10, 0).await.unwrap()[0].id, id);
        // A rescan updates the tags; the edited names stay, the others follow the file.
        let mut retagged = entry("/m/b_100%.mp3", "Beta 2", "Ann");
        retagged.album = Some("Best of".into());
        db.upsert_library_track(&retagged).await.unwrap();
        let track = db.library_track(id).await.unwrap().unwrap();
        assert_eq!(
            (track.title.as_str(), track.tags.title.as_str()),
            ("Gamma", "Beta 2")
        );
        assert_eq!(track.album.as_deref(), Some("Best of"));
        assert_eq!(
            db.library_names("/m/b_100%.mp3").await.unwrap(),
            Some(TrackNames {
                title: "Gamma".into(),
                artist: None,
                album: Some("Best of".into())
            })
        );
        assert_eq!(db.library_names("/m/a.mp3").await.unwrap(), None);
        // Only the album edited: still used for playback.
        let album_only = CustomNames {
            album: Some("Live".into()),
            ..Default::default()
        };
        db.rename_library_track(id, &album_only).await.unwrap();
        assert_eq!(
            db.library_names("/m/b_100%.mp3")
                .await
                .unwrap()
                .unwrap()
                .album
                .as_deref(),
            Some("Live")
        );
        let track = db
            .rename_library_track(id, &CustomNames::default())
            .await
            .unwrap()
            .unwrap();
        assert!(!track.edited);
        assert_eq!(track.title, "Beta 2");

        db.remove_library_paths(&["/m/a.mp3".into()]).await.unwrap();
        assert_eq!(db.library_index().await.unwrap().len(), 1);

        assert_eq!(db.loudness("ytdlp", "x").await.unwrap(), None);
        db.save_loudness("ytdlp", "x", -12.5).await.unwrap();
        assert_eq!(db.loudness("ytdlp", "x").await.unwrap(), Some(-12.5));
    }
}
