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

/// Longer passwords are rejected so hashing cannot be abused for denial of service.
pub const MAX_PASSWORD_LEN: usize = 1024;

/// argon2 is CPU and memory heavy: run it off the async workers, a few at a time.
static HASH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub fn hash_password(password: &str) -> Result<String> {
    if password.chars().count() < 8 {
        return Err(DbError::Invalid("password must be at least 8 characters".into()));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(DbError::Invalid("password is too long".into()));
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| DbError::Invalid(e.to_string()))
}

fn verify(hash: &str, password: &str) -> bool {
    password.len() <= MAX_PASSWORD_LEN
        && PasswordHash::new(hash)
            .is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// A real hash (same parameters as stored ones) to verify against for unknown users.
fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| hash_password("dummy-password-for-timing").expect("valid password"));
    &DUMMY
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let _slot = HASH_SLOTS
        .acquire()
        .await
        .map_err(|_| DbError::Invalid("shutting down".into()))?;
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DbError::Invalid(e.to_string()))
}

async fn hash_password_async(password: Option<&str>) -> Result<Option<String>> {
    match password {
        Some(p) => {
            let p = p.to_owned();
            Ok(Some(blocking(move || hash_password(&p)).await??))
        }
        None => Ok(None),
    }
}

async fn verify_async(hash: String, password: &str) -> Result<bool> {
    let password = password.to_owned();
    blocking(move || verify(&hash, &password)).await
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

/// What a login was checked against; a session is only created while it still holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Proof {
    /// The password hash the password was verified with.
    Password(String),
    /// The single sign-on subject the account was found by.
    Oidc(String),
}

impl Db {
    pub async fn create_user(&self, username: &str, password: Option<&str>, is_admin: bool) -> Result<User> {
        let username = username.trim();
        if username.is_empty() || username.len() > 64 {
            return Err(DbError::Invalid("username must be 1-64 characters".into()));
        }
        let hash = hash_password_async(password).await?;
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

    /// Creates an account for a single sign-on identity with its roles, all or nothing.
    /// [`DbError::Conflict`] if the name or the subject is taken.
    pub async fn create_oidc_user(&self, username: &str, subject: &str, roles: &[String]) -> Result<User> {
        let username = username.trim();
        if username.is_empty() || username.len() > 64 {
            return Err(DbError::Invalid("username must be 1-64 characters".into()));
        }
        let mut tx = self.pool().begin().await?;
        let id = sqlx::query(
            "INSERT INTO users (username, password_hash, is_admin, created_at, oidc_subject) VALUES (?, NULL, 0, ?, ?)",
        )
        .bind(username)
        .bind(now())
        .bind(subject)
        .execute(&mut *tx)
        .await
        .map_err(conflict)?
        .last_insert_rowid();
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
        self.user(id).await
    }

    /// Removes administrator rights unless the account is the last enabled administrator
    /// (atomic); `false` if it was kept.
    pub async fn demote_admin(&self, id: i64) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE users SET is_admin = 0 WHERE id = ? AND is_admin = 1 \
             AND (SELECT COUNT(*) FROM users WHERE is_admin = 1 AND disabled = 0 AND id != ?) > 0",
        )
        .bind(id)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Creates the first administrator, atomically failing with [`DbError::Conflict`] if any
    /// account exists already (concurrent setups cannot create two admins).
    pub async fn create_first_admin(&self, username: &str, password: &str) -> Result<User> {
        let username = username.trim();
        if username.is_empty() || username.len() > 64 {
            return Err(DbError::Invalid("username must be 1-64 characters".into()));
        }
        let hash = hash_password_async(Some(password)).await?;
        let result = sqlx::query(
            "INSERT INTO users (username, password_hash, is_admin, created_at)
             SELECT ?, ?, 1, ? WHERE NOT EXISTS (SELECT 1 FROM users)",
        )
        .bind(username)
        .bind(hash)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(conflict)?;
        if result.rows_affected() == 0 {
            return Err(DbError::Conflict);
        }
        self.user(result.last_insert_rowid()).await
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
        Ok(self
            .password_login(username, password)
            .await?
            .map(|(user, _)| user))
    }

