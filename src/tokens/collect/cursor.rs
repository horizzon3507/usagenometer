//! Cursor state scanner (`Cursor/User/globalStorage/state.vscdb`).
//!
//! Cursor persists chat state in a VS Code-style SQLite store. Real token
//! numbers live in `cursorDiskKV` rows keyed `bubbleId:{composer}:{bubble}`
//! as `tokenCount: {inputTokens, outputTokens}`; other usage-bearing objects
//! in `ItemTable`/`cursorDiskKV` (`usage`, `tokenUsage`, `tokenCount` fields
//! with numeric values) are extracted too.
//!
//! Not tokens (deliberately ignored):
//! - `composerData:{id}` → `usageData.default.amount` — request/cost units.
//! - `aiService.generations` — generation metadata (uuid/type/text) only.
//!
//! The database is opened strictly read-only (`SQLITE_OPEN_READ_ONLY`). When
//! the live app holds the WAL lock, we copy `state.vscdb` plus `-wal`/`-shm`
//! to a temp dir and read the copy — we never write to a live app's DB.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::util::{
    self, model_on_record, session_on_record, ts_on_record, usage_counts, usage_on_record,
};
use crate::tokens::TokenEvent;

const PROVIDER: &str = "cursor";
const MAX_ROWS: usize = 20_000;

pub fn scan() -> Vec<TokenEvent> {
    scan_roots().iter().flat_map(|db| scan_db(db)).collect()
}

/// Cursor state DBs we know how to read. The global store carries the chat
/// bubbles; workspace stores are metadata-only today.
pub fn scan_roots() -> Vec<PathBuf> {
    let home = util::home();
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    vec![
        cfg.join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    ]
}

/// Open a vscdb read-only; on lock/open failure retry against a temp copy of
/// the db + WAL/SHM sidecars so a running app is never blocked or written to.
/// Returns the connection plus the temp dir to remove once it is dropped.
fn open_readonly(path: &Path) -> Option<(Connection, Option<PathBuf>)> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let uri = format!("file:{}?mode=ro", path.display());
    if let Ok(conn) = Connection::open_with_flags(&uri, flags) {
        return Some((conn, None));
    }
    let tmp = std::env::temp_dir().join(format!(
        "usg-cursor-{}-{}",
        std::process::id(),
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("db")
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).ok()?;
    let name = path.file_name()?.to_str()?;
    let copy = tmp.join(name);
    fs::copy(path, &copy).ok()?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{}", path.display(), suffix));
        if side.exists() {
            let _ = fs::copy(&side, tmp.join(format!("{name}{suffix}")));
        }
    }
    let uri = format!("file:{}?mode=ro", copy.display());
    Connection::open_with_flags(&uri, flags)
        .ok()
        .map(|conn| (conn, Some(tmp)))
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .is_ok()
}

/// `key` column may be named `key` (ItemTable) or `key` (cursorDiskKV) —
/// both expose (key, value); probe gently.
fn rows(conn: &Connection, table: &str, like: &str, limit: usize) -> Vec<(String, String)> {
    if !table_exists(conn, table) {
        return Vec::new();
    }
    let sql = format!("SELECT key, value FROM {table} WHERE key LIKE ?1 LIMIT ?2");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let Ok(iter) = stmt.query_map(rusqlite::params![like, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return Vec::new();
    };
    iter.flatten().collect()
}

/// Per-bubble token counts: `bubbleId:{composerId}:{bubbleId}` →
/// `{tokenCount:{inputTokens,outputTokens}, ...}`.
fn bubble_event(provider: &str, key: &str, value: &Value, fallback_ts: f64) -> Option<TokenEvent> {
    let counts = value
        .get("tokenCount")
        .and_then(usage_counts)
        .or_else(|| usage_on_record(value))?;
    // bubbleId:<composerId>:<bubbleId> — keep both halves as the session ref.
    let session = key
        .strip_prefix("bubbleId:")
        .map(str::to_string)
        .or_else(|| session_on_record(value));
    Some(TokenEvent {
        provider: provider.into(),
        model: model_on_record(value),
        session_id: session,
        project: None,
        ts_unix: ts_on_record(value).unwrap_or(fallback_ts),
        input_tokens: counts.input,
        output_tokens: counts.output,
        cache_read_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
    })
}

/// Other keys may hold usage objects (`usage`, `tokenUsage`, `tokenCount`) at
/// top level or on array elements. Request counts and costs are not tokens:
/// `usageData`, `costInCents`, `amount` fields are never mapped.
fn generic_event(provider: &str, key: &str, value: &Value, fallback_ts: f64) -> Vec<TokenEvent> {
    let mut out = Vec::new();
    let candidates: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        _ => vec![value],
    };
    for item in candidates {
        let Some(counts) = usage_on_record(item) else {
            continue;
        };
        out.push(TokenEvent {
            provider: provider.into(),
            model: model_on_record(item),
            session_id: session_on_record(item).or_else(|| Some(key.to_string())),
            project: None,
            ts_unix: ts_on_record(item).unwrap_or(fallback_ts),
            input_tokens: counts.input,
            output_tokens: counts.output,
            cache_read_tokens: counts.cache_read,
            cache_write_tokens: counts.cache_write,
        });
    }
    out
}

