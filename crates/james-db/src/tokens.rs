//! Login sessions and API tokens. Only SHA-256 hashes of tokens are stored.

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::users::User;
use crate::{now, Db, DbError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Session,
    Api,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            TokenKind::Session => "session",
            TokenKind::Api => "api",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    /// First 12 hex characters of the hash, used to identify a token in the UI.
    pub id: String,
    pub kind: TokenKind,
    pub name: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used: Option<i64>,
}

type TokenRow = (String, String, String, i64, Option<i64>, Option<i64>);

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl Db {
    /// Creates a token and returns its secret value (shown once).
    pub async fn create_token(
        &self,
        user_id: i64,
        kind: TokenKind,
        name: &str,
        lifetime_secs: Option<i64>,
    ) -> Result<String> {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = format!("jms_{}", hex::encode(bytes));
        let created = now();
        sqlx::query(
            "INSERT INTO tokens (token_hash, user_id, kind, name, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(hash(&token))
        .bind(user_id)
        .bind(kind.as_str())
        .bind(name)
        .bind(created)
        .bind(lifetime_secs.map(|l| created + l))
        .execute(self.pool())
        .await?;
        Ok(token)
    }

    /// Resolves a token to its (enabled) user and updates its last use.
    pub async fn token_user(&self, token: &str) -> Result<Option<User>> {
        let hashed = hash(token);
        let row: Option<(i64, Option<i64>)> =
            sqlx::query_as("SELECT user_id, expires_at FROM tokens WHERE token_hash = ?")
                .bind(&hashed)
                .fetch_optional(self.pool())
                .await?;
        let Some((user_id, expires)) = row else {
            return Ok(None);
        };
        let current = now();
        if expires.is_some_and(|e| e <= current) {
            sqlx::query("DELETE FROM tokens WHERE token_hash = ?")
                .bind(&hashed)
                .execute(self.pool())
                .await?;
            return Ok(None);
        }
        sqlx::query("UPDATE tokens SET last_used = ? WHERE token_hash = ?")
            .bind(current)
            .bind(&hashed)
            .execute(self.pool())
            .await?;
        let user = self.user(user_id).await?;
        Ok((!user.disabled).then_some(user))
    }

    /// Revokes all tokens of a user except `keep` (e.g. the caller's own session).
    pub async fn revoke_user_tokens(&self, user_id: i64, keep: Option<&str>) -> Result<u64> {
        let keep = keep.map(hash).unwrap_or_default();
        Ok(
            sqlx::query("DELETE FROM tokens WHERE user_id = ? AND token_hash != ?")
                .bind(user_id)
                .bind(keep)
                .execute(self.pool())
                .await?
                .rows_affected(),
        )
    }

    pub async fn revoke_token(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM tokens WHERE token_hash = ?")
            .bind(hash(token))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Revokes a token by the id shown in [`TokenInfo`].
    pub async fn revoke_token_id(&self, user_id: i64, id: &str) -> Result<()> {
        if id.len() < 12 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(DbError::Invalid("invalid token id".into()));
        }
        let result = sqlx::query("DELETE FROM tokens WHERE user_id = ? AND substr(token_hash, 1, 12) = ?")
            .bind(user_id)
            .bind(&id[..12])
            .execute(self.pool())
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub async fn tokens(&self, user_id: i64) -> Result<Vec<TokenInfo>> {
        let rows: Vec<TokenRow> = sqlx::query_as(
            "SELECT token_hash, kind, name, created_at, expires_at, last_used FROM tokens
             WHERE user_id = ? ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(hash, kind, name, created_at, expires_at, last_used)| TokenInfo {
                    id: hash[..12].to_owned(),
                    kind: if kind == "api" {
                        TokenKind::Api
                    } else {
                        TokenKind::Session
                    },
                    name,
                    created_at,
                    expires_at,
                    last_used,
                },
            )
            .collect())
    }

    pub async fn purge_expired_tokens(&self) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM tokens WHERE expires_at IS NOT NULL AND expires_at <= ?")
                .bind(now())
                .execute(self.pool())
                .await?
                .rows_affected(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_lifecycle() {
        let db = Db::memory().await.unwrap();
        let user = db.create_user("t", None, false).await.unwrap();
        let token = db
            .create_token(user.id, TokenKind::Api, "ci", None)
            .await
            .unwrap();
        assert!(token.starts_with("jms_"));
        assert_eq!(db.token_user(&token).await.unwrap().unwrap().id, user.id);
        assert!(db.token_user("jms_invalid").await.unwrap().is_none());

        let expired = db
            .create_token(user.id, TokenKind::Session, "", Some(-1))
            .await
            .unwrap();
        assert!(db.token_user(&expired).await.unwrap().is_none());

        let list = db.tokens(user.id).await.unwrap();
        assert_eq!(list.len(), 1, "expired token was deleted on use");
        db.revoke_token_id(user.id, &list[0].id).await.unwrap();
        assert!(db.token_user(&token).await.unwrap().is_none());
    }
}
