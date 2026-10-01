//! opencode (SST) session scanner.
//!
//! What the CLI persists locally (verified against sst/opencode source):
//! - `<data>/opencode.db` / `opencode-<channel>.db` — SQLite (drizzle):
//!   `message(id, session_id, time_created, data)` where `data` is the
//!   MessageV2.Info JSON for `role: "assistant"` rows:
//!   `tokens{input,output,reasoning,cache{read,write}}`, `modelID`,
//!   `providerID`, `time{created,completed}` (ms), `path{cwd,root}`.
//!   `session(id, project_id, directory, ...)` supplies the project dir.
//! - `<data>/storage/` — the older file layout the same records were kept in:
//!   `session/<projectID>/<sessionID>.json` (session meta incl. `directory`)
//!   and `message/<sessionID>/<msgID>.json` (same Info shape).
//!   `part/<msgID>/*.json` carries step parts, no usage — not read.
//! - `<data>` is `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`;
//!   `OPENCODE_DATA` overrides the root when set.
//!
//! `storage/auth.json` holds OAuth material only — never parsed. The ledger
//! emits one event per assistant message that reports real token fields;
//! opencode already stores `input`/`output` decomposed (input excludes
//! cache-read, output excludes reasoning, reasoning is billed at the output
//! rate) so it maps onto the shared buckets as input / output+reasoning /
//! cache.read / cache.write. `model` is the bare `modelID` — `providerID` is
//! the transport vendor, not a model identity, and the price table normalizes
//! vendor prefixes anyway. `project` is the session `directory` basename.
//!
//! The sqlite store is preferred when present; the JSON storage tree is also
//! scanned — a build migrated from files to db produces identical events
//! (same session/model/ts/counts) and `event_hash` dedupes them.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::util::{self, collect_files, file_mtime, read_json};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "opencode";
const DB_CHUNK: i64 = 5_000;

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for root in scan_roots() {
        if !root.is_dir() {
            continue;
        }
        events.extend(scan_root(&root, store.as_ref()));
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    let home = util::home();
    let mut roots = Vec::new();
    for env in ["OPENCODE_DATA", "XDG_DATA_HOME"] {
        if let Ok(custom) = std::env::var(env) {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                let base = PathBuf::from(trimmed);
                roots.push(if env == "XDG_DATA_HOME" {
                    base.join("opencode")
                } else {
                    base
                });
            }
        }
    }
    roots.push(home.join(".local/share/opencode"));
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("opencode"));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// One data root: sqlite store(s) plus the JSON storage tree.
fn scan_root(root: &Path, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for db in opencode_dbs(root) {
        // Whole-file skip: an unchanged db re-emits identical events.
        if !util::whole_file_changed(store, &db) {
            continue;
        }
        events.extend(scan_db(&db));
        if let Ok(meta) = fs::metadata(&db) {
            util::mark_scanned(store, &db, meta.len());
        }
    }
    events.extend(scan_storage(&root.join("storage"), store));
    events
}

/// `opencode.db` + channel-suffixed `opencode-<channel>.db` files.
fn opencode_dbs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("opencode") && name.ends_with(".db") {
            out.push(entry.path());
        }
    }
    out.sort();
    out
}

// ---------- sqlite store ----------

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .is_ok()
}

/// Open read-only; on failure retry against a temp copy of the db + WAL/SHM
/// sidecars so a running opencode is never blocked or written to.
fn open_readonly(path: &Path) -> Option<(Connection, Option<PathBuf>)> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let uri = format!("file:{}?mode=ro", path.display());
    if let Ok(conn) = Connection::open_with_flags(&uri, flags) {
        return Some((conn, None));
    }
    let tmp = std::env::temp_dir().join(format!(
        "usg-opencode-{}-{}",
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

/// session id → `directory` (project path) from the `session` table.
fn session_dirs(conn: &Connection) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if !table_exists(conn, "session") {
        return map;
    }
    let Ok(mut stmt) = conn.prepare("SELECT id, directory FROM session") else {
        return map;
    };
    let Ok(iter) = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return map;
    };
    for row in iter.flatten() {
        map.insert(row.0, row.1);
    }
    map
}