    /// Checks a password login; also returns the proof for [`Db::create_session`], so a
    /// password changed during the (slow) check does not yield a session.
    pub async fn password_login(&self, username: &str, password: &str) -> Result<Option<(User, Proof)>> {
        let row: Option<(i64, Option<String>, bool)> =
            sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
                .bind(username.trim())
                .fetch_optional(self.pool())
                .await?;
        match row {
            Some((id, Some(hash), false)) => {
                if verify_async(hash.clone(), password).await? {
                    Ok(Some((self.user(id).await?, Proof::Password(hash))))
                } else {
                    Ok(None)
                }
            }
            _ => {
                // Without a usable account, spend the same time so the response does not
                // reveal whether the username exists.
                verify_async(dummy_hash().to_owned(), password).await?;
                Ok(None)
            }
        }
    }

    pub async fn set_password(&self, id: i64, password: Option<&str>) -> Result<()> {
        let hash = hash_password_async(password).await?;
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
    async fn no_session_after_the_credentials_changed() {
        let db = Db::memory().await.unwrap();
        let user = db
            .create_user("alice", Some("old-password"), false)
            .await
            .unwrap();
        let (_, proof) = db.password_login("alice", "old-password").await.unwrap().unwrap();
        assert!(db.create_session(user.id, &proof, 60).await.unwrap().is_some());
        // The password is reset while a login is being checked.
        db.set_password(user.id, Some("new-password")).await.unwrap();
        assert_eq!(db.create_session(user.id, &proof, 60).await.unwrap(), None);
        let oidc = db.create_oidc_user("bob", "https://idp#bob", &[]).await.unwrap();
        let proof = Proof::Oidc("https://idp#bob".into());
        assert!(db.create_session(oidc.id, &proof, 60).await.unwrap().is_some());
        db.set_oidc_subject(oidc.id, None).await.unwrap();
        assert_eq!(
            db.create_session(oidc.id, &proof, 60).await.unwrap(),
            None,
            "unlinked"
        );
    }

    #[tokio::test]
    async fn the_last_admin_is_never_demoted() {
        let db = Db::memory().await.unwrap();
        let a = db.create_user("a", None, true).await.unwrap();
        let b = db.create_user("b", None, true).await.unwrap();
        assert!(db.demote_admin(a.id).await.unwrap());
        assert!(
            !db.demote_admin(b.id).await.unwrap(),
            "b is the last administrator"
        );
        assert!(db.user(b.id).await.unwrap().is_admin);
        // A disabled administrator does not count.
        let c = db.create_user("c", None, true).await.unwrap();
        db.set_disabled(c.id, true).await.unwrap();
        assert!(!db.demote_admin(b.id).await.unwrap());
    }

    #[tokio::test]
    async fn sso_accounts_are_created_with_roles_or_not_at_all() {
        let db = Db::memory().await.unwrap();
        let user = db
            .create_oidc_user("alice", "https://idp#1", &["dj".into()])
            .await
            .unwrap();
        assert_eq!(user.oidc_subject.as_deref(), Some("https://idp#1"));
        assert!(!user.has_password);
        assert!(matches!(
            db.create_oidc_user("alice2", "https://idp#1", &[]).await,
            Err(DbError::Conflict)
        ));
        assert!(matches!(
            db.create_oidc_user("bob", "https://idp#2", &["nope".into()])
                .await,
            Err(DbError::Invalid(_))
        ));
        assert!(
            matches!(db.user_by_oidc("https://idp#2").await, Err(DbError::NotFound)),
            "nothing is left of a failed creation"
        );
    }

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
    fn dummy_hash_is_a_real_hash() {
        assert!(PasswordHash::new(dummy_hash()).is_ok());
        assert!(!verify(dummy_hash(), "anything"));
    }

    #[test]
    fn rejects_short_and_huge_passwords() {
        assert!(hash_password("short").is_err());
        assert!(hash_password(&"x".repeat(MAX_PASSWORD_LEN + 1)).is_err());
    }

    #[tokio::test]
    async fn first_admin_only_once() {
        let db = Db::memory().await.unwrap();
        let (a, b) = tokio::join!(
            db.create_first_admin("one", "password-one"),
            db.create_first_admin("two", "password-two")
        );
        assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(), 1);
        assert_eq!(db.user_count().await.unwrap(), 1);
    }
}
