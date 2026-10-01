//! Devin CLI (local) scanner.
//!
//! The user-facing Devin CLI keeps per-session state under its data dir —
//! `$DEVIN_DATA_DIR`, else the platform `devin/cli` store
//! (`~/.local/share/devin/cli`, `~/Library/Application Support/devin/cli`,
//! `%APPDATA%\devin\cli`):
//!
//! - `sessions.db` — SQLite (WAL). `sessions` carries id, working_directory,
//!   model, agent_mode, hidden (internal helper sessions); `message_nodes`
//!   carries one `chat_message` JSON per node. Assistant nodes hold
//!   `metadata.metrics` with real token fields
//!   (`input_tokens`/`output_tokens`/`cache_read_tokens`/
//!   `cache_creation_tokens`) plus `metadata.generation_model` and a
//!   per-message RFC3339 `metadata.created_at` (the column is a batch write
//!   time — the JSON value is authoritative). A request is written as 2-3
//!   nodes carrying identical metrics, so events dedupe per
//!   message_id/request_id/node within a scan and by `event_hash` after.
//!   Incremental scans resume at the stored `row_id` watermark (kept in the
//!   offset slot; WAL means file mtime is not a change signal).
//! - `transcripts/*.json` — ATIF exports Devin keeps after pruning
//!   sessions.db. Steps carry `metrics.{prompt_tokens,completion_tokens,
//!   cached_tokens}`; `prompt_tokens` INCLUDES cached (split here). A
//!   transcript is read only for a session the db no longer has — db rows
//!   are finer-grained. `final_metrics` backs a session whose steps carry
//!   no metrics.
//!
//! `credentials.toml` and the `sessions.metadata` ACU/credit counters are
//! not token data and are never read for usage.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::util::{self, collect_files, file_mtime};
use super::{parse_rfc3339, project_name};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "devin-local";
/// Per-scan row bound — a real-world sessions.db measured ~48k nodes.
const MAX_NODE_ROWS: i64 = 200_000;

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

