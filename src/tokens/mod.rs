//! Local token ledger: append-only per-agent token usage events.
//!
//! Scanners under [`collect`] read the session/log files that AI CLI agents
//! already write to disk (no secrets, read-only) and record token usage into
//! the same SQLite database as snapshot history
//! (`~/.local/share/usagenometer/history.sqlite3`).
//!
//! Events are deduplicated by [`TokenEvent::event_hash`]: re-scanning a file
//! never double-counts. Incremental scanning skips consumed bytes via
//! `token_scan_offsets`. A single unreadable file or malformed line never
//! fails a whole scan.

pub mod collect;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::paths;

/// One recorded token-usage event from an agent's local files.
#[derive(Debug, Clone)]
pub struct TokenEvent {
    pub provider: String,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub ts_unix: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl TokenEvent {
    /// Stable dedup key for `token_events.event_hash` (PRIMARY KEY).
    ///
    /// FNV-1a (64-bit) over a canonical `\x1f`-joined rendering of every field.
    /// Two scanners that produce identical field values deduplicate to the
    /// same row regardless of scan order.
    pub fn event_hash(&self) -> String {
        let canonical = format!(
            "{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
            self.provider,
            self.model.as_deref().unwrap_or(""),
            self.session_id.as_deref().unwrap_or(""),
            self.project.as_deref().unwrap_or(""),
            self.ts_unix.to_bits(),
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
        );
        format!("{:016x}", fnv1a64(canonical.as_bytes()))
    }
}

/// Aggregate token/event totals for one (provider, model) pair.
#[derive(Debug, Clone)]
pub struct TokenTotals {
    pub provider: String,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub events: u64,
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// SQLite-backed event store sharing the history database file.
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn open() -> Result<Self> {
        Self::open_at(paths::history_db())
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        paths::ensure_dir(&path)?;
        let store = Self { path };
        store.init()?;
        Ok(store)
    }

    fn conn(&self) -> Result<Connection> {
        Connection::open(&self.path)
            .with_context(|| format!("open token db {}", self.path.display()))
    }