fn scan_db(path: &Path) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    let Some((conn, tmp)) = open_readonly(path) else {
        return events;
    };
    if table_exists(&conn, "message") {
        let dirs = session_dirs(&conn);
        let mut offset = 0i64;
        loop {
            let Ok(mut stmt) = conn.prepare(
                "SELECT id, session_id, time_created, data FROM message
                 ORDER BY time_created LIMIT ?1 OFFSET ?2",
            ) else {
                break;
            };
            let Ok(rows) = stmt
                .query_map(rusqlite::params![DB_CHUNK, offset], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map(|iter| iter.collect::<Vec<_>>())
            else {
                break;
            };
            let n = rows.len() as i64;
            for row in rows.into_iter().flatten() {
                let (_, session_id, time_created, raw) = row;
                let Ok(data) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                let project = dirs.get(&session_id).and_then(|d| project_name(d));
                if let Some(ev) = message_event(
                    &data,
                    Some(session_id),
                    project,
                    time_created as f64 / 1000.0,
                ) {
                    events.push(ev);
                }
            }
            offset += n;
            if n < DB_CHUNK {
                break;
            }
        }
    }
    drop(conn);
    if let Some(tmp) = tmp {
        let _ = fs::remove_dir_all(tmp);
    }
    events
}

// ---------- JSON storage tree ----------

fn scan_storage(storage: &Path, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    if !storage.is_dir() {
        return Vec::new();
    }
    let dirs = json_session_dirs(&storage.join("session"));
    let mut events = Vec::new();
    for file in collect_files(&[storage.join("message")], &["json"]) {
        if !util::whole_file_changed(store, &file) {
            continue;
        }
        let session_id = file
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string);
        if let Some(data) = read_json(&file) {
            let sid = data
                .get("sessionID")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or(session_id);
            let project = sid
                .as_deref()
                .and_then(|id| dirs.get(id))
                .and_then(|d| project_name(d));
            let fallback = file_mtime(&file);
            if let Some(ev) = message_event(&data, sid, project, fallback) {
                events.push(ev);
            }
        }
        if let Ok(meta) = fs::metadata(&file) {
            util::mark_scanned(store, &file, meta.len());
        }
    }
    events
}

/// session id → `directory` from `storage/session/**/*.json`.
fn json_session_dirs(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for file in collect_files(&[root.to_path_buf()], &["json"]) {
        let Some(data) = read_json(&file) else {
            continue;
        };
        let Some(id) = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                file.file_stem()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
        else {
            continue;
        };
        if let Some(dir) = data.get("directory").and_then(|v| v.as_str()) {
            map.insert(id, dir.to_string());
        }
    }
    map
}

// ---------- shared ----------

/// opencode's decomposed buckets → ledger buckets. Reasoning is billed at
/// the output rate upstream, so it folds into output (same convention as
/// `usage_counts`).
fn opencode_tokens(v: &Value) -> Option<util::UsageCounts> {
    let tokens = v.get("tokens")?.as_object()?;
    let n = |v: Option<&Value>| v.and_then(|v| v.as_u64()).unwrap_or(0);
    let counts = util::UsageCounts {
        input: n(tokens.get("input")),
        output: n(tokens.get("output")).saturating_add(n(tokens.get("reasoning"))),
        cache_read: n(tokens.get("cache").and_then(|c| c.get("read"))),
        cache_write: n(tokens.get("cache").and_then(|c| c.get("write"))),
    };
    (!counts.is_empty()).then_some(counts)
}

/// `modelID` top-level, or the older `model{modelID}` nest.
fn model_id(v: &Value) -> Option<String> {
    v.get("modelID")
        .or_else(|| v.get("model").and_then(|m| m.get("modelID")))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `time.created` (ms epoch) on the Info record.
fn created_unix(v: &Value) -> Option<f64> {
    v.get("time")
        .and_then(|t| t.get("created"))
        .and_then(|v| v.as_f64())
        .map(|ms| ms / 1000.0)
}

fn project_name(directory: &str) -> Option<String> {
    let name = Path::new(directory.trim_end_matches('/'))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())?;
    (!name.is_empty()).then_some(name)
}

