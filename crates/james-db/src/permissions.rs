//! Roles, permission grants and permission evaluation.
//!
//! Permissions are dotted strings (`playback.play`). A grant may be `*`
//! (everything) or `prefix.*`, and either global or limited to one instance.
//! Effective grants for a voice client are the union of the built-in
//! `everyone` role, the roles of the linked account and the roles mapped to
//! the client's platform groups.

use serde::{Deserialize, Serialize};

use crate::{conflict, Db, DbError, Result};

pub const EVERYONE_ROLE: &str = "everyone";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub permission: String,
    /// `None` = all instances.
    pub instance: Option<String>,
}

impl Grant {
    pub fn global(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            instance: None,
        }
    }

    pub fn allows(&self, permission: &str, instance: Option<&str>) -> bool {
        let instance_ok = match (&self.instance, instance) {
            (None, _) => true,
            (Some(granted), Some(wanted)) => granted == wanted,
            (Some(_), None) => false,
        };
        instance_ok && permission_matches(&self.permission, permission)
    }
}

pub fn permission_matches(granted: &str, wanted: &str) -> bool {
    granted == "*"
        || granted == wanted
        || granted.strip_suffix(".*").is_some_and(|prefix| {
            wanted
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

pub fn allowed(grants: &[Grant], permission: &str, instance: Option<&str>) -> bool {
    grants.iter().any(|g| g.allows(permission, instance))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub builtin: bool,
    pub grants: Vec<Grant>,
}

impl Db {
    pub async fn roles(&self) -> Result<Vec<Role>> {
        let rows: Vec<(i64, String, String, bool)> =
            sqlx::query_as("SELECT id, name, description, builtin FROM roles ORDER BY name")
                .fetch_all(self.pool())
                .await?;
        let mut roles = Vec::with_capacity(rows.len());
        for (id, name, description, builtin) in rows {
            roles.push(Role {
                id,
                name,
                description,
                builtin,
                grants: self.role_grants(id).await?,
            });
        }
        Ok(roles)
    }

    pub async fn role(&self, name: &str) -> Result<Role> {
        let (id, name, description, builtin): (i64, String, String, bool) =
            sqlx::query_as("SELECT id, name, description, builtin FROM roles WHERE name = ?")
                .bind(name)
                .fetch_optional(self.pool())
                .await?
                .ok_or(DbError::NotFound)?;
        Ok(Role {
            id,
            name,
            description,
            builtin,
            grants: self.role_grants(id).await?,
        })
    }

    async fn role_grants(&self, role_id: i64) -> Result<Vec<Grant>> {
        Ok(sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT permission, instance_id FROM role_permissions WHERE role_id = ? ORDER BY permission, instance_id",
        )
        .bind(role_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|(permission, instance)| Grant { permission, instance })
        .collect())
    }

    pub async fn create_role(&self, name: &str, description: &str) -> Result<Role> {
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            return Err(DbError::Invalid(
                "role names may only contain letters, digits, '-' and '_'".into(),
            ));
        }
        sqlx::query("INSERT INTO roles (name, description) VALUES (?, ?)")
            .bind(name)
            .bind(description)
            .execute(self.pool())
            .await
            .map_err(conflict)?;
        self.role(name).await
    }

    pub async fn delete_role(&self, name: &str) -> Result<()> {
        let role = self.role(name).await?;
        if role.builtin {
            return Err(DbError::Invalid("built-in roles cannot be deleted".into()));
        }
        sqlx::query("DELETE FROM roles WHERE id = ?")
            .bind(role.id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn set_role_grants(&self, name: &str, grants: &[Grant]) -> Result<()> {
        let role = self.role(name).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM role_permissions WHERE role_id = ?")
            .bind(role.id)
            .execute(&mut *tx)
            .await?;
        for grant in grants {
            sqlx::query(
                "INSERT OR IGNORE INTO role_permissions (role_id, permission, instance_id) VALUES (?, ?, ?)",
            )
            .bind(role.id)
            .bind(grant.permission.trim())
            .bind(&grant.instance)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Roles granted to members of a platform group.
    pub async fn set_group_roles(&self, platform: &str, group: &str, roles: &[String]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM group_roles WHERE platform = ? AND group_id = ?")
            .bind(platform)
            .bind(group)
            .execute(&mut *tx)
            .await?;
        for role in roles {
            let inserted = sqlx::query(
                "INSERT INTO group_roles (platform, group_id, role_id) SELECT ?, ?, id FROM roles WHERE name = ?",
            )
            .bind(platform)
            .bind(group)
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

    /// All group → roles mappings of a platform.
    pub async fn group_roles(&self, platform: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "SELECT g.group_id, r.name FROM group_roles g JOIN roles r ON r.id = g.role_id
             WHERE g.platform = ? ORDER BY g.group_id, r.name",
        )
        .bind(platform)
        .fetch_all(self.pool())
        .await?)
    }

    /// Grants of an account (admins get `*`).
    pub async fn user_grants(&self, user_id: i64) -> Result<Vec<Grant>> {
        let user = self.user(user_id).await?;
        if user.disabled {
            return Ok(Vec::new());
        }
        if user.is_admin {
            return Ok(vec![Grant::global("*")]);
        }
        self.grants_where(
            "SELECT rp.permission, rp.instance_id FROM role_permissions rp
                JOIN user_roles ur ON ur.role_id = rp.role_id WHERE ur.user_id = ?",
            |q| q.bind(user_id),
        )
        .await
    }

    /// Effective grants of a voice client.
    pub async fn client_grants(&self, platform: &str, uid: &str, groups: &[String]) -> Result<Vec<Grant>> {
        let mut grants = self
            .role(EVERYONE_ROLE)
            .await
            .map(|r| r.grants)
            .unwrap_or_default();
        if let Some(user) = self.user_by_identity(platform, uid).await? {
            grants.extend(self.user_grants(user.id).await?);
        }
        for group in groups {
            grants.extend(
                self.grants_where(
                    "SELECT rp.permission, rp.instance_id FROM role_permissions rp
                     JOIN group_roles g ON g.role_id = rp.role_id WHERE g.platform = ? AND g.group_id = ?",
                    |q| q.bind(platform).bind(group),
                )
                .await?,
            );
        }
        grants.sort_by(|a, b| (&a.permission, &a.instance).cmp(&(&b.permission, &b.instance)));
        grants.dedup();
        Ok(grants)
    }

    async fn grants_where<'q>(
        &self,
        sql: &'q str,
        bind: impl FnOnce(
            sqlx::query::QueryAs<
                'q,
                sqlx::Sqlite,
                (String, Option<String>),
                sqlx::sqlite::SqliteArguments<'q>,
            >,
        ) -> sqlx::query::QueryAs<
            'q,
            sqlx::Sqlite,
            (String, Option<String>),
            sqlx::sqlite::SqliteArguments<'q>,
        >,
    ) -> Result<Vec<Grant>> {
        Ok(bind(sqlx::query_as(sql))
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .map(|(permission, instance)| Grant { permission, instance })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matching() {
        assert!(permission_matches("*", "anything.here"));
        assert!(permission_matches("queue.*", "queue.manage"));
        assert!(!permission_matches("queue.*", "queuex.manage"));
        assert!(!permission_matches("queue.*", "queue"));
        assert!(permission_matches("playback.play", "playback.play"));
        let scoped = Grant {
            permission: "playback.*".into(),
            instance: Some("a".into()),
        };
        assert!(scoped.allows("playback.play", Some("a")));
        assert!(!scoped.allows("playback.play", Some("b")));
        assert!(!scoped.allows("playback.play", None));
    }

    #[tokio::test]
    async fn effective_client_grants() {
        let db = Db::memory().await.unwrap();
        // Restrict "everyone" to playing only.
        db.set_role_grants(EVERYONE_ROLE, &[Grant::global("playback.play")])
            .await
            .unwrap();
        let mods = db.create_role("mods", "").await.unwrap();
        db.set_role_grants(
            &mods.name,
            &[Grant {
                permission: "queue.*".into(),
                instance: Some("main".into()),
            }],
        )
        .await
        .unwrap();
        db.set_group_roles("teamspeak", "7", &["mods".into()])
            .await
            .unwrap();

        let guest = db.client_grants("teamspeak", "guest-uid", &[]).await.unwrap();
        assert!(allowed(&guest, "playback.play", Some("main")));
        assert!(!allowed(&guest, "queue.manage", Some("main")));

        let moderator = db
            .client_grants("teamspeak", "mod-uid", &["7".into()])
            .await
            .unwrap();
        assert!(allowed(&moderator, "queue.manage", Some("main")));
        assert!(!allowed(&moderator, "queue.manage", Some("other")));

        let admin = db.create_user("root", None, true).await.unwrap();
        db.link_identity("teamspeak", "admin-uid", admin.id, "Root")
            .await
            .unwrap();
        let admin_grants = db.client_grants("teamspeak", "admin-uid", &[]).await.unwrap();
        assert!(allowed(&admin_grants, "instance.delete", None));

        assert!(db.delete_role(EVERYONE_ROLE).await.is_err());
        db.delete_role("mods").await.unwrap();
        assert!(db.group_roles("teamspeak").await.unwrap().is_empty());
    }
}
