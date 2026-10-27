//! User accounts.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{conflict, now, Db, DbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub has_password: bool,
    pub oidc_subject: Option<String>,
    pub created_at: i64,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub platform: String,
    pub uid: String,
    pub user_id: i64,
    pub name: String,
}

pub fn hash_password(password: &str) -> Result<String> {
    if password.len() < 8 {
        return Err(DbError::Invalid("password must be at least 8 characters".into()));
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| DbError::Invalid(e.to_string()))
}

fn verify(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

const SELECT: &str =
    "SELECT u.id, u.username, u.is_admin, u.disabled, u.password_hash IS NOT NULL AS has_password,
        u.oidc_subject, u.created_at,
        COALESCE((SELECT json_group_array(r.name) FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                  WHERE ur.user_id = u.id), '[]') AS roles
    FROM users u";

fn map(row: sqlx::sqlite::SqliteRow) -> Result<User> {
    let roles: String = row.try_get("roles")?;
    let mut roles: Vec<String> = serde_json::from_str(&roles)?;
    roles.sort();
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        is_admin: row.try_get("is_admin")?,
        disabled: row.try_get("disabled")?,
        has_password: row.try_get("has_password")?,
        oidc_subject: row.try_get("oidc_subject")?,
        created_at: row.try_get("created_at")?,
        roles,
    })
}

