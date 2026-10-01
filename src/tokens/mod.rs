//! Token usage ledger — shared contract for the tokens-core PR.
//!
//! This is a stub: it ships the exact `TokenEvent` / `TokenStore` surface the
//! cost & budget features are built on, with the same SQLite file as
//! `src/history.rs`. The JSONL scanners behind `scan_all` land in the
//! tokens-core PR.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::paths;

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
    /// Deterministic dedup key for the event (FNV-1a 64, hex).
    pub fn event_hash(&self) -> String {
        let mut h: u64 = 0xcbf29ce484222325;
        fn fnv(h: &mut u64, bytes: &[u8]) {
            for b in bytes {
                *h ^= u64::from(*b);
                *h = h.wrapping_mul(0x100000001b3);
            }
        }
        fn field(h: &mut u64, s: Option<&str>) {
            fnv(h, s.unwrap_or("").as_bytes());
            fnv(h, &[0]);
        }
        field(&mut h, Some(&self.provider));
        field(&mut h, self.model.as_deref());
        field(&mut h, self.session_id.as_deref());
        field(&mut h, self.project.as_deref());
        fnv(&mut h, &self.ts_unix.to_bits().to_le_bytes());
        for v in [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
        ] {
            fnv(&mut h, &v.to_le_bytes());
        }
        format!("{h:016x}")
    }
}

pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn open() -> Result<Self> {
        let path = paths::history_db();
        paths::ensure_dir(&path)?;
        let store = Self { path };
        store.init()?;
        Ok(store)
    }

    fn conn(&self) -> Result<Connection> {
        Connection::open(&self.path)
            .with_context(|| format!("open token store {}", self.path.display()))
    }

    fn init(&self) -> Result<()> {
        init_schema(&self.conn()?)
    }

    /// Insert events, deduplicating on `event_hash`. Returns rows inserted.
    pub fn record_events(&self, events: &[TokenEvent]) -> Result<usize> {
        let conn = self.conn()?;
        let mut inserted = 0usize;
        for ev in events {
            let n = conn.execute(
                r#"
                INSERT OR IGNORE INTO token_events
                    (event_hash, provider, model, session_id, project, ts,
                     input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
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
                ],
            )?;
            inserted += n;
        }
        Ok(inserted)
    }

    /// Events since `since_unix` (None = all), optionally for one provider.
    pub fn events_since(
        &self,
        since_unix: Option<f64>,
        provider: Option<&str>,
    ) -> Result<Vec<TokenEvent>> {
        let conn = self.conn()?;
        let (sql, args): (&str, Vec<rusqlite::types::Value>) = match (since_unix, provider) {
            (Some(ts), Some(p)) => (
                "SELECT provider, model, session_id, project, ts, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens FROM token_events \
                 WHERE ts >= ?1 AND provider = ?2 ORDER BY ts ASC",
                vec![ts.into(), p.to_string().into()],
            ),
            (Some(ts), None) => (
                "SELECT provider, model, session_id, project, ts, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens FROM token_events \
                 WHERE ts >= ?1 ORDER BY ts ASC",
                vec![ts.into()],
            ),
            (None, Some(p)) => (
                "SELECT provider, model, session_id, project, ts, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens FROM token_events \
                 WHERE provider = ?1 ORDER BY ts ASC",
                vec![p.to_string().into()],
            ),
            (None, None) => (
                "SELECT provider, model, session_id, project, ts, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens FROM token_events ORDER BY ts ASC",
                vec![],
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
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
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn init_schema(conn: &Connection) -> Result<()> {
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
        CREATE INDEX IF NOT EXISTS idx_token_events_ts ON token_events(ts);
        CREATE INDEX IF NOT EXISTS idx_token_events_provider_ts ON token_events(provider, ts);
        "#,
    )?;
    Ok(())
}

/// Scan all known agent transcript sources and record new token events.
/// TODO(tokens-core): real JSONL scanners land in the tokens-core PR; this
/// stub always reports 0 new events.
pub fn scan_all(_providers: &[&str]) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (TokenStore, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-tokens-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let store = TokenStore { path: path.clone() };
        store.init().unwrap();
        (store, path)
    }

    fn ev(provider: &str, ts: f64) -> TokenEvent {
        TokenEvent {
            provider: provider.into(),
            model: Some("claude-sonnet-4-5".into()),
            session_id: Some("s1".into()),
            project: Some("p".into()),
            ts_unix: ts,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 5,
        }
    }

    #[test]
    fn event_hash_is_deterministic() {
        assert_eq!(
            ev("claude", 1.0).event_hash(),
            ev("claude", 1.0).event_hash()
        );
        assert_ne!(
            ev("claude", 1.0).event_hash(),
            ev("claude", 2.0).event_hash()
        );
    }

    #[test]
    fn record_dedupes_and_reads_back() {
        let (store, path) = test_store();
        let events = vec![ev("claude", 10.0), ev("codex", 20.0)];
        assert_eq!(store.record_events(&events).unwrap(), 2);
        assert_eq!(store.record_events(&events).unwrap(), 0);

        let all = store.events_since(None, None).unwrap();
        assert_eq!(all.len(), 2);
        let since = store.events_since(Some(15.0), None).unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].provider, "codex");
        let prov = store.events_since(None, Some("claude")).unwrap();
        assert_eq!(prov.len(), 1);
        assert_eq!(prov[0].input_tokens, 100);
        let _ = std::fs::remove_file(&path);
    }
}
