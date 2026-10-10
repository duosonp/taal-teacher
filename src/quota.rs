//! Daily token budgets (SQLite) and a per-address request rate limit (in memory).
//!
//! Budgets are counted in real tokens as reported by the Anthropic API, per UTC day, under
//! three keys: the visitor's cookie id, a salted hash of their IP address, and "global".

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

const DAY: u64 = 86_400;

/// Current UTC day number and seconds until it ends.
pub fn today() -> (i64, u64) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    ((now / DAY) as i64, DAY - now % DAY)
}

pub struct Store {
    conn: Mutex<Connection>,
    salt: String,
}

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS usage (
               day INTEGER NOT NULL, key TEXT NOT NULL, tokens INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (day, key));
             CREATE TABLE IF NOT EXISTS meta (name TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS answers (key TEXT PRIMARY KEY, value TEXT NOT NULL, created INTEGER NOT NULL);",
        )?;
        // A random salt kept with the data, so IP hashes stay stable across restarts
        // but can't be reversed from the database alone.
        let salt: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE name = 'salt'", [], |r| r.get(0)).optional()?;
        let salt = match salt {
            Some(s) => s,
            None => {
                let s = format!("{:032x}", rand::random::<u128>());
                conn.execute("INSERT INTO meta (name, value) VALUES ('salt', ?1)", [&s])?;
                s
            }
        };
        let (day, _) = today();
        conn.execute("DELETE FROM usage WHERE day < ?1", [day - 7])?;
        Ok(Self { conn: Mutex::new(conn), salt })
    }

    pub fn ip_key(&self, ip: &str) -> String {
        let digest = Sha256::digest(format!("{}:{ip}", self.salt));
        let hex: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
        format!("ip:{hex}")
    }

    pub fn used(&self, key: &str) -> u64 {
        let (day, _) = today();
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT tokens FROM usage WHERE day = ?1 AND key = ?2", params![day, key], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0) as u64
    }

    pub fn add(&self, keys: &[&str], tokens: u64) {
        if tokens == 0 {
            return;
        }
        let (day, _) = today();
        let mut conn = self.conn.lock().unwrap();
        let result = (|| {
            let tx = conn.transaction()?;
            for key in keys {
                tx.execute(
                    "INSERT INTO usage (day, key, tokens) VALUES (?1, ?2, ?3)
                     ON CONFLICT (day, key) DO UPDATE SET tokens = tokens + excluded.tokens",
                    params![day, key, tokens as i64],
                )?;
            }
            tx.commit()
        })();
        if let Err(e) = result {
            tracing::error!("could not record usage: {e}");
        }
    }
}

impl Store {
    /// Cached answer for a shared prompt (lessons, topic suggestions), keyed by the prompt's hash.
    pub fn answer_key(kind: &str, prompt: &str) -> String {
        let digest = Sha256::digest(format!("{kind}\n{prompt}"));
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn cached_answer(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM answers WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn store_answer(&self, key: &str, value: &str) {
        let (day, _) = today();
        let conn = self.conn.lock().unwrap();
        let result = conn
            .execute(
                "INSERT INTO answers (key, value, created) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, created = excluded.created",
                params![key, value, day],
            )
            // Keep the table bounded: drop the oldest beyond 5000 answers.
            .and_then(|_| {
                conn.execute(
                    "DELETE FROM answers WHERE key IN (SELECT key FROM answers ORDER BY created DESC LIMIT -1 OFFSET 5000)",
                    [],
                )
            });
        if let Err(e) = result {
            tracing::error!("could not cache answer: {e}");
        }
    }
}

/// Sliding-window limit on requests per address.
pub struct RateLimit {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimit {
    pub fn new(max: usize, window: Duration) -> Self {
        Self { max, window, hits: Mutex::new(HashMap::new()) }
    }

    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap();
        if hits.len() > 10_000 {
            hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < self.window));
        }
        let q = hits.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
            q.pop_front();
        }
        if q.len() >= self.max {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_add_up_per_key() {
        let s = Store::open(":memory:").unwrap();
        s.add(&["c:a", "global"], 100);
        s.add(&["c:a", "global"], 50);
        s.add(&["c:b", "global"], 7);
        assert_eq!(s.used("c:a"), 150);
        assert_eq!(s.used("c:b"), 7);
        assert_eq!(s.used("global"), 157);
        assert_eq!(s.used("c:nobody"), 0);
    }

    #[test]
    fn ip_keys_are_hashed_and_stable() {
        let s = Store::open(":memory:").unwrap();
        let k = s.ip_key("203.0.113.9");
        assert!(k.starts_with("ip:") && !k.contains("203"));
        assert_eq!(k, s.ip_key("203.0.113.9"));
        assert_ne!(k, s.ip_key("203.0.113.10"));
    }

    #[test]
    fn caches_shared_answers() {
        let s = Store::open(":memory:").unwrap();
        let k = Store::answer_key("ui", "translate into German");
        assert_eq!(s.cached_answer(&k), None);
        s.store_answer(&k, r#"{"a":"b"}"#);
        assert_eq!(s.cached_answer(&k).as_deref(), Some(r#"{"a":"b"}"#));
        assert_ne!(k, Store::answer_key("ui", "translate into French"));
    }

    #[test]
    fn rate_limit_blocks_after_max() {
        let r = RateLimit::new(2, Duration::from_secs(60));
        assert!(r.allow("x") && r.allow("x"));
        assert!(!r.allow("x"));
        assert!(r.allow("y"));
    }
}
