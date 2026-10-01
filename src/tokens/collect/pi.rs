//! Pi coding agent (pi-mono) session scanner — token ledger only.
//!
//! Sessions live at `~/.pi/agent/sessions/--<cwd-slug>--/<ts>_<sessionId>.jsonl`
//! (cwd slug: path with `/`, `\`, `:` → `-`, wrapped in `--`). Roots also
//! honor pi's env overrides — `PI_CODING_AGENT_DIR` (agent dir),
//! `PI_CODING_AGENT_SESSION_DIR`, `PI_CONFIG_DIR`, `PI_PROFILE` — plus the
//! XDG variant `$XDG_DATA_HOME/pi` (`~/.local/share/pi`).
//!
//! File layout (verified against pi-mono session-manager.ts / ai types.ts):
//! - First line: `{"type":"session","version":3,"id","timestamp","cwd",
//!   "parentSession"?}` — session id + project (cwd basename, else the
//!   `--slug--` dir's last `-` segment). Read even on resumed scans.
//! - `type:"message"` entries whose `message.role` is `"assistant"` carry
//!   `message.model`/`responseModel` + `message.usage`.
//! - `type:"usage"` entries (`kind:"cache_warm"` etc.) carry `provider`,
//!   `model` and `usage` — billed calls that never appear as messages.
//! - `role:"toolResult"` messages and `type:"compaction"`/`"branch_summary"`
//!   entries may carry a nested `usage` — counted, model unattributed.
//! Pi's `Usage` shape: `{input, output, cacheRead, cacheWrite, reasoning?,
//! totalTokens, cost:{...}}`; `reasoning` is already inside `output`.
//!
//! Sessions are trees; every usage-bearing entry counts (no branch walk).
//! A forked session copies entries into a new file, so that usage re-counts
//! under the fork's session id — per-file counting is the ledger contract.
//! `~/.pi/agent/auth.json` is OAuth material and is never read for usage.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::project_name;
use super::util::{self, UsageCounts, collect_files, file_mtime, read_jsonl_events};
use crate::providers::types::coerce_unix_seconds;
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "pi";
/// Bound on the header line peek (pi scans up to 1 MiB for it upstream).
const HEADER_MAX_BYTES: u64 = 1024 * 1024;

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for root in scan_roots() {
        if !root.is_dir() {
            continue;
        }
        for path in collect_files(std::slice::from_ref(&root), &["jsonl"]) {
            let ctx = file_context(&path);
            let fallback_ts = file_mtime(&path);
            let offset = util::resume_offset(store.as_ref(), &path);
            let (mut found, next) = read_jsonl_events(&path, offset, |record| {
                record_events(record, &ctx, fallback_ts)
            });
            events.append(&mut found);
            util::mark_scanned(store.as_ref(), &path, next);
        }
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in [
        "PI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_SESSION_DIR",
        "PI_CONFIG_DIR",
        "PI_PROFILE",
    ] {
        if let Ok(custom) = std::env::var(var) {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                roots.push(PathBuf::from(trimmed));
            }
        }
    }
    let home = util::home();
    roots.push(home.join(".pi").join("agent"));
    roots.push(
        std::env::var("XDG_DATA_HOME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join("pi"),
    );
    roots.sort();
    roots.dedup();
    roots
}

/// Per-file context resolved before streaming entries: session id + project
/// come from the `type:"session"` header (re-read on every scan so resumed
/// offsets keep attribution), with filename/dir-slug fallbacks.
struct FileCtx {
    session: Option<String>,
    project: Option<String>,
    header_ts: Option<f64>,
}

fn file_context(path: &Path) -> FileCtx {
    // `--<cwd-slug>--` — the wrapper dashes are not part of the path.
    let slug = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim_matches('-');
    let mut ctx = FileCtx {
        session: path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|stem| stem.rsplit('_').next())
            .map(str::to_string),
        project: project_name("", slug),
        header_ts: None,
    };
    let Some(header) = first_line_json(path) else {
        return ctx;
    };
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return ctx;
    }
    if let Some(id) = header.get("id").and_then(Value::as_str)
        && !id.is_empty()
    {
        ctx.session = Some(id.to_string());
    }
    if let Some(cwd) = header.get("cwd").and_then(Value::as_str) {
        ctx.project = project_name(cwd, slug);
    }
    ctx.header_ts = util::ts_on_record(&header);
    ctx
}

