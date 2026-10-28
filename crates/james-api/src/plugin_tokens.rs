//! Tokens for plugin web pages.
//!
//! Plugin pages are served sandboxed (opaque origin), so they cannot use the session cookie.
//! The web interface requests a token bound to one user, plugin and instance and passes it in
//! the page's URL fragment. The token is only accepted by that plugin's endpoints on that
//! instance, only while the session that requested it is valid (logout or revocation ends it),
//! and every request is still checked against the user's current permissions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

pub const PREFIX: &str = "jpt_";
const LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_TOKENS: usize = 10_000;
/// Tokens per user; the oldest is replaced.
const MAX_PER_USER: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct PluginToken {
    pub user: i64,
    pub plugin: String,
    pub instance: String,
    /// The session (API token) that requested it.
    pub session: String,
    expires: Instant,
}

#[derive(Clone, Default)]
pub struct PluginTokens {
    tokens: Arc<Mutex<HashMap<String, PluginToken>>>,
}

fn oldest(tokens: &HashMap<String, PluginToken>, filter: impl Fn(&PluginToken) -> bool) -> Option<String> {
    tokens
        .iter()
        .filter(|(_, t)| filter(t))
        .min_by_key(|(_, t)| t.expires)
        .map(|(k, _)| k.clone())
}

impl PluginTokens {
    pub fn issue(&self, user: i64, session: &str, plugin: &str, instance: &str) -> (String, Duration) {
        let now = Instant::now();
        let mut tokens = self.tokens.lock();
        tokens.retain(|_, t| t.expires > now);
        // One user cannot crowd out others: the per-user limit applies first.
        if tokens.values().filter(|t| t.user == user).count() >= MAX_PER_USER {
            if let Some(key) = oldest(&tokens, |t| t.user == user) {
                tokens.remove(&key);
            }
        }
        if tokens.len() >= MAX_TOKENS {
            if let Some(key) = oldest(&tokens, |_| true) {
                tokens.remove(&key);
            }
        }
        let token = format!("{PREFIX}{}", crate::linking::random_code(32));
        tokens.insert(
            token.clone(),
            PluginToken {
                user,
                plugin: plugin.to_owned(),
                instance: instance.to_owned(),
                session: session.to_owned(),
                expires: now + LIFETIME,
            },
        );
        (token, LIFETIME)
    }

    pub fn resolve(&self, token: &str) -> Option<PluginToken> {
        let tokens = self.tokens.lock();
        tokens.get(token).filter(|t| t.expires > Instant::now()).cloned()
    }

    /// Ends all tokens of a user (password change, deletion).
    pub fn revoke_user(&self, user: i64) {
        self.tokens.lock().retain(|_, t| t.user != user);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_bound_and_revocable() {
        let tokens = PluginTokens::default();
        let (token, _) = tokens.issue(7, "session", "now-playing", "music");
        assert!(token.starts_with(PREFIX));
        let resolved = tokens.resolve(&token).unwrap();
        assert_eq!(
            (
                resolved.user,
                resolved.plugin.as_str(),
                resolved.instance.as_str(),
                resolved.session.as_str()
            ),
            (7, "now-playing", "music", "session")
        );
        assert!(tokens.resolve("jpt_unknown").is_none());
        tokens.revoke_user(7);
        assert!(tokens.resolve(&token).is_none());
    }

    #[test]
    fn users_are_capped_without_affecting_others() {
        let tokens = PluginTokens::default();
        let (other, _) = tokens.issue(1, "s1", "p", "i");
        let (first, _) = tokens.issue(2, "s2", "p", "i");
        for _ in 0..MAX_PER_USER {
            tokens.issue(2, "s2", "p", "i");
        }
        assert!(tokens.resolve(&first).is_none(), "oldest of the user replaced");
        assert!(tokens.resolve(&other).is_some(), "other users keep theirs");
    }
}