/// Devin CLI store dirs that may contain `sessions.db` / `transcripts/`.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    // DEVIN_DATA_DIR points at the store dir itself (sessions.db inside).
    if let Ok(custom) = std::env::var("DEVIN_DATA_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            roots.push(PathBuf::from(trimmed));
        }
    }
    let home = util::home();
    // XDG default on Linux and (XDG-flavoured builds) macOS.
    roots.push(home.join(".local").join("share").join("devin").join("cli"));
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            roots.push(PathBuf::from(trimmed).join("devin").join("cli"));
        }
    }
    // dirs::data_dir: ~/Library/Application Support on macOS, %APPDATA% on Windows.
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("devin").join("cli"));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let trimmed = local.trim();
        if !trimmed.is_empty() {
            roots.push(PathBuf::from(trimmed).join("devin").join("cli"));
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn scan_root(root: &Path, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    let db = root.join("sessions.db");
    // None = db unreadable: transcripts then count as authoritative.
    let db_sessions = scan_db(&db, store, &mut events).unwrap_or_default();
    events.extend(scan_transcripts(
        &root.join("transcripts"),
        &db_sessions,
        store,
    ));
    events
}

struct SessionMeta {
    working_directory: String,
    model: String,
    agent_mode: String,
}

/// Read `sessions.db`: fill `events` with one event per assistant request and
/// return every session id the db knows (transcripts fill only its gaps).
/// `None` when the db cannot be opened — callers then treat transcripts as
/// authoritative.
fn scan_db(
    path: &Path,
    store: Option<&TokenStore>,
    events: &mut Vec<TokenEvent>,
) -> Option<HashSet<String>> {
    if !path.exists() {
        return Some(HashSet::new());
    }
    let (conn, tmp) = open_readonly(path)?;
    let out = scan_db_conn(&conn, path, store, events);
    drop(conn);
    if let Some(tmp) = tmp {
        let _ = fs::remove_dir_all(tmp);
    }
    Some(out)
}

/// Open the db strictly read-only; on failure retry against a temp copy of
/// the db + WAL/SHM sidecars so a running CLI is never blocked or written to.
fn open_readonly(path: &Path) -> Option<(Connection, Option<PathBuf>)> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let uri = format!("file:{}?mode=ro", path.display());
    if let Ok(conn) = Connection::open_with_flags(&uri, flags) {
        return Some((conn, None));
    }
    let tmp = std::env::temp_dir().join(format!(
        "usg-devin-{}-{}",
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

fn table_columns(conn: &Connection, table: &str) -> HashSet<String> {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return HashSet::new();
    };
    let Ok(iter) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return HashSet::new();
    };
    iter.flatten().collect()
}

fn scan_db_conn(
    conn: &Connection,
    db_path: &Path,
    store: Option<&TokenStore>,
    events: &mut Vec<TokenEvent>,
) -> HashSet<String> {
    let fallback_ts = file_mtime(db_path);

    // sessions → metadata map; ids double as the "db owns this session" set
    // for transcript gap-filling.
    let mut sessions: HashMap<String, SessionMeta> = HashMap::new();
    let mut db_sessions = HashSet::new();
    let scols = table_columns(conn, "sessions");
    if scols.contains("id") {
        let pick = |name: &str, lit: &str| {
            if scols.contains(name) {
                format!("COALESCE({name}, {lit})")
            } else {
                lit.to_string()
            }
        };
        let sql = format!(
            "SELECT id, {}, {}, {}, {} FROM sessions",
            pick("working_directory", "''"),
            pick("model", "''"),
            pick("agent_mode", "''"),
            pick("hidden", "0"),
        );
        if let Ok(mut stmt) = conn.prepare(&sql)
            && let Ok(iter) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
        {
            for row in iter.flatten() {
                let (id, wd, model, mode, hidden) = row;
                db_sessions.insert(id.clone());
                if hidden != 0 {
                    continue; // Devin's own internal sessions (summary agents).
                }
                sessions.insert(
                    id,
                    SessionMeta {
                        working_directory: wd,
                        model,
                        agent_mode: mode,
                    },
                );
            }
        }
    }

    // message_nodes: resume at the stored row watermark when the table has a
    // rowid-ish column; otherwise parse everything (dedupe still applies).
    let ncols = table_columns(conn, "message_nodes");
    if !ncols.contains("session_id") || !ncols.contains("chat_message") {
        return db_sessions;
    }
    let mark_col = ["row_id", "rowid", "node_id"]
        .iter()
        .find(|c| ncols.contains(**c))
        .map(|s| s.to_string());
    let created_expr = if ncols.contains("created_at") {
        "COALESCE(created_at, 0)".to_string()
    } else {
        "0".to_string()
    };

    let after = store
        .and_then(|s| s.scan_offset(db_path).ok().flatten())
        .map(|(off, _)| off as i64)
        .unwrap_or(0);
    let sql = match &mark_col {
        Some(col) => format!(
            "SELECT {col}, session_id, chat_message, {created_expr} FROM message_nodes \
             WHERE {col} > ?1 ORDER BY {col} LIMIT {MAX_NODE_ROWS}"
        ),
        None => format!(
            "SELECT node_id, session_id, chat_message, {created_expr} FROM message_nodes \
             ORDER BY session_id LIMIT {MAX_NODE_ROWS}"
        ),
    };
    let mut rows_out: Vec<(i64, String, String, i64)> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&sql)
        && let Ok(iter) = stmt.query_map(rusqlite::params![after], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
    {
        rows_out.extend(iter.flatten());
    }

    // One event per request: 2-3 nodes carry the same metrics, so the last
    // node per request key wins. Across scans, identical copies dedupe on
    // event_hash at insert.
    let mut keyed: HashMap<String, TokenEvent> = HashMap::new();
    let mut last_mark: Option<i64> = None;
    for (row_id, session_id, raw, created_col) in &rows_out {
        last_mark = Some(last_mark.unwrap_or(0).max(*row_id));
        let Ok(cm) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if cm.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        if !sessions.contains_key(session_id) {
            // Node of a hidden or session-table-pruned row: hidden sessions
            // are internal (no user usage); pruned rows keep their events.
            if db_sessions.contains(session_id) {
                continue;
            }
        }
        let Some(meta) = cm.get("metadata") else {
            continue;
        };
        let Some(metrics) = meta.get("metrics") else {
            continue;
        };
        let get = |k: &str| -> u64 {
            metrics
                .get(k)
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
                .unwrap_or(0)
        };
        let (input, output, cache_read, cache_write) = (
            get("input_tokens"),
            get("output_tokens"),
            get("cache_read_tokens"),
            get("cache_creation_tokens"),
        );
        if input + output + cache_read + cache_write == 0 {
            continue;
        }
        let sess = sessions.get(session_id);
        let model = meta
            .get("generation_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| sess.map(|s| s.model.clone()).filter(|s| !s.is_empty()))
            .or_else(|| sess.map(|s| s.agent_mode.clone()).filter(|s| !s.is_empty()))
            .or_else(|| Some("devin".to_string()));
        let project = sess
            .map(|s| s.working_directory.as_str())
            .and_then(|wd| project_name(wd, ""));
        let ts = meta
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339)
            .unwrap_or_else(|| {
                if *created_col > 0 {
                    *created_col as f64
                } else {
                    fallback_ts
                }
            });
        let key = cm
            .get("message_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                meta.get("request_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("node:{row_id}"));
        keyed.insert(
            format!("{session_id}|{key}"),
            TokenEvent {
                provider: PROVIDER.into(),
                model,
                session_id: Some(session_id.clone()),
                project,
                ts_unix: ts,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
            },
        );
    }
    events.extend(keyed.into_values());

    if let (Some(mark), Some(_col)) = (last_mark, &mark_col) {
        util::mark_scanned(store, db_path, mark as u64);
    }
    db_sessions
}

/// `transcripts/*.json` (ATIF). Emits per-step events for sessions the db
/// does not have; whole-file rescans are skipped by size+mtime.
fn scan_transcripts(
    dir: &Path,
    db_sessions: &HashSet<String>,
    store: Option<&TokenStore>,
) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for path in collect_files(std::slice::from_ref(&dir.to_path_buf()), &["json"]) {
        if !util::whole_file_changed(store, &path) {
            continue;
        }
        if let Ok(meta) = fs::metadata(&path) {
            util::mark_scanned(store, &path, meta.len());
        }
        events.extend(parse_transcript(&path, db_sessions));
    }
    events
}