    fn init(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS token_events (
                event_hash TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                model TEXT,
                session_id TEXT,
                project TEXT,
                ts REAL NOT NULL,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_token_events_ts
                ON token_events(ts);
            CREATE INDEX IF NOT EXISTS idx_token_events_provider_ts
                ON token_events(provider, ts);
            CREATE TABLE IF NOT EXISTS token_scan_offsets (
                path TEXT PRIMARY KEY,
                byte_offset INTEGER NOT NULL DEFAULT 0,
                mtime REAL
            );
            CREATE TABLE IF NOT EXISTS token_scan_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    /// Insert events, ignoring hash duplicates. Returns rows inserted.
    pub fn record_events(&self, events: &[TokenEvent]) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT OR IGNORE INTO token_events
                    (event_hash, provider, model, session_id, project, ts,
                     input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
            )?;
            for ev in events {
                inserted += stmt.execute(params![
                    ev.event_hash(),
                    ev.provider,
                    ev.model,
                    ev.session_id,
                    ev.project,
                    ev.ts_unix,
                    ev.input_tokens,
                    ev.output_tokens,
                    ev.cache_read_tokens,
                    ev.cache_write_tokens,
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Events ordered oldest→newest; optional lower bound and provider filter.
    pub fn events_since(
        &self,
        since_unix: Option<f64>,
        provider: Option<&str>,
    ) -> Result<Vec<TokenEvent>> {
        let conn = self.conn()?;
        let mut out = Vec::new();
        let sql = r#"
            SELECT provider, model, session_id, project, ts,
                   input_tokens, output_tokens, cache_read_tokens, cache_write_tokens
            FROM token_events
            WHERE (?1 IS NULL OR ts >= ?1)
              AND (?2 IS NULL OR provider = ?2)
            ORDER BY ts ASC
            "#;
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![since_unix, provider], |row| {
            Ok(TokenEvent {
                provider: row.get(0)?,
                model: row.get(1)?,
                session_id: row.get(2)?,
                project: row.get(3)?,
                ts_unix: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_write_tokens: row.get(8)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Cumulative totals grouped by (provider, model), for export/reporting.
    pub fn totals(&self) -> Result<Vec<TokenTotals>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT provider, model,
                   SUM(input_tokens), SUM(output_tokens),
                   SUM(cache_read_tokens), SUM(cache_write_tokens),
                   COUNT(*)
            FROM token_events
            GROUP BY provider, model
            ORDER BY provider, model
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TokenTotals {
                provider: row.get(0)?,
                model: row.get(1)?,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
                cache_read_tokens: row.get(4)?,
                cache_write_tokens: row.get(5)?,
                events: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Stored incremental-scan position for a path: (byte_offset, mtime).
    ///
    /// Append-only logs resume at `byte_offset`; whole-file formats are
    /// re-parsed when size or mtime changed since the recorded offset.
    pub fn scan_offset(&self, path: &std::path::Path) -> Result<Option<(u64, f64)>> {
        let conn = self.conn()?;
        let key = path.display().to_string();
        let row = conn
            .query_row(
                "SELECT byte_offset, mtime FROM token_scan_offsets WHERE path = ?1",
                params![key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)),
            )
            .ok();
        Ok(row.map(|(off, mtime)| (off.max(0) as u64, mtime)))
    }

    pub fn set_scan_offset(&self, path: &std::path::Path, byte_offset: u64, mtime: f64) {
        if let Ok(conn) = self.conn() {
            let _ = conn.execute(
                r#"
                INSERT INTO token_scan_offsets (path, byte_offset, mtime)
                VALUES (?1, ?2, ?3)
                ON CONFLICT(path) DO UPDATE SET
                    byte_offset = excluded.byte_offset,
                    mtime = excluded.mtime
                "#,
                params![path.display().to_string(), byte_offset as i64, mtime],
            );
        }
    }

    /// Unix time of the last completed `scan_all`, if any.
    pub fn last_scan_unix(&self) -> Result<Option<f64>> {
        let conn = self.conn()?;
        let v: Option<String> = conn
            .query_row(
                "SELECT value FROM token_scan_meta WHERE key = 'last_scan_unix'",
                [],
                |row| row.get(0),
            )
            .ok();
        Ok(v.and_then(|s| s.parse::<f64>().ok()))
    }

    pub fn record_scan(&self, at_unix: f64) {
        if let Ok(conn) = self.conn() {
            let _ = conn.execute(
                r#"
                INSERT INTO token_scan_meta (key, value) VALUES ('last_scan_unix', ?1)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                "#,
                params![at_unix.to_string()],
            );
        }
    }

    /// Stored events count per provider (for doctor / export).
    pub fn provider_event_counts(&self) -> Result<Vec<(String, u64)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT provider, COUNT(*) FROM token_events GROUP BY provider ORDER BY provider",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (provider, count) = row?;
            out.push((provider, count.max(0) as u64));
        }
        Ok(out)
    }
}

/// Scan the given providers' local files and record new events.
///
/// Returns how many events were newly inserted. Dedup by `event_hash` makes
/// repeat scans cheap and idempotent; individual file/line failures are
/// contained inside each scanner and never abort the run.
pub fn scan_all(providers: &[&str]) -> usize {
    let Ok(store) = TokenStore::open() else {
        return 0;
    };
    let mut total = 0usize;
    for provider in providers {
        let events = collect::scan_provider_files(provider);
        if let Ok(n) = store.record_events(&events) {
            total += n;
        }
    }
    store.record_scan(now_secs());
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (TokenStore, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-tokens-{}-{}.sqlite3",
            std::process::id(),
            tests_nonce()
        ));
        let _ = std::fs::remove_file(&path);
        (TokenStore::open_at(path.clone()).unwrap(), path)
    }

    fn tests_nonce() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::SeqCst)
    }

    fn ev(input: u64) -> TokenEvent {
        TokenEvent {
            provider: "grok".into(),
            model: Some("grok-code-fast".into()),
            session_id: Some("s1".into()),
            project: Some("proj".into()),
            ts_unix: 1_700_000_000.0,
            input_tokens: input,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }

    #[test]
    fn event_hash_is_stable() {
        assert_eq!(ev(10).event_hash(), ev(10).event_hash());
        assert_ne!(ev(10).event_hash(), ev(11).event_hash());
    }

    #[test]
    fn record_dedups_and_filters() {
        let (store, path) = tmp_store();
        assert_eq!(store.record_events(&[ev(10), ev(10)]).unwrap(), 1);
        assert_eq!(store.record_events(&[ev(10)]).unwrap(), 0);
        let all = store.events_since(None, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].input_tokens, 10);
        assert_eq!(store.events_since(Some(2e9), None).unwrap().len(), 0);
        assert_eq!(store.events_since(None, Some("grok")).unwrap().len(), 1);
        assert_eq!(store.events_since(None, Some("cursor")).unwrap().len(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn totals_group_by_provider_and_model() {
        let (store, path) = tmp_store();
        let mut e2 = ev(5);
        e2.model = Some("grok-4".into());
        e2.session_id = Some("s2".into());
        store.record_events(&[ev(10), e2]).unwrap();
        let totals = store.totals().unwrap();
        assert_eq!(totals.len(), 2);
        let first = &totals[0];
        assert_eq!(first.provider, "grok");
        assert!(first.events >= 1);
        let counts = store.provider_event_counts().unwrap();
        assert_eq!(counts, vec![("grok".to_string(), 2)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scan_offsets_roundtrip() {
        let (store, path) = tmp_store();
        let p = std::path::Path::new("/tmp/fake.jsonl");
        assert!(store.scan_offset(p).unwrap().is_none());
        store.set_scan_offset(p, 1234, 9.5);
        assert_eq!(store.scan_offset(p).unwrap(), Some((1234, 9.5)));
        store.record_scan(42.0);
        assert_eq!(store.last_scan_unix().unwrap(), Some(42.0));
        let _ = std::fs::remove_file(&path);
    }
}
