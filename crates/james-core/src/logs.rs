//! Recent log messages per instance (James' own and those of plugins), for the web interface.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;

/// Messages kept per instance.
pub const MAX_ENTRIES: usize = 2000;
/// Bytes of messages kept per instance (older ones are dropped first).
pub const MAX_BYTES: usize = 512 * 1024;
/// Longest message kept, in bytes (longer ones are cut).
pub const MAX_MESSAGE: usize = 4000;
/// Instances with a log; messages for further ones are dropped.
const MAX_INSTANCES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// Increases with every message (over all instances, starting at 1 when James starts).
    pub seq: u64,
    /// Unix milliseconds.
    pub time: i64,
    pub level: LogLevel,
    /// The plugin that wrote it; `None` for James itself.
    pub plugin: Option<String>,
    pub message: String,
}

#[derive(Default)]
struct Log {
    entries: VecDeque<LogEntry>,
    bytes: usize,
}

pub struct InstanceLogs {
    /// Random per process: tells clients that sequence numbers started over.
    boot: u64,
    seq: std::sync::atomic::AtomicU64,
    logs: RwLock<HashMap<String, Arc<Mutex<Log>>>>,
    /// Deleted instances: their tasks may still log while they wind down.
    forgotten: Mutex<HashSet<String>>,
}

