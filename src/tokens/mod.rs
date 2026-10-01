//! Token ledger — per-request token events stored alongside history snapshots.
//!
//! One `TokenEvent` per API request / session entry, deduplicated by
//! `event_hash`. Stored in the same SQLite file as `history.rs`
//! (`~/.local/share/usagenometer/history.sqlite3`) under `token_events`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
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
    /// Stable dedup key over the event's identity fields.
    pub fn event_hash(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.provider.hash(&mut hasher);
        self.model.hash(&mut hasher);
        self.session_id.hash(&mut hasher);
        self.project.hash(&mut hasher);
        self.ts_unix.to_bits().hash(&mut hasher);
        self.input_tokens.hash(&mut hasher);
        self.output_tokens.hash(&mut hasher);
        self.cache_read_tokens.hash(&mut hasher);
        self.cache_write_tokens.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn open() -> Result<Self> {
        let path = paths::history_db();
        Self::open_at(path)
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
            "#,
        )?;
        Ok(())
    }

    /// Insert events, skipping hashes already stored. Returns rows inserted.
    pub fn record_events(&self, events: &[TokenEvent]) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().context("begin token_events tx")?;
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
            for event in events {
                inserted += stmt.execute(params![
                    event.event_hash(),
                    event.provider,
                    event.model,
                    event.session_id,
                    event.project,
                    event.ts_unix,
                    event.input_tokens,
                    event.output_tokens,
                    event.cache_read_tokens,
                    event.cache_write_tokens,
                ])?;
            }
        }
        tx.commit().context("commit token_events tx")?;
        Ok(inserted)
    }

    /// Events at or after `since_unix` (all when `None`), optionally filtered
    /// to one provider, oldest first.
    pub fn events_since(
        &self,
        since_unix: Option<f64>,
        provider: Option<&str>,
    ) -> Result<Vec<TokenEvent>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
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

/// Scan local provider logs into the token store. Returns events recorded.
///
/// TODO(tokens): per-provider JSONL scanners land in a sibling PR — this stub
/// keeps the shared contract so the TUI can ship first.
pub fn scan_all(_providers: &[&str]) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(provider: &str, ts: f64, input: u64) -> TokenEvent {
        TokenEvent {
            provider: provider.into(),
            model: Some("model-x".into()),
            session_id: Some("sess-1".into()),
            project: Some("proj".into()),
            ts_unix: ts,
            input_tokens: input,
            output_tokens: 10,
            cache_read_tokens: 1,
            cache_write_tokens: 2,
        }
    }

    #[test]
    fn event_hash_stable() {
        let event = sample("codex", 1000.0, 5);
        assert_eq!(event.event_hash(), event.event_hash());
        let other = sample("codex", 1000.0, 6);
        assert_ne!(event.event_hash(), other.event_hash());
    }

    #[test]
    fn record_and_query_dedups() {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-tokens-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = TokenStore::open_at(path.clone()).unwrap();

        let events = vec![
            sample("codex", 100.0, 5),
            sample("cursor", 200.0, 7),
            sample("codex", 300.0, 11),
        ];
        assert_eq!(store.record_events(&events).unwrap(), 3);
        assert_eq!(store.record_events(&events).unwrap(), 0);

        let all = store.events_since(None, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ts_unix, 100.0);

        let since = store.events_since(Some(150.0), None).unwrap();
        assert_eq!(since.len(), 2);

        let codex = store.events_since(None, Some("codex")).unwrap();
        assert_eq!(codex.len(), 2);
        assert_eq!(codex[1].input_tokens, 11);

        let _ = std::fs::remove_file(&path);
    }
}
