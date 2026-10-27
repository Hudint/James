//! Saved playlists. Tracks are stored as JSON so any source can be saved.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{now, Db, DbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub owner_id: Option<i64>,
    pub track_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Db {
    pub async fn create_playlist(&self, name: &str, owner_id: Option<i64>) -> Result<Playlist> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Invalid("playlist name must not be empty".into()));
        }
        let time = now();
        let id =
            sqlx::query("INSERT INTO playlists (name, owner_id, created_at, updated_at) VALUES (?, ?, ?, ?)")
                .bind(name)
                .bind(owner_id)
                .bind(time)
                .bind(time)
                .execute(self.pool())
                .await?
                .last_insert_rowid();
        self.playlist(id).await
    }

    pub async fn playlist(&self, id: i64) -> Result<Playlist> {
        self.playlists_where("WHERE p.id = ?", Some(id))
            .await?
            .pop()
            .ok_or(DbError::NotFound)
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        self.playlists_where("", None).await
    }

    async fn playlists_where(&self, filter: &str, id: Option<i64>) -> Result<Vec<Playlist>> {
        let sql = format!(
            "SELECT p.id, p.name, p.owner_id,
                (SELECT COUNT(*) FROM playlist_entries e WHERE e.playlist_id = p.id),
                p.created_at, p.updated_at
             FROM playlists p {filter} ORDER BY p.name COLLATE NOCASE"
        );
        let mut query = sqlx::query_as::<_, (i64, String, Option<i64>, i64, i64, i64)>(&sql);
        if let Some(id) = id {
            query = query.bind(id);
        }
        Ok(query
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .map(
                |(id, name, owner_id, track_count, created_at, updated_at)| Playlist {
                    id,
                    name,
                    owner_id,
                    track_count,
                    created_at,
                    updated_at,
                },
            )
            .collect())
    }

    pub async fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        let result = sqlx::query("UPDATE playlists SET name = ?, updated_at = ? WHERE id = ?")
            .bind(name.trim())
            .bind(now())
            .bind(id)
            .execute(self.pool())
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub async fn delete_playlist(&self, id: i64) -> Result<()> {
        let result = sqlx::query("DELETE FROM playlists WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub async fn playlist_tracks<T: DeserializeOwned>(&self, id: i64) -> Result<Vec<T>> {
        self.playlist(id).await?;
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT track FROM playlist_entries WHERE playlist_id = ? ORDER BY position")
                .bind(id)
                .fetch_all(self.pool())
                .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).map_err(Into::into))
            .collect()
    }

    /// Replaces all tracks of a playlist.
    pub async fn set_playlist_tracks<T: Serialize>(&self, id: i64, tracks: &[T]) -> Result<()> {
        self.playlist(id).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM playlist_entries WHERE playlist_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for (position, track) in tracks.iter().enumerate() {
            sqlx::query("INSERT INTO playlist_entries (playlist_id, position, track) VALUES (?, ?, ?)")
                .bind(id)
                .bind(position as i64)
                .bind(serde_json::to_string(track)?)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE playlists SET updated_at = ? WHERE id = ?")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Appends tracks to a playlist.
    pub async fn append_playlist_tracks<T: Serialize>(&self, id: i64, tracks: &[T]) -> Result<()> {
        self.playlist(id).await?;
        let mut tx = self.pool().begin().await?;
        let start: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_entries WHERE playlist_id = ?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        for (offset, track) in tracks.iter().enumerate() {
            sqlx::query("INSERT INTO playlist_entries (playlist_id, position, track) VALUES (?, ?, ?)")
                .bind(id)
                .bind(start + offset as i64)
                .bind(serde_json::to_string(track)?)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE playlists SET updated_at = ? WHERE id = ?")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn playlist_tracks() {
        let db = Db::memory().await.unwrap();
        let list = db.create_playlist("Party", None).await.unwrap();
        db.append_playlist_tracks(list.id, &["a", "b"]).await.unwrap();
        db.append_playlist_tracks(list.id, &["c"]).await.unwrap();
        let tracks: Vec<String> = db.playlist_tracks(list.id).await.unwrap();
        assert_eq!(tracks, ["a", "b", "c"]);
        assert_eq!(db.playlist(list.id).await.unwrap().track_count, 3);
        db.set_playlist_tracks(list.id, &["z"]).await.unwrap();
        let tracks: Vec<String> = db.playlist_tracks(list.id).await.unwrap();
        assert_eq!(tracks, ["z"]);
        db.delete_playlist(list.id).await.unwrap();
        assert!(db.playlists().await.unwrap().is_empty());
    }
}