impl Db {
    pub async fn create_user(&self, username: &str, password: Option<&str>, is_admin: bool) -> Result<User> {
        let username = username.trim();
        if username.is_empty() || username.len() > 64 {
            return Err(DbError::Invalid("username must be 1-64 characters".into()));
        }
        let hash = password.map(hash_password).transpose()?;
        let id = sqlx::query(
            "INSERT INTO users (username, password_hash, is_admin, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(username)
        .bind(hash)
        .bind(is_admin)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(conflict)?
        .last_insert_rowid();
        self.user(id).await
    }

    pub async fn user(&self, id: i64) -> Result<User> {
        let row = sqlx::query(&format!("{SELECT} WHERE u.id = ?"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(DbError::NotFound)?;
        map(row)
    }

    pub async fn user_by_name(&self, username: &str) -> Result<User> {
        let row = sqlx::query(&format!("{SELECT} WHERE u.username = ?"))
            .bind(username.trim())
            .fetch_optional(self.pool())
            .await?
            .ok_or(DbError::NotFound)?;
        map(row)
    }

    pub async fn user_by_oidc(&self, subject: &str) -> Result<User> {
        let row = sqlx::query(&format!("{SELECT} WHERE u.oidc_subject = ?"))
            .bind(subject)
            .fetch_optional(self.pool())
            .await?
            .ok_or(DbError::NotFound)?;
        map(row)
    }

    pub async fn users(&self) -> Result<Vec<User>> {
        sqlx::query(&format!("{SELECT} ORDER BY u.username"))
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .map(map)
            .collect()
    }

    pub async fn user_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(self.pool())
            .await?)
    }

    /// Checks username and password; disabled users and users without a password never match.
    pub async fn authenticate(&self, username: &str, password: &str) -> Result<Option<User>> {
        let row: Option<(i64, Option<String>, bool)> =
            sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
                .bind(username.trim())
                .fetch_optional(self.pool())
                .await?;
        match row {
            Some((id, Some(hash), false)) if verify(&hash, password) => Ok(Some(self.user(id).await?)),
            Some(_) => Ok(None),
            None => {
                // Spend comparable time to not reveal whether the user exists.
                let _ = verify("$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$aGFzaA", password);
                Ok(None)
            }
        }
    }

    pub async fn set_password(&self, id: i64, password: Option<&str>) -> Result<()> {
        let hash = password.map(hash_password).transpose()?;
        self.update_user_field(id, "password_hash", hash).await
    }

    pub async fn set_admin(&self, id: i64, is_admin: bool) -> Result<()> {
        self.update_user_field(id, "is_admin", is_admin).await
    }

    pub async fn set_disabled(&self, id: i64, disabled: bool) -> Result<()> {
        self.update_user_field(id, "disabled", disabled).await
    }

    pub async fn set_oidc_subject(&self, id: i64, subject: Option<&str>) -> Result<()> {
        self.update_user_field(id, "oidc_subject", subject.map(str::to_owned))
            .await
    }

    async fn update_user_field<T>(&self, id: i64, column: &'static str, value: T) -> Result<()>
    where
        T: for<'q> sqlx::Encode<'q, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send,
    {
        let result = sqlx::query(&format!("UPDATE users SET {column} = ? WHERE id = ?"))
            .bind(value)
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(conflict)?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub async fn delete_user(&self, id: i64) -> Result<()> {
        let result = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub async fn set_user_roles(&self, id: i64, roles: &[String]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM user_roles WHERE user_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for role in roles {
            let inserted = sqlx::query(
                "INSERT INTO user_roles (user_id, role_id) SELECT ?, id FROM roles WHERE name = ?",
            )
            .bind(id)
            .bind(role)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() == 0 {
                return Err(DbError::Invalid(format!("unknown role '{role}'")));
            }
        }
        tx.commit().await?;
        Ok(())
    }

    // Platform identities ---------------------------------------------------------

    pub async fn link_identity(&self, platform: &str, uid: &str, user_id: i64, name: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO identities (platform, uid, user_id, name, created_at) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (platform, uid) DO UPDATE SET user_id = excluded.user_id, name = excluded.name",
        )
        .bind(platform)
        .bind(uid)
        .bind(user_id)
        .bind(name)
        .bind(now())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn unlink_identity(&self, platform: &str, uid: &str) -> Result<()> {
        sqlx::query("DELETE FROM identities WHERE platform = ? AND uid = ?")
            .bind(platform)
            .bind(uid)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn identities(&self, user_id: i64) -> Result<Vec<Identity>> {
        Ok(sqlx::query_as::<_, (String, String, i64, String)>(
            "SELECT platform, uid, user_id, name FROM identities WHERE user_id = ? ORDER BY platform, uid",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|(platform, uid, user_id, name)| Identity {
            platform,
            uid,
            user_id,
            name,
        })
        .collect())
    }

    pub async fn user_by_identity(&self, platform: &str, uid: &str) -> Result<Option<User>> {
        let id: Option<i64> =
            sqlx::query_scalar("SELECT user_id FROM identities WHERE platform = ? AND uid = ?")
                .bind(platform)
                .bind(uid)
                .fetch_optional(self.pool())
                .await?;
        match id {
            Some(id) => Ok(Some(self.user(id).await?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_authenticate_and_roles() {
        let db = Db::memory().await.unwrap();
        let user = db
            .create_user("Alice", Some("correct horse"), false)
            .await
            .unwrap();
        assert!(user.has_password);
        assert!(matches!(
            db.create_user("alice", None, false).await,
            Err(DbError::Conflict)
        ));
        assert!(db.authenticate("alice", "correct horse").await.unwrap().is_some());
        assert!(db
            .authenticate("alice", "wrong password")
            .await
            .unwrap()
            .is_none());
        assert!(db.authenticate("bob", "whatever1").await.unwrap().is_none());

        db.set_disabled(user.id, true).await.unwrap();
        assert!(db.authenticate("alice", "correct horse").await.unwrap().is_none());

        db.set_user_roles(user.id, &["dj".into(), "admin".into()])
            .await
            .unwrap();
        assert_eq!(db.user(user.id).await.unwrap().roles, ["admin", "dj"]);
        assert!(matches!(
            db.set_user_roles(user.id, &["nope".into()]).await,
            Err(DbError::Invalid(_))
        ));
        assert_eq!(
            db.user(user.id).await.unwrap().roles,
            ["admin", "dj"],
            "failed update is rolled back"
        );
    }

    #[tokio::test]
    async fn identities_link_to_users() {
        let db = Db::memory().await.unwrap();
        let user = db.create_user("bob", None, false).await.unwrap();
        db.link_identity("teamspeak", "uid=", user.id, "Bob")
            .await
            .unwrap();
        assert_eq!(
            db.user_by_identity("teamspeak", "uid=")
                .await
                .unwrap()
                .unwrap()
                .id,
            user.id
        );
        assert_eq!(db.identities(user.id).await.unwrap().len(), 1);
        db.delete_user(user.id).await.unwrap();
        assert!(
            db.user_by_identity("teamspeak", "uid=").await.unwrap().is_none(),
            "cascade"
        );
    }

    #[test]
    fn rejects_short_passwords() {
        assert!(hash_password("short").is_err());
    }
}