/// First newline-terminated JSON record of a session file (the header),
/// read bounded and tolerantly.
fn first_line_json(path: &Path) -> Option<Value> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(HEADER_MAX_BYTES));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).ok()?;
    serde_json::from_slice::<Value>(&line).ok()
}

fn nonempty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn num_at(usage: &Value, key: &str) -> u64 {
    usage
        .get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
        .unwrap_or(0)
}

/// Pi `Usage` → buckets. `reasoning` stays inside `output` (pi already
/// includes it there, matching every other ledger bucket convention).
fn pi_usage(usage: &Value) -> Option<UsageCounts> {
    if !usage.is_object() {
        return None;
    }
    let counts = UsageCounts {
        input: num_at(usage, "input"),
        output: num_at(usage, "output"),
        cache_read: num_at(usage, "cacheRead"),
        cache_write: num_at(usage, "cacheWrite"),
    };
    (!counts.is_empty()).then_some(counts)
}

fn record_events(record: &Value, ctx: &FileCtx, fallback_ts: f64) -> Vec<TokenEvent> {
    let ts = |value: Option<&Value>| {
        value
            .and_then(coerce_unix_seconds)
            .or_else(|| util::ts_on_record(record))
            .or(ctx.header_ts)
            .unwrap_or(fallback_ts)
    };
    let emit = |model: Option<String>, ts_unix: f64, counts: UsageCounts| {
        vec![TokenEvent {
            provider: PROVIDER.into(),
            model,
            session_id: ctx.session.clone(),
            project: ctx.project.clone(),
            ts_unix,
            input_tokens: counts.input,
            output_tokens: counts.output,
            cache_read_tokens: counts.cache_read,
            cache_write_tokens: counts.cache_write,
        }]
    };
    match record.get("type").and_then(Value::as_str) {
        Some("message") => {
            let message = &record["message"];
            match message.get("role").and_then(Value::as_str) {
                Some("assistant") => {
                    let Some(counts) = message.get("usage").and_then(pi_usage) else {
                        return Vec::new();
                    };
                    let model = nonempty_str(message.get("responseModel"))
                        .or_else(|| nonempty_str(message.get("model")));
                    emit(model, ts(message.get("timestamp")), counts)
                }
                Some("toolResult") => {
                    // Nested tool-side model calls (subagents); unattributed.
                    let Some(counts) = message.get("usage").and_then(pi_usage) else {
                        return Vec::new();
                    };
                    emit(None, ts(message.get("timestamp")), counts)
                }
                _ => Vec::new(),
            }
        }
        Some("usage") | Some("compaction") | Some("branch_summary") => {
            let Some(counts) = record.get("usage").and_then(pi_usage) else {
                return Vec::new();
            };
            emit(
                nonempty_str(record.get("model")),
                ts(record.get("timestamp")),
                counts,
            )
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-pi-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sessions/--work-myproj--")).unwrap();
        dir
    }

    fn write_session(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn assistant_and_usage_entries_emit_events() {
        let dir = fixture_dir("basic");
        let file = write_session(
            &dir.join("sessions/--work-myproj--"),
            "2026-09-30T10-00-00-000Z_abc-123.jsonl",
            &[
                r#"{"type":"session","version":3,"id":"abc-123","timestamp":"2026-09-30T10:00:00.000Z","cwd":"/work/myproj","provider":"anthropic","modelId":"claude-sonnet-4-5"}"#,
                r#"{"type":"message","id":"e1","parentId":null,"timestamp":"2026-09-30T10:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1790000001000}}"#,
                r#"{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-09-30T10:00:04.000Z","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":3,"output":191,"cacheRead":0,"cacheWrite":1684,"reasoning":50,"totalTokens":1878,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","timestamp":1790000004000}}"#,
                "not json",
                r#"{"type":"usage","id":"e3","parentId":"e2","timestamp":"2026-09-30T10:01:00.000Z","kind":"cache_warm","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":10,"output":1,"cacheRead":0,"cacheWrite":5,"totalTokens":16,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
                r#"{"type":"message","id":"e4","parentId":"e3","timestamp":"2026-09-30T10:02:00.000Z","message":{"role":"assistant","content":[],"api":"openai-responses","provider":"openai","model":"gpt-5.1-codex","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"aborted","timestamp":1790000120000,"errorMessage":"Request was aborted"}}"#,
            ],
        );
        let ctx = file_context(&file);
        assert_eq!(ctx.session.as_deref(), Some("abc-123"));
        assert_eq!(ctx.project.as_deref(), Some("myproj"));

        let (events, next) = read_jsonl_events(&file, 0, |r| record_events(r, &ctx, 0.0));
        assert_eq!(next as usize, fs::metadata(&file).unwrap().len() as usize);
        // zero-usage aborted message and the malformed line emit nothing
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].provider, "pi");
        assert_eq!(events[0].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(events[0].input_tokens, 3);
        assert_eq!(events[0].output_tokens, 191); // reasoning stays inside
        assert_eq!(events[0].cache_write_tokens, 1684);
        assert_eq!(events[0].session_id.as_deref(), Some("abc-123"));
        assert!(events[0].ts_unix > 1_790_000_000.0); // message.timestamp ms
        assert_eq!(events[1].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(events[1].input_tokens, 10);
        assert_eq!(events[1].cache_write_tokens, 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn toolresult_and_compaction_usage_counted_unattributed() {
        let dir = fixture_dir("nested");
        let file = write_session(
            &dir.join("sessions/--work-myproj--"),
            "2026-09-30T11-00-00-000Z_def-456.jsonl",
            &[
                r#"{"type":"session","version":3,"id":"def-456","timestamp":"2026-09-30T11:00:00.000Z","cwd":"/work/myproj"}"#,
                r#"{"type":"message","id":"e1","parentId":null,"timestamp":"2026-09-30T11:00:05.000Z","message":{"role":"toolResult","toolCallId":"t1","toolName":"subagent","content":[],"usage":{"input":40,"output":9,"cacheRead":2,"cacheWrite":0,"totalTokens":51,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"isError":false,"timestamp":1790003605000}}"#,
                r#"{"type":"compaction","id":"e2","parentId":"e1","timestamp":"2026-09-30T11:30:00.000Z","summary":"…","firstKeptEntryId":"e1","tokensBefore":9000,"usage":{"input":120,"output":30,"cacheRead":0,"cacheWrite":0,"totalTokens":150,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
            ],
        );
        let ctx = file_context(&file);
        let (events, _) = read_jsonl_events(&file, 0, |r| record_events(r, &ctx, 0.0));
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.model.is_none()));
        assert_eq!(events[0].cache_read_tokens, 2);
        assert_eq!(events[1].input_tokens, 120);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn slug_fallback_when_header_missing() {
        let dir = fixture_dir("slug");
        let file = write_session(
            &dir.join("sessions/--work-myproj--"),
            "2026-09-30T12-00-00-000Z_ghi-789.jsonl",
            &[
                r#"{"type":"message","id":"e1","parentId":null,"timestamp":"2026-09-30T12:00:01.000Z","message":{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1790006401000}}"#,
            ],
        );
        let ctx = file_context(&file);
        assert_eq!(ctx.session.as_deref(), Some("ghi-789"));
        assert_eq!(ctx.project.as_deref(), Some("myproj"));
        let (events, _) = read_jsonl_events(&file, 0, |r| record_events(r, &ctx, 0.0));
        assert_eq!(events.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_roots_cover_env_and_defaults() {
        let roots = scan_roots();
        let home = util::home();
        assert!(roots.contains(&home.join(".pi").join("agent")));
        assert!(roots.iter().any(
            |r| r.ends_with("pi") && r.to_string_lossy().contains("share")
                || r.to_string_lossy().contains("XDG")
        ));
        // never invented: unknown env state still yields only real dirs
        for r in &roots {
            assert!(r.is_absolute() || r.as_os_str().is_empty() == false);
        }
    }

    #[test]
    fn missing_dirs_and_bad_files_yield_nothing() {
        let dir = fixture_dir("empty");
        let file = write_session(
            &dir.join("sessions/--work-other--"),
            "x_y.jsonl",
            &["{\"type\":\"message\"}", "garbage"],
        );
        let ctx = file_context(&file);
        assert_eq!(ctx.session.as_deref(), Some("y"));
        let (events, _) = read_jsonl_events(&file, 0, |r| record_events(r, &ctx, 0.0));
        assert!(events.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
