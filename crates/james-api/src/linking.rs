//! One-time codes for linking a voice platform identity to an account.
//!
//! The user requests a code in the web UI and sends `!link <code>` to the bot;
//! the bot then links the sender's platform identity to the account.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::Rng;

const CODE_LIFETIME: Duration = Duration::from_secs(600);
/// 10 characters from a 31-symbol alphabet (~49 bits): guessing is infeasible even with
/// many identities sending `!link` in parallel.
const CODE_LENGTH: usize = 10;
/// Unambiguous characters (no 0/O, 1/I/L).
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// A random code from an unambiguous alphabet.
pub fn random_code(length: usize) -> String {
    let mut rng = rand::rng();
    (0..length)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

#[derive(Clone, Default)]
pub struct LinkCodes {
    codes: Arc<Mutex<HashMap<String, (i64, Instant)>>>,
}

impl LinkCodes {
    /// Creates a new code for `user_id`, replacing an older code of that user.
    pub fn issue(&self, user_id: i64) -> String {
        let mut codes = self.codes.lock().unwrap();
        codes.retain(|_, (user, created)| *user != user_id && created.elapsed() < CODE_LIFETIME);
        let code = loop {
            let candidate = random_code(CODE_LENGTH);
            if !codes.contains_key(&candidate) {
                break candidate;
            }
        };
        codes.insert(code.clone(), (user_id, Instant::now()));
        code
    }

    /// Consumes a code; returns the user it was issued for.
    pub fn redeem(&self, code: &str) -> Option<i64> {
        let mut codes = self.codes.lock().unwrap();
        let (user, created) = codes.remove(&code.trim().to_ascii_uppercase())?;
        (created.elapsed() < CODE_LIFETIME).then_some(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_single_use() {
        let links = LinkCodes::default();
        let first = links.issue(1);
        let second = links.issue(1);
        assert_eq!(links.redeem(&first), None, "replaced by the newer code");
        assert_eq!(second.len(), 10);
        assert_eq!(links.redeem(&second.to_lowercase()), Some(1), "case-insensitive");
        assert_eq!(links.redeem(&second), None);
    }
}
