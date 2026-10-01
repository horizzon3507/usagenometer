//! Local token ledger: parses AI-agent session files into the shared sqlite
//! DB and reports per-model/per-project/per-session/per-day totals.
//! Local-only — no network calls, no secrets touched.

pub mod collect;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::paths;

#[derive(Debug, Clone, PartialEq)]
pub struct TokenEvent {
    pub provider: String,        // "claude", "codex", ...
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub project: Option<String>, // project dir or slug
    pub ts_unix: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl TokenEvent {
    /// Stable dedupe key across re-scans (upstream uuid when present,
    /// else hash of provider+session+ts+counts+model).
    pub fn event_hash(&self) -> String {
        // An upstream entry that carries a uuid (e.g. Claude transcript lines)
        // maps 1:1 onto this field tuple, so a content hash over all fields is
        // a stable dedupe key whether or not the source assigned an id.
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = FNV_OFFSET;
        let mut feed = |bytes: &[u8]| {
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(FNV_PRIME);
            }
            h ^= 0x1f; // field separator
            h = h.wrapping_mul(FNV_PRIME);
        };
        feed(self.provider.as_bytes());
        feed(self.session_id.as_deref().unwrap_or("").as_bytes());
        feed(self.project.as_deref().unwrap_or("").as_bytes());
        feed(self.model.as_deref().unwrap_or("").as_bytes());
        feed(&self.ts_unix.to_bits().to_le_bytes());
        feed(&self.input_tokens.to_le_bytes());
        feed(&self.output_tokens.to_le_bytes());
        feed(&self.cache_read_tokens.to_le_bytes());
        feed(&self.cache_write_tokens.to_le_bytes());
        format!("{h:016x}")
    }
}

pub struct TokenStore {
    conn: Connection,
    path: PathBuf,
}

impl TokenStore {
    /// Opens/creates the token_events table in the SAME sqlite file
    /// HistoryStore uses (reuse its path logic).
    pub fn open() -> Result<Self> {
        Self::open_at(paths::history_db())
    }

