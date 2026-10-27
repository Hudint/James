//! Local library index and loudness cache.

use serde::{Deserialize, Serialize};

use crate::{now, Db, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryTrack {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub size: i64,
    pub mtime: i64,
    pub added_at: i64,
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

type Row = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    i64,
    i64,
    i64,
);

fn map(r: Row) -> LibraryTrack {
    LibraryTrack {
        id: r.0,
        path: r.1,
        title: r.2,
        artist: r.3,
        album: r.4,
        duration_ms: r.5,
        size: r.6,
        mtime: r.7,
        added_at: r.8,
    }
}

const COLUMNS: &str = "id, path, title, artist, album, duration_ms, size, mtime, added_at";

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

    /// Searches title, artist, album and path (case-insensitive substring match).
    pub async fn search_library(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<LibraryTrack>> {
        let pattern = format!(
            "%{}%",
            query
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        Ok(sqlx::query_as::<_, Row>(&format!(
            "SELECT {COLUMNS} FROM library_tracks
             WHERE title LIKE ?1 ESCAPE '\\' OR artist LIKE ?1 ESCAPE '\\' OR album LIKE ?1 ESCAPE '\\'
                OR path LIKE ?1 ESCAPE '\\'
             ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, path LIMIT ?2 OFFSET ?3"
        ))
        .bind(pattern)
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
        let pattern = format!(
            "%{}%",
            query
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM library_tracks WHERE title LIKE ?1 ESCAPE '\\' OR artist LIKE ?1 ESCAPE '\\'
                OR album LIKE ?1 ESCAPE '\\' OR path LIKE ?1 ESCAPE '\\'",
        )
        .bind(pattern)
        .fetch_one(self.pool())
        .await?)
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
        db.remove_library_paths(&["/m/a.mp3".into()]).await.unwrap();
        assert_eq!(db.library_index().await.unwrap().len(), 1);

        assert_eq!(db.loudness("ytdlp", "x").await.unwrap(), None);
        db.save_loudness("ytdlp", "x", -12.5).await.unwrap();
        assert_eq!(db.loudness("ytdlp", "x").await.unwrap(), Some(-12.5));
    }
}