impl Default for InstanceLogs {
    fn default() -> Self {
        let boot = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64 ^ u64::from(std::process::id()));
        Self {
            boot,
            seq: Default::default(),
            logs: Default::default(),
            forgotten: Default::default(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Characters that are invisible, change the text direction or show as empty space.
fn hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// Combining marks (stacked, they draw over neighbouring lines).
fn combining(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{0483}'..='\u{0489}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

/// One line without control, line-breaking or hidden characters and at most two combining marks
/// in a row: log lines cannot be forged or disguised (e.g. a nickname with a line break).
pub fn clean(text: &str) -> String {
    let mut marks = 0;
    text.trim_end_matches(['\n', '\r'])
        .chars()
        .filter(|c| !hidden(*c))
        .filter(|c| {
            marks = if combining(*c) { marks + 1 } else { 0 };
            marks <= 2
        })
        .map(|c| match c {
            '\t' => c,
            '\u{2028}' | '\u{2029}' | '\u{0085}' => '⏎',
            c if c.is_control() => '⏎',
            c => c,
        })
        .collect()
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

impl Log {
    /// Makes room: the oldest info or debug message goes first, then the oldest warning, errors
    /// last, so a flood (many commands, failing tracks, a chatty plugin) does not push out
    /// errors.
    fn evict(&mut self) {
        let oldest = |max: LogLevel| self.entries.iter().position(|e| e.level <= max);
        let index = oldest(LogLevel::Info)
            .or_else(|| oldest(LogLevel::Warn))
            .unwrap_or(0);
        if let Some(old) = self.entries.remove(index) {
            self.bytes -= old.message.len();
        }
    }
}

impl InstanceLogs {
    fn log(&self, instance: &str) -> Option<Arc<Mutex<Log>>> {
        if let Some(log) = self.logs.read().unwrap_or_else(|e| e.into_inner()).get(instance) {
            return Some(log.clone());
        }
        let mut logs = self.logs.write().unwrap_or_else(|e| e.into_inner());
        // Checked under the write lock, which `forget` holds too.
        if lock(&self.forgotten).contains(instance) {
            return None;
        }
        if logs.len() >= MAX_INSTANCES && !logs.contains_key(instance) {
            return None;
        }
        Some(logs.entry(instance.to_owned()).or_default().clone())
    }

    pub fn push(&self, instance: &str, level: LogLevel, plugin: Option<&str>, message: &str) {
        let Some(log) = self.log(instance) else {
            return;
        };
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let message = cut(&clean(message), MAX_MESSAGE);
        let mut log = lock(&log);
        // Taken under the instance's lock, so its entries stay in order.
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let size = message.len();
        while !log.entries.is_empty() && (log.entries.len() >= MAX_ENTRIES || log.bytes + size > MAX_BYTES) {
            log.evict();
        }
        log.bytes += size;
        log.entries.push_back(LogEntry {
            seq,
            time,
            level,
            plugin: plugin.map(str::to_owned),
            message,
        });
    }

    /// Changes when James restarts, see [`LogEntry::seq`].
    pub fn boot(&self) -> u64 {
        self.boot
    }

    /// Messages after `after` (oldest first), at most `limit` (the newest ones).
    pub fn since(&self, instance: &str, after: u64, limit: usize) -> Vec<LogEntry> {
        let Some(log) = self
            .logs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(instance)
            .cloned()
        else {
            return Vec::new();
        };
        let log = lock(&log);
        let newer = log.entries.len() - log.entries.partition_point(|e| e.seq <= after);
        log.entries
            .iter()
            .skip(log.entries.len() - newer.min(limit))
            .cloned()
            .collect()
    }

    /// Removes the log of a deleted instance; messages for it are ignored until [`Self::revive`].
    pub fn forget(&self, instance: &str) {
        let mut logs = self.logs.write().unwrap_or_else(|e| e.into_inner());
        lock(&self.forgotten).insert(instance.to_owned());
        logs.remove(instance);
    }

    /// An instance with this id was created (again), with an empty log.
    pub fn revive(&self, instance: &str) {
        let mut logs = self.logs.write().unwrap_or_else(|e| e.into_inner());
        lock(&self.forgotten).remove(instance);
        logs.remove(instance);
    }
}

/// Keys a [`RateLimiter`] tracks at most.
const MAX_KEYS: usize = 10_000;

/// At most `limit` events per key and window (e.g. log messages of one client or plugin).
pub struct RateLimiter {
    limit: u32,
    window: std::time::Duration,
    keys: Mutex<HashMap<String, (std::time::Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(limit: u32, window: std::time::Duration) -> Self {
        Self {
            limit,
            window,
            keys: Mutex::default(),
        }
    }

    pub fn allow(&self, key: &str) -> bool {
        let now = std::time::Instant::now();
        let mut keys = lock(&self.keys);
        if keys.len() >= MAX_KEYS && !keys.contains_key(key) {
            keys.retain(|_, (start, _)| now.duration_since(*start) < self.window);
            // Still full (many new keys within one window, e.g. fresh identities): new ones wait.
            if keys.len() >= MAX_KEYS {
                return false;
            }
        }
        let (start, count) = keys.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(*start) >= self.window {
            *start = now;
            *count = 0;
        }
        *count += 1;
        *count <= self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_recent_messages_per_instance() {
        let logs = InstanceLogs::default();
        for i in 0..MAX_ENTRIES + 5 {
            logs.push("a", LogLevel::Info, None, &format!("m{i}"));
        }
        logs.push("b", LogLevel::Warn, Some("greeter"), &"ä".repeat(MAX_MESSAGE));
        let all = logs.since("a", 0, usize::MAX);
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0].message, "m5");
        let last = all.last().unwrap().seq;
        assert!(logs.since("a", last, 10).is_empty());
        let tail = logs.since("a", 0, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1].seq, last);
        assert_eq!(logs.since("a", last - 3, 100).len(), 3);
        let b = logs.since("b", 0, 10);
        assert_eq!(b[0].plugin.as_deref(), Some("greeter"));
        assert!(b[0].message.len() <= MAX_MESSAGE + '…'.len_utf8());

        logs.forget("a");
        logs.push("a", LogLevel::Info, None, "late");
        assert!(
            logs.since("a", 0, 10).is_empty(),
            "a deleted instance's tasks winding down"
        );
        logs.revive("a");
        logs.push("a", LogLevel::Info, None, "new");
        assert_eq!(logs.since("a", 0, 10).len(), 1);
    }

    #[test]
    fn lines_cannot_be_forged() {
        let logs = InstanceLogs::default();
        logs.push(
            "a",
            LogLevel::Info,
            None,
            "!help x\n2026-10-29 WARN identity exported\u{202E}\x1b[31m\u{2028}y\u{2060}\u{E0041}\n",
        );
        assert_eq!(
            logs.since("a", 0, 1)[0].message,
            "!help x⏎2026-10-29 WARN identity exported⏎[31m⏎y"
        );
        logs.push("a", LogLevel::Info, None, &"\x01".repeat(MAX_MESSAGE));
        assert!(
            logs.since("a", 0, 2)[1].message.len() <= MAX_MESSAGE + '…'.len_utf8(),
            "cut after cleaning"
        );
    }

    /// Floods (a chatty plugin, a client spamming commands) are limited by their sources.
    #[test]
    fn rate_limiter() {
        let limiter = RateLimiter::new(3, std::time::Duration::from_secs(60));
        assert!((0..3).all(|_| limiter.allow("a")));
        assert!(!limiter.allow("a"));
        assert!(limiter.allow("b"));
        let short = RateLimiter::new(1, std::time::Duration::ZERO);
        assert!(short.allow("a") && short.allow("a"), "a new window starts");
    }

    #[test]
    fn floods_keep_errors() {
        let logs = InstanceLogs::default();
        logs.push("a", LogLevel::Error, None, "connection failed");
        for i in 0..MAX_ENTRIES * 2 {
            logs.push("a", LogLevel::Info, None, &format!("!help {i}"));
            logs.push("a", LogLevel::Warn, None, &format!("track failed {i}"));
        }
        let kept = logs.since("a", 0, usize::MAX);
        assert_eq!(kept.len(), MAX_ENTRIES);
        assert_eq!(kept[0].message, "connection failed");
        assert_eq!(
            clean("a\u{301}\u{301}\u{301}\u{301}b\u{1BCA0}"),
            "a\u{301}\u{301}b"
        );
    }

    #[test]
    fn bytes_are_limited() {
        let logs = InstanceLogs::default();
        let big = "x".repeat(MAX_MESSAGE);
        for _ in 0..MAX_ENTRIES {
            logs.push("a", LogLevel::Debug, Some("chatty"), &big);
        }
        let kept = logs.since("a", 0, usize::MAX);
        assert!(kept.iter().map(|e| e.message.len()).sum::<usize>() <= MAX_BYTES);
        assert_eq!(kept.len(), MAX_BYTES / MAX_MESSAGE);
    }
}