/// One event per assistant message with real usage.
fn message_event(
    data: &Value,
    session_id: Option<String>,
    project: Option<String>,
    fallback_ts: f64,
) -> Option<TokenEvent> {
    if data.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    let counts = opencode_tokens(data)?;
    Some(TokenEvent {
        provider: PROVIDER.into(),
        model: model_id(data),
        session_id,
        project: project.or_else(|| {
            ["root", "cwd"]
                .iter()
                .find_map(|k| {
                    data.get("path")
                        .and_then(|p| p.get(*k))
                        .and_then(|v| v.as_str())
                })
                .and_then(project_name)
        }),
        ts_unix: created_unix(data).unwrap_or(fallback_ts),
        input_tokens: counts.input,
        output_tokens: counts.output,
        cache_read_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-opencode-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn write_session(storage: &Path, project: &str, id: &str, directory: &str) {
        let dir = storage.join("session").join(project);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{id}.json")),
            format!(
                r#"{{"id":"{id}","projectID":"{project}","directory":"{directory}","title":"t","version":"0.6.0","time":{{"created":1759300000000,"updated":1759300001000}}}}"#
            ),
        )
        .unwrap();
    }

    fn write_message(storage: &Path, session: &str, id: &str, body: &str) {
        let dir = storage.join("message").join(session);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{id}.json")), body).unwrap();
    }

    #[test]
    fn json_layout_emits_assistant_events() {
        let dir = fixture_dir("json");
        let storage = dir.join("storage");
        write_session(&storage, "proj-9", "ses_1", "/home/u/work/myapp");
        write_message(
            &storage,
            "ses_1",
            "msg_a",
            r#"{"id":"msg_a","sessionID":"ses_1","role":"user","time":{"created":1759300000100}}"#,
        );
        write_message(
            &storage,
            "ses_1",
            "msg_b",
            r#"{"id":"msg_b","sessionID":"ses_1","role":"assistant","modelID":"claude-sonnet-4-5","providerID":"anthropic","time":{"created":1759300000500},"path":{"cwd":"/home/u/work/myapp","root":"/home/u/work/myapp"},"tokens":{"input":500,"output":120,"reasoning":30,"cache":{"read":80,"write":12}}}"#,
        );
        // Assistant message still streaming: tokens all zero → no event.
        write_message(
            &storage,
            "ses_1",
            "msg_c",
            r#"{"id":"msg_c","sessionID":"ses_1","role":"assistant","modelID":"gpt-5","time":{"created":1759300000600},"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
        );
        // Malformed file skipped, never fails the scan.
        write_message(&storage, "ses_1", "msg_bad", "{not json");
        let events = scan_storage(&storage, None);
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.provider, "opencode");
        assert_eq!(ev.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(ev.session_id.as_deref(), Some("ses_1"));
        assert_eq!(ev.project.as_deref(), Some("myapp"));
        assert_eq!(ev.ts_unix, 1759300000.5);
        assert_eq!(ev.input_tokens, 500);
        assert_eq!(ev.output_tokens, 150); // output + reasoning
        assert_eq!(ev.cache_read_tokens, 80);
        assert_eq!(ev.cache_write_tokens, 12);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_older_model_nest_and_path_fallback() {
        let dir = fixture_dir("nest");
        let storage = dir.join("storage");
        // No session file: project falls back to message path.root.
        write_message(
            &storage,
            "ses_x",
            "msg_1",
            r#"{"id":"msg_1","sessionID":"ses_x","role":"assistant","model":{"providerID":"openai","modelID":"gpt-5-codex"},"time":{"created":1759300000000},"path":{"cwd":"/repo/sub","root":"/repo"},"tokens":{"input":10,"output":5}}"#,
        );
        let events = scan_storage(&storage, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(events[0].project.as_deref(), Some("repo"));
        assert_eq!(events[0].cache_read_tokens, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn db_layout_emits_assistant_events() {
        let dir = fixture_dir("db");
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, directory TEXT);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
                 INSERT INTO session VALUES ('ses_1', 'proj-9', '/home/u/work/myapp');",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    "msg_b",
                    "ses_1",
                    1759300000500i64,
                    1759300000500i64,
                    r#"{"role":"assistant","modelID":"kimi-k2","providerID":"opencode","time":{"created":1759300000500},"tokens":{"input":1000,"output":200,"reasoning":50,"cache":{"read":0,"write":0}}}"#
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    "msg_u",
                    "ses_1",
                    1759300000100i64,
                    1759300000100i64,
                    r#"{"role":"user","time":{"created":1759300000100}}"#
                ],
            )
            .unwrap();
        }
        let events = scan_db(&db);
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.provider, "opencode");
        assert_eq!(ev.model.as_deref(), Some("kimi-k2"));
        assert_eq!(ev.session_id.as_deref(), Some("ses_1"));
        assert_eq!(ev.project.as_deref(), Some("myapp"));
        assert_eq!(ev.output_tokens, 250);
        assert_eq!(ev.ts_unix, 1759300000.5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dirs_and_db_are_empty() {
        let dir = fixture_dir("empty");
        assert!(scan_root(&dir.join("nope"), None).is_empty());
        assert!(scan_storage(&dir.join("storage"), None).is_empty());
        assert!(scan_db(&dir.join("opencode.db")).is_empty());
    }
}
