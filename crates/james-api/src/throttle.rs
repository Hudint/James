//! Brute-force protection for logins.
//!
//! Failures are counted per (username, client network) and per client network, with hard
//! blocks: the tight limit applies to the pair, so an attacker cannot lock other people out
//! of their accounts, and the per-network limit stops spraying many usernames. IPv6 clients
//! are grouped by /64, since one host can use a whole /64. Failures per username (from many
//! networks) only *slow down* further attempts for that username instead of blocking it,
//! so the owner can still log in.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use parking_lot::Mutex;

const WINDOW: Duration = Duration::from_secs(600);
const FIRST_BLOCK: Duration = Duration::from_secs(60);
const MAX_BLOCK: Duration = Duration::from_secs(1800);
/// Bound on tracked keys, so an attacker cannot exhaust memory with random usernames.
const MAX_KEYS: usize = 50_000;

const PAIR_LIMIT: u32 = 5;
const IP_LIMIT: u32 = 30;
const USER_LIMIT: u32 = 100;
/// Extra response time for logins to a username under distributed attack.
const USER_DELAY: Duration = Duration::from_secs(3);

/// The address used for throttling: IPv4 as is, IPv6 reduced to its /64 network.
pub(crate) fn network_of(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

#[derive(Clone, Copy)]
struct Entry {
    failures: u32,
    since: Instant,
    blocked_until: Option<Instant>,
    block: Duration,
}

#[derive(Clone, Default)]
pub struct LoginThrottle {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

/// The throttle keys of one login attempt.
pub struct LoginKeys {
    keys: Vec<(String, u32)>,
}

impl LoginKeys {
    pub fn new(username: &str, ip: Option<IpAddr>) -> Self {
        let ip = ip.map(network_of);
        // Usernames are at most 64 characters; longer input cannot exist and is cut so keys
        // stay small no matter what attackers send.
        let user: String = username.trim().to_lowercase().chars().take(64).collect();
        let mut keys = vec![(format!("user:{user}"), USER_LIMIT)];
        if let Some(ip) = ip {
            keys.push((format!("pair:{user}|{ip}"), PAIR_LIMIT));
            keys.push((format!("ip:{ip}"), IP_LIMIT));
        }
        Self { keys }
    }
}

impl LoginThrottle {
    /// Remaining block time if the pair or network key of the attempt is blocked.
    pub fn blocked(&self, attempt: &LoginKeys) -> Option<Duration> {
        let now = Instant::now();
        let entries = self.entries.lock();
        attempt
            .keys
            .iter()
            .filter(|(k, _)| !k.starts_with("user:"))
            .filter_map(|(k, _)| entries.get(k)?.blocked_until)
            .filter(|until| *until > now)
            .map(|until| until - now)
            .max()
    }

    pub fn failed(&self, attempt: &LoginKeys) {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        if entries.len() > MAX_KEYS {
            entries.retain(|_, e| {
                now.duration_since(e.since) < WINDOW || e.blocked_until.is_some_and(|u| u > now)
            });
        }
        if entries.len() > MAX_KEYS {
            // Under a flood (e.g. random usernames) drop entries with a single failure first;
            // counters of targeted accounts have many failures and survive.
            entries.retain(|_, e| e.failures > 1 || e.blocked_until.is_some_and(|u| u > now));
        }
        if entries.len() > MAX_KEYS {
            entries.retain(|_, e| e.blocked_until.is_some_and(|u| u > now) || e.failures > USER_LIMIT / 4);
        }
        for (key, limit) in &attempt.keys {
            let entry = entries.entry(key.clone()).or_insert(Entry {
                failures: 0,
                since: now,
                blocked_until: None,
                block: FIRST_BLOCK,
            });
            if now.duration_since(entry.since) > WINDOW && entry.blocked_until.is_none_or(|u| u <= now) {
                entry.failures = 0;
                entry.since = now;
            }
            entry.failures += 1;
            if entry.failures >= *limit {
                entry.blocked_until = Some(now + entry.block);
                entry.block = (entry.block * 2).min(MAX_BLOCK);
                entry.failures = 0;
                entry.since = now;
            }
        }
    }

    /// Delay to apply when the username itself is under attack (many networks failing).
    pub fn delay(&self, attempt: &LoginKeys) -> Option<Duration> {
        let now = Instant::now();
        let entries = self.entries.lock();
        attempt
            .keys
            .iter()
            .filter(|(k, _)| k.starts_with("user:"))
            .filter_map(|(k, _)| entries.get(k)?.blocked_until)
            .any(|until| until > now)
            .then_some(USER_DELAY)
    }

    /// A successful login clears the pair (other keys age out).
    pub fn succeeded(&self, attempt: &LoginKeys) {
        let mut entries = self.entries.lock();
        for (key, _) in &attempt.keys {
            if key.starts_with("pair:") {
                entries.remove(key);
            }
        }
    }
}

/// The client address: the TCP peer, or — if the peer is a trusted reverse proxy — the
/// right-most address in `X-Forwarded-For` that is not itself a trusted proxy.
pub fn client_ip(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    trusted_proxies: &[IpAddr],
) -> Option<IpAddr> {
    let peer = peer?.ip();
    if !trusted_proxies.contains(&peer) {
        return Some(peer);
    }
    let forwarded = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>();
    forwarded
        .iter()
        .flat_map(|v| v.split(','))
        .rev()
        .filter_map(|part| part.trim().parse::<IpAddr>().ok())
        .find(|ip| !trusted_proxies.contains(ip))
        .or(Some(peer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn pair_blocks_quickly_without_locking_out_others() {
        let throttle = LoginThrottle::default();
        let attacker = LoginKeys::new("admin", ip("6.6.6.6"));
        for _ in 0..PAIR_LIMIT {
            throttle.failed(&attacker);
        }
        assert!(throttle.blocked(&attacker).is_some());
        assert!(
            throttle
                .blocked(&LoginKeys::new("admin", ip("1.1.1.1")))
                .is_none(),
            "owner can still log in"
        );
        assert!(throttle
            .blocked(&LoginKeys::new("other", ip("6.6.6.6")))
            .is_none());
    }

    #[test]
    fn ipv6_is_grouped_by_64_and_users_are_only_slowed() {
        let throttle = LoginThrottle::default();
        for i in 0..PAIR_LIMIT {
            throttle.failed(&LoginKeys::new("admin", ip(&format!("2001:db8:1:2::{i:x}"))));
        }
        assert!(
            throttle
                .blocked(&LoginKeys::new("admin", ip("2001:db8:1:2::ffff")))
                .is_some(),
            "same /64"
        );
        assert!(throttle
            .blocked(&LoginKeys::new("admin", ip("2001:db8:9:9::1")))
            .is_none());

        let throttle = LoginThrottle::default();
        for i in 0..USER_LIMIT {
            throttle.failed(&LoginKeys::new(
                "admin",
                ip(&format!("10.{}.{}.1", i / 200, i % 200)),
            ));
        }
        let owner = LoginKeys::new("admin", ip("1.1.1.1"));
        assert!(
            throttle.blocked(&owner).is_none(),
            "the owner is never hard-blocked"
        );
        assert_eq!(throttle.delay(&owner), Some(USER_DELAY), "but slowed down");
    }

    #[test]
    fn ip_limit_stops_username_spraying() {
        let throttle = LoginThrottle::default();
        for i in 0..IP_LIMIT {
            throttle.failed(&LoginKeys::new(&format!("user{i}"), ip("6.6.6.6")));
        }
        assert!(throttle
            .blocked(&LoginKeys::new("fresh", ip("6.6.6.6")))
            .is_some());
    }

    #[test]
    fn forwarded_for_only_from_trusted_proxies() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "9.9.9.9, 1.2.3.4".parse().unwrap());
        let proxy: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let trusted = ["10.0.0.1".parse().unwrap()];
        assert_eq!(
            client_ip(&headers, Some(proxy), &trusted),
            ip("1.2.3.4"),
            "right-most untrusted"
        );
        assert_eq!(
            client_ip(&headers, Some(proxy), &[]),
            ip("10.0.0.1"),
            "untrusted peer: header ignored"
        );
    }
}

/// Per-user budget for expensive actions (resolving URLs, searching, adding tracks), which
/// start external tools. Chat commands have their own per-client limit in the runtime.
#[derive(Clone, Default)]
pub struct ActionLimiter {
    recent: Arc<Mutex<HashMap<i64, std::collections::VecDeque<Instant>>>>,
    /// Unauthenticated callers, by network (IPv6 /64).
    addresses: Arc<Mutex<HashMap<IpAddr, std::collections::VecDeque<Instant>>>>,
}

/// Tracked keys after which idle entries are dropped (bounds memory under address rotation).
const MAX_TRACKED: usize = 10_000;

fn take_slot<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, std::collections::VecDeque<Instant>>,
    key: K,
) -> Result<(), crate::error::ApiError> {
    let now = Instant::now();
    if map.len() >= MAX_TRACKED {
        map.retain(|_, times| {
            times
                .back()
                .is_some_and(|t| now.duration_since(*t) <= ACTION_WINDOW)
        });
    }
    let times = map.entry(key).or_default();
    while times
        .front()
        .is_some_and(|t| now.duration_since(*t) > ACTION_WINDOW)
    {
        times.pop_front();
    }
    if times.len() >= ACTIONS_PER_WINDOW {
        return Err(crate::error::ApiError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "too many requests, please slow down",
        ));
    }
    times.push_back(now);
    Ok(())
}

const ACTIONS_PER_WINDOW: usize = 20;
const ACTION_WINDOW: Duration = Duration::from_secs(10);

impl ActionLimiter {
    pub fn check(&self, user: i64) -> Result<(), crate::error::ApiError> {
        take_slot(&mut self.recent.lock(), user)
    }

    /// Budget of an unauthenticated caller (IPv6 addresses count per /64).
    pub fn check_address(&self, ip: IpAddr) -> Result<(), crate::error::ApiError> {
        take_slot(&mut self.addresses.lock(), network_of(ip))
    }
}