    pub(crate) fn open_at(path: PathBuf) -> Result<Self> {
        paths::ensure_dir(&path)?;
        let conn = Connection::open(&path)
            .with_context(|| format!("open token ledger {}", path.display()))?;
        let store = Self { conn, path };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
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
            CREATE INDEX IF NOT EXISTS idx_token_events_ts ON token_events(ts);
            CREATE INDEX IF NOT EXISTS idx_token_events_provider_ts ON token_events(provider, ts);
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

    /// INSERT OR IGNORE by event_hash; returns number newly inserted.
    pub fn record_events(&self, events: &[TokenEvent]) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
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
            for e in events {
                inserted += stmt.execute(params![
                    e.event_hash(),
                    e.provider,
                    e.model,
                    e.session_id,
                    e.project,
                    e.ts_unix,
                    e.input_tokens as i64,
                    e.output_tokens as i64,
                    e.cache_read_tokens as i64,
                    e.cache_write_tokens as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn events_since(
        &self,
        since_unix: Option<f64>,
        provider: Option<&str>,
    ) -> Result<Vec<TokenEvent>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT provider, model, session_id, project, ts,
                   input_tokens, output_tokens, cache_read_tokens, cache_write_tokens
            FROM token_events
            WHERE (?1 IS NULL OR ts >= ?1)
              AND (?2 IS NULL OR provider = ?2)
            ORDER BY ts ASC
            "#,
        )?;
        let rows = stmt.query_map(params![since_unix, provider], |row| {
            Ok(TokenEvent {
                provider: row.get(0)?,
                model: row.get(1)?,
                session_id: row.get(2)?,
                project: row.get(3)?,
                ts_unix: row.get(4)?,
                input_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                output_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                cache_read_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                cache_write_tokens: row.get::<_, i64>(8)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Total recorded events (doctor / status surfaces).
    pub fn event_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM token_events", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// DB file path (same file as HistoryStore).
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub(crate) fn scan_offsets(&self) -> Result<HashMap<String, u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, byte_offset FROM token_scan_offsets")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?.max(0) as u64))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    pub(crate) fn save_scan_offset(&self, path: &str, offset: u64) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO token_scan_offsets (path, byte_offset) VALUES (?1, ?2)
            ON CONFLICT(path) DO UPDATE SET byte_offset = excluded.byte_offset
            "#,
            params![path, offset as i64],
        )?;
        Ok(())
    }

    /// Stored incremental-scan position for a path: (byte_offset, mtime).
    ///
    /// Append-only logs resume at `byte_offset`; whole-file formats are
    /// re-parsed when size or mtime changed since the recorded offset.
    pub fn scan_offset(&self, path: &std::path::Path) -> Result<Option<(u64, f64)>> {
        let key = path.display().to_string();
        let row = self
            .conn
            .query_row(
                "SELECT byte_offset, mtime FROM token_scan_offsets WHERE path = ?1",
                params![key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)),
            )
            .ok();
        Ok(row.map(|(off, mtime)| (off.max(0) as u64, mtime)))
    }

    pub fn set_scan_offset(&self, path: &std::path::Path, byte_offset: u64, mtime: f64) {
        let _ = self.conn.execute(
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

    /// Unix time of the last completed `scan_all`, if any.
    pub fn last_scan_unix(&self) -> Result<Option<f64>> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM token_scan_meta WHERE key = 'last_scan_unix'",
                [],
                |row| row.get(0),
            )
            .ok();
        Ok(v.and_then(|s| s.parse::<f64>().ok()))
    }

    pub fn record_scan(&self, at_unix: f64) {
        let _ = self.conn.execute(
            r#"
            INSERT INTO token_scan_meta (key, value) VALUES ('last_scan_unix', ?1)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![at_unix.to_string()],
        );
    }

    /// Stored events count per provider (for doctor / export).
    pub fn provider_event_counts(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
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

    /// Cumulative totals grouped by (provider, model), for export/reporting.
    pub fn totals(&self) -> Result<Vec<TokenTotals>> {
        let mut stmt = self.conn.prepare(
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

/// Scan every provider's local files, record new events, return count.
pub fn scan_all(providers: &[&str]) -> usize {
    let Ok(store) = TokenStore::open() else {
        return 0;
    };
    scan_with_store(&store, providers)
}

fn scan_with_store(store: &TokenStore, providers: &[&str]) -> usize {
    let mut offsets = collect::ScanOffsets::load(store);
    let mut events = Vec::new();
    for provider in providers {
        let mut batch = match *provider {
            "claude" => collect::claude::collect(&mut offsets),
            "codex" => collect::codex::collect(&mut offsets),
            "grok" => collect::grok::scan(),
            "gemini" => collect::gemini::scan(),
            "cursor" => collect::cursor::scan(),
            "antigravity" => collect::antigravity::scan(),
            "opencode" => collect::opencode::scan(),
            "glm" => collect::glm::scan(),
            "omp" => collect::omp::scan(),
            "droid" => collect::droid::scan(),
            "pi" => collect::pi::scan(),
            "kimi" => collect::kimi::scan(),
            _ => Vec::new(),
        };
        events.append(&mut batch);
    }
    offsets.flush(store);
    let n = store.record_events(&events).unwrap_or(0);
    store.record_scan(now_unix());
    n
}

// ---------- reporting helpers (used by `usg tokens`; additive, not part of
// the ledger contract)

/// Grouping dimension for `usg tokens --by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Model,
    Project,
    Session,
    Day,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Totals {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, e: &TokenEvent) {
        self.input += e.input_tokens;
        self.output += e.output_tokens;
        self.cache_read += e.cache_read_tokens;
        self.cache_write += e.cache_write_tokens;
    }
}

#[derive(Debug)]
pub struct GroupRow {
    pub provider: String,
    pub keys: Vec<String>,
    pub totals: Totals,
}

/// Group events by provider, then by each requested dimension.
pub fn aggregate(events: &[TokenEvent], groups: &[Group]) -> Vec<GroupRow> {
    let local = local_offset();
    let mut map: BTreeMap<(String, Vec<String>), Totals> = BTreeMap::new();
    for e in events {
        let keys: Vec<String> = groups.iter().map(|g| group_key(e, *g, local)).collect();
        map.entry((e.provider.clone(), keys))
            .or_default()
            .add(e);
    }
    map.into_iter()
        .map(|((provider, keys), totals)| GroupRow {
            provider,
            keys,
            totals,
        })
        .collect()
}

fn group_key(e: &TokenEvent, g: Group, local: time::UtcOffset) -> String {
    match g {
        Group::Model => e.model.clone().unwrap_or_else(|| "?".into()),
        Group::Project => e.project.clone().unwrap_or_else(|| "?".into()),
        Group::Session => e.session_id.clone().unwrap_or_else(|| "?".into()),
        Group::Day => day_key(e.ts_unix, local),
    }
}

pub fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn local_offset() -> time::UtcOffset {
    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC)
}

/// Unix seconds for the start of the local day containing `unix`.
pub fn day_start_unix(unix: f64, local: time::UtcOffset) -> f64 {
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(unix as i64) else {
        return unix;
    };
    let local_date = dt.to_offset(local).date();
    local_date.midnight().assume_offset(local).unix_timestamp() as f64
}

/// `YYYY-MM-DD` label for an event timestamp, in local time.
pub fn day_key(unix: f64, local: time::UtcOffset) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix as i64)
        .map(|dt| dt.to_offset(local).date().to_string())
        .unwrap_or_else(|_| "?".into())
}

/// Parse `--since` values: RFC3339 (e.g. 2026-10-01T00:00:00Z) or YYYY-MM-DD
/// (interpreted as local-day midnight, falling back to UTC).
pub fn parse_since(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) =
        time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc3339)
    {
        return Some(dt.unix_timestamp() as f64 + f64::from(dt.nanosecond()) / 1e9);
    }
    if let Ok(date) = time::Date::parse(
        trimmed,
        &time::format_description::well_known::Iso8601::DATE,
    ) {
        let local = local_offset();
        return Some(date.midnight().assume_offset(local).unix_timestamp() as f64);
    }
    None
}

/// Thousands separators for big token counts (1234567 → "1,234,567").
pub fn fmt_num(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(provider: &str, ts: f64, input: u64, output: u64) -> TokenEvent {
        TokenEvent {
            provider: provider.into(),
            model: Some("m1".into()),
            session_id: Some("s1".into()),
            project: Some("p1".into()),
            ts_unix: ts,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }

    #[test]
    fn event_hash_stable_and_distinct() {
        let a = event("claude", 1.5, 10, 20);
        let b = event("claude", 1.5, 10, 20);
        let c = event("claude", 1.5, 11, 20);
        let d = event("codex", 1.5, 10, 20);
        assert_eq!(a.event_hash(), b.event_hash());
        assert_ne!(a.event_hash(), c.event_hash());
        assert_ne!(a.event_hash(), d.event_hash());
        assert_eq!(a.event_hash().len(), 16);
    }

    #[test]
    fn store_roundtrip_and_dedupe() {
        let path =
            std::env::temp_dir().join(format!("usagenometer-tokens-{}.sqlite3", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = TokenStore::open_at(path.clone()).unwrap();
        let events = vec![
            event("claude", 100.0, 10, 5),
            event("claude", 200.0, 20, 5),
            event("codex", 300.0, 30, 5),
        ];
        assert_eq!(store.record_events(&events).unwrap(), 3);
        // Re-recording the same events dedupes by event_hash.
        assert_eq!(store.record_events(&events).unwrap(), 0);

        let all = store.events_since(None, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ts_unix, 100.0);
        let claude = store.events_since(None, Some("claude")).unwrap();
        assert_eq!(claude.len(), 2);
        let since = store.events_since(Some(150.0), None).unwrap();
        assert_eq!(since.len(), 2);
        assert_eq!(store.event_count().unwrap(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scan_offsets_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-offsets-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = TokenStore::open_at(path.clone()).unwrap();
        store.save_scan_offset("/tmp/a.jsonl", 42).unwrap();
        store.save_scan_offset("/tmp/a.jsonl", 100).unwrap();
        let map = store.scan_offsets().unwrap();
        assert_eq!(map.get("/tmp/a.jsonl"), Some(&100));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn aggregate_groups_by_dimensions() {
        let mut e1 = event("claude", 1_700_000_000.0, 10, 5);
        e1.cache_read_tokens = 7;
        let mut e2 = event("claude", 1_700_000_100.0, 20, 5);
        e2.model = Some("m2".into());
        let mut e3 = event("codex", 1_700_000_200.0, 30, 5);
        e3.project = None;
        let rows = aggregate(&[e1, e2, e3], &[Group::Model]);
        // key order: (claude,m1), (claude,m2), (codex,m2)
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].provider, "claude");
        assert_eq!(rows[0].keys, vec!["m1"]);
        assert_eq!(rows[0].totals.input, 10);
        assert_eq!(rows[0].totals.total(), 22);
        assert_eq!(rows[2].provider, "codex");
        // grouping by nothing yields per-provider rows
        let flat = aggregate(&[event("claude", 1.0, 1, 1), event("claude", 2.0, 2, 2)], &[]);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].totals.input, 3);
    }

    #[test]
    fn parses_since_formats() {
        let ts = parse_since("2026-10-01T00:00:00Z").unwrap();
        assert_eq!(ts, 1_790_812_800.0);
        assert!(parse_since("2026-10-01").is_some());
        assert!(parse_since("garbage").is_none());
    }

    #[test]
    fn day_boundaries() {
        let utc = time::UtcOffset::UTC;
        // 2026-10-01T12:00:00Z → day starts at 2026-10-01T00:00:00Z
        assert_eq!(day_start_unix(1_790_812_800.0 + 43200.0, utc), 1_790_812_800.0);
        assert_eq!(day_key(1_790_812_800.0 + 43200.0, utc), "2026-10-01");
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(999), "999");
        assert_eq!(fmt_num(1_234_567), "1,234,567");
    }
}