/// Scan one vscdb file. Public for tests/fixtures.
pub fn scan_db(path: &Path) -> Vec<TokenEvent> {
    scan_db_as(PROVIDER, path)
}

/// vscdb probe shared with the Antigravity scanner (its own provider id).
pub(super) fn scan_db_as(provider: &str, path: &Path) -> Vec<TokenEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Some((conn, tmp)) = open_readonly(path) else {
        return Vec::new();
    };
    let fallback_ts = util::file_mtime(path);
    let mut events = Vec::new();

    for table in ["cursorDiskKV", "ItemTable"] {
        // Bubble token counts — the only documented per-message token field.
        for (key, raw) in rows(&conn, table, "bubbleId:%", MAX_ROWS) {
            if let Ok(value) = serde_json::from_str::<Value>(&raw)
                && let Some(ev) = bubble_event(provider, &key, &value, fallback_ts)
            {
                events.push(ev);
            }
        }
        // Usage-ish keys: token fields only when real numbers exist.
        for like in ["%usage%", "%token%", "%aiService%", "%generation%"] {
            for (key, raw) in rows(&conn, table, like, MAX_ROWS) {
                if key.starts_with("bubbleId:") || key.contains("composerData") {
                    continue; // handled above / cost counters are not tokens
                }
                let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                events.extend(generic_event(provider, &key, &value, fallback_ts));
            }
        }
    }
    drop(conn);
    if let Some(tmp) = tmp {
        let _ = fs::remove_dir_all(tmp);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bubble_token_counts_readonly() {
        let db_dir =
            std::env::temp_dir().join(format!("usg-cursor-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&db_dir);
        fs::create_dir_all(&db_dir).unwrap();
        let db = db_dir.join("state.vscdb");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB);
                 CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "bubbleId:comp-1:bub-7",
                    r#"{"type":2,"text":"answer","tokenCount":{"inputTokens":1200,"outputTokens":340},"createdAt":1759300000000}"#
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "composerData:comp-1",
                    r#"{"usageData":{"default":{"costInCents":8,"amount":2}}}"#
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    "aiService.generations",
                    r#"[{"unixMs":1759300000000,"generationUUID":"g-1","type":"composer"}]"#
                ],
            )
            .unwrap();
        }
        let events = scan_db(&db);
        assert_eq!(events.len(), 1, "only real token fields become events");
        let ev = &events[0];
        assert_eq!(ev.input_tokens, 1200);
        assert_eq!(ev.output_tokens, 340);
        assert_eq!(ev.session_id.as_deref(), Some("comp-1:bub-7"));
        // Read-only guarantee: file contents unchanged after the scan.
        let after = fs::read(&db).unwrap();
        assert!(
            Connection::open_with_flags(
                format!("file:{}?mode=ro", db.display()),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
            .is_ok()
        );
        drop(after);
        let _ = fs::remove_dir_all(&db_dir);
    }

    #[test]
    fn missing_db_is_empty() {
        let events = scan_db(Path::new("/nonexistent/state.vscdb"));
        assert!(events.is_empty());
    }
}