fn parse_transcript(path: &Path, db_sessions: &HashSet<String>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    let Some(tf) = util::read_json(path) else {
        return events;
    };
    let session_id = tf
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        });
    let owned_by_db = session_id
        .as_ref()
        .map(|id| db_sessions.contains(id))
        .unwrap_or(false);
    if owned_by_db {
        return events;
    }
    let fallback_ts = file_mtime(path);
    let agent_model = tf
        .get("agent")
        .and_then(|a| a.get("model_name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut any_step = false;
    let mut last_ts: Option<f64> = None;
    if let Some(steps) = tf.get("steps").and_then(|s| s.as_array()) {
        for step in steps {
            let ts = step
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(parse_rfc3339);
            if let Some(t) = ts {
                last_ts = Some(last_ts.map(|p: f64| p.max(t)).unwrap_or(t));
            }
            let Some(metrics) = step.get("metrics") else {
                continue;
            };
            let get = |k: &str| -> u64 {
                metrics
                    .get(k)
                    .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
                    .unwrap_or(0)
            };
            let (prompt, completion, cached) = (
                get("prompt_tokens"),
                get("completion_tokens"),
                get("cached_tokens"),
            );
            if prompt + completion + cached == 0 {
                continue;
            }
            any_step = true;
            let model = step
                .get("model_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| agent_model.clone())
                .or_else(|| Some("devin".to_string()));
            events.push(TokenEvent {
                provider: PROVIDER.into(),
                model,
                session_id: session_id.clone(),
                project: None, // ATIF carries no working directory.
                ts_unix: ts.unwrap_or(fallback_ts),
                // ATIF prompt_tokens is inclusive of cached_tokens.
                input_tokens: prompt.saturating_sub(cached),
                output_tokens: completion,
                cache_read_tokens: cached,
                cache_write_tokens: 0,
            });
        }
    }

    // Steps without metrics: one session-level event from final_metrics
    // (verified to equal the per-step sums when both exist).
    if !any_step {
        if let Some(fin) = tf.get("final_metrics") {
            let get = |k: &str| -> u64 {
                fin.get(k)
                    .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
                    .unwrap_or(0)
            };
            let (prompt, completion, cached) = (
                get("total_prompt_tokens"),
                get("total_completion_tokens"),
                get("total_cached_tokens"),
            );
            if prompt + completion + cached > 0 {
                events.push(TokenEvent {
                    provider: PROVIDER.into(),
                    model: agent_model.or_else(|| Some("devin".to_string())),
                    session_id,
                    project: None,
                    ts_unix: last_ts.unwrap_or(fallback_ts),
                    input_tokens: prompt.saturating_sub(cached),
                    output_tokens: completion,
                    cache_read_tokens: cached,
                    cache_write_tokens: 0,
                });
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-devin-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_db(dir: &Path) -> PathBuf {
        let db = dir.join("sessions.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                working_directory TEXT,
                model TEXT,
                agent_mode TEXT,
                created_at INTEGER,
                last_activity_at INTEGER,
                title TEXT,
                main_chain_id INTEGER,
                workspace_dirs TEXT,
                hidden INTEGER,
                metadata TEXT
            );
            CREATE TABLE message_nodes (
                row_id INTEGER PRIMARY KEY,
                node_id INTEGER,
                session_id TEXT,
                chat_message TEXT,
                created_at INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, working_directory, model, agent_mode, hidden)
             VALUES ('s-1', '/work/proj', 'swe-2', 'normal', 0), ('s-h', '/w', 'swe-2', 'normal', 1)",
            [],
        )
        .unwrap();
        // Two nodes for one request (streaming + final copies), identical metrics.
        for row_id in [1, 2] {
            conn.execute(
                "INSERT INTO message_nodes (row_id, node_id, session_id, chat_message, created_at)
                 VALUES (?1, ?1, 's-1', ?2, 1759300000)",
                rusqlite::params![
                    row_id,
                    r#"{"message_id":"m-1","role":"assistant","metadata":{"request_id":"r-1","generation_model":"swe-2-max","created_at":"2026-09-30T10:00:00Z","metrics":{"input_tokens":100,"output_tokens":40,"cache_read_tokens":60,"cache_creation_tokens":10}}}"#
                ],
            )
            .unwrap();
        }
        // A second request, user/tool rows, a malformed row, a hidden-session row.
        conn.execute(
            "INSERT INTO message_nodes (row_id, node_id, session_id, chat_message, created_at)
             VALUES
             (3, 3, 's-1', '{\"role\":\"user\",\"content\":\"hi\"}', 1759300001),
             (4, 4, 's-1', 'not json', 1759300002),
             (5, 5, 's-1', '{\"message_id\":\"m-2\",\"role\":\"assistant\",\"metadata\":{\"created_at\":\"2026-09-30T10:05:00Z\",\"metrics\":{\"input_tokens\":7,\"output_tokens\":3}}}', 1759300003),
             (6, 6, 's-h', '{\"message_id\":\"m-3\",\"role\":\"assistant\",\"metadata\":{\"metrics\":{\"input_tokens\":999}}}', 1759300004)",
            [],
        )
        .unwrap();
        db
    }

    #[test]
    fn parses_sessions_db_deduped() {
        let dir = fixture_dir("db");
        let db = write_db(&dir);
        let mut events = Vec::new();
        let ids = scan_db(&db, None, &mut events).unwrap();
        assert_eq!(events.len(), 2, "one event per request, not per node");
        let first = events
            .iter()
            .find(|e| e.input_tokens == 100)
            .expect("request m-1");
        assert_eq!(first.output_tokens, 40);
        assert_eq!(first.cache_read_tokens, 60);
        assert_eq!(first.cache_write_tokens, 10);
        assert_eq!(first.model.as_deref(), Some("swe-2-max"));
        assert_eq!(first.session_id.as_deref(), Some("s-1"));
        assert_eq!(first.project.as_deref(), Some("proj"));
        assert_eq!(
            first.ts_unix,
            parse_rfc3339("2026-09-30T10:00:00Z").unwrap()
        );
        let second = events.iter().find(|e| e.input_tokens == 7).unwrap();
        assert_eq!(second.model.as_deref(), Some("swe-2")); // session model fallback
        assert!(ids.contains("s-1") && ids.contains("s-h"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcripts_fill_gaps_and_split_cached() {
        let dir = fixture_dir("tr");
        let tr = dir.join("transcripts");
        fs::create_dir_all(&tr).unwrap();
        // Session the db still has → skipped.
        fs::write(
            tr.join("s-db.json"),
            r#"{"schema_version":"atif/1","session_id":"s-db","agent":{"model_name":"swe-2"},"steps":[{"step_id":1,"timestamp":"2026-09-01T10:00:00Z","source":"agent","model_name":"swe-2","metrics":{"prompt_tokens":100,"completion_tokens":20,"cached_tokens":80}}],"final_metrics":{"total_prompt_tokens":100,"total_completion_tokens":20,"total_cached_tokens":80}}"#,
        )
        .unwrap();
        // Session pruned from db → per-step events.
        fs::write(
            tr.join("s-old.json"),
            r#"{"schema_version":"atif/1","session_id":"s-old","agent":{"model_name":"swe-1.6"},"steps":[
                {"step_id":1,"timestamp":"2026-09-01T10:00:00Z","source":"user"},
                {"step_id":2,"timestamp":"2026-09-01T10:01:00Z","source":"agent","metrics":{"prompt_tokens":98110,"completion_tokens":1200,"cached_tokens":97354}},
                {"step_id":3,"timestamp":"2026-09-01T10:02:00Z","source":"agent","model_name":"swe-2","metrics":{"prompt_tokens":50,"completion_tokens":10,"cached_tokens":0}}
            ]}"#,
        )
        .unwrap();
        fs::write(tr.join("broken.json"), "{not json").unwrap();

        let db_sessions: HashSet<String> = ["s-db".to_string()].into_iter().collect();
        let events = parse_transcript(&tr.join("s-db.json"), &db_sessions);
        assert!(
            events.is_empty(),
            "db-owned session yields no transcript events"
        );
        let events = parse_transcript(&tr.join("s-old.json"), &db_sessions);
        assert_eq!(events.len(), 2);
        let step = &events[0];
        assert_eq!(step.input_tokens, 98110 - 97354, "prompt includes cached");
        assert_eq!(step.output_tokens, 1200);
        assert_eq!(step.cache_read_tokens, 97354);
        assert_eq!(step.model.as_deref(), Some("swe-1.6")); // agent fallback
        assert_eq!(step.session_id.as_deref(), Some("s-old"));
        assert!(events.iter().all(|e| e.provider == PROVIDER));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn final_metrics_when_steps_have_none() {
        let dir = fixture_dir("fin");
        let tr = dir.join("transcripts");
        fs::create_dir_all(&tr).unwrap();
        fs::write(
            tr.join("s-fin.json"),
            r#"{"session_id":"s-fin","agent":{"model_name":"swe-2"},"steps":[{"step_id":1,"timestamp":"2026-09-02T09:00:00Z","source":"user"}],"final_metrics":{"total_prompt_tokens":300,"total_completion_tokens":50,"total_cached_tokens":100}}"#,
        )
        .unwrap();
        let events = parse_transcript(&tr.join("s-fin.json"), &HashSet::new());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 200);
        assert_eq!(events[0].output_tokens, 50);
        assert_eq!(events[0].cache_read_tokens, 100);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_db_is_empty() {
        let mut events = Vec::new();
        let ids = scan_db(Path::new("/nonexistent/sessions.db"), None, &mut events).unwrap();
        assert!(events.is_empty());
        assert!(ids.is_empty());
    }
}
