//! Oh My Pi (omp) session scanner — omp.sh, github.com/can1357/oh-my-pi,
//! the coding-agent fork of Mario Zechner's pi-mono.
//!
//! Session files (docs/session.md, packages/utils/src/dirs.ts):
//! - `<agent>/sessions/<encoded-cwd>/<ts>_<sessionId>.jsonl` — encoded-cwd is
//!   `-<rel-path>` under home, `-tmp-<rel-path>` under tmp, `--<abs-path>--`
//!   elsewhere, or `<scope>-<name>-<sha256>` (17.2.5–17.2.8 hashed scheme).
//! - Subagent/advisor sessions: `<ts>_<sessionId>/<AgentName>.jsonl` sibling
//!   files under a dir named after the parent stem — each a complete session
//!   file with its own header, so the recursive `*.jsonl` scan finds them.
//!
//! Roots mirror omp's own resolution (`pi-utils/dirs`):
//! - `PI_CODING_AGENT_DIR` → `<dir>/sessions` (default-profile override)
//! - `OMP_PROFILE`/`PI_PROFILE` → `~/<PI_CONFIG_DIR|.omp>/profiles/<n>/agent`
//! - `PI_CONFIG_DIR` → `~/<dir>/agent` (config dir name, default `.omp`)
//! - `$XDG_DATA_HOME/omp` — XDG flattens the `agent/` prefix away
//! - `~/.omp/agent` default; `~/.pi/agent` for upstream-pi leftovers (same
//!   record format — the fork kept it byte-identical).
//!
//! File format: JSONL. Current files open with a fixed-width 256-byte
//! `{"type":"title",...}` slot, then the `{"type":"session","id","timestamp",
//! "cwd"}` header, then append-only tree records `{type,id,parentId,
//! timestamp,...}`. Usage rides only on `type:"message"` entries whose
//! `message.role == "assistant"` (matching tokscale's pi adapter):
//! `message.usage = {input, output, cacheRead, cacheWrite, totalTokens,
//! cost}` — all four token buckets are disjoint, and `reasoning` is already
//! inside `output`. `model_change` entries (`model: "provider/modelId"`)
//! seed the model for entries that lack one. `message.timestamp` is Unix ms;
//! the entry `timestamp` is RFC3339.
//!
//! Entries are append-only (branching moves a `leafId` pointer; the title
//! slot is rewritten in place at fixed width), so byte-offset incremental
//! scans are safe. No auth or credential files are ever read.

use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util::{
    UsageCounts, collect_files, file_mtime, read_jsonl_events, resume_offset, ts_on_record,
};
use super::{project_name, util};
use crate::providers::types::coerce_unix_seconds;
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "omp";

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for root in scan_roots() {
        let sessions = root.join("sessions");
        if !sessions.is_dir() {
            continue;
        }
        for file in collect_files(std::slice::from_ref(&sessions), &["jsonl"]) {
            let offset = resume_offset(store.as_ref(), &file);
            let (mut found, next) = parse_session(&sessions, &file, offset);
            events.append(&mut found);
            util::mark_scanned(store.as_ref(), &file, next);
        }
    }
    events
}

/// Agent dirs whose `sessions/` subtree is scanned.
pub fn scan_roots() -> Vec<PathBuf> {
    let home = util::home();
    // PI_CONFIG_DIR is a config dir *name* relative to home (default ".omp").
    let config_dir = std::env::var("PI_CONFIG_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { home.join(p) })
        .unwrap_or_else(|| home.join(".omp"));

    let xdg_omp = std::env::var("XDG_DATA_HOME")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("omp"));

    let profile = std::env::var("OMP_PROFILE")
        .ok()
        .or_else(|| std::env::var("PI_PROFILE").ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "default" && !v.contains('/') && !v.contains(".."));

    let mut roots = Vec::new();
    if let Some(name) = profile {
        // Named profiles derive their own agent dir and ignore the
        // PI_CODING_AGENT_DIR override (mirroring pi-utils).
        roots.push(config_dir.join("profiles").join(&name).join("agent"));
        if let Some(xdg) = xdg_omp {
            roots.push(xdg.join("profiles").join(&name));
        }
    } else {
        match std::env::var("PI_CODING_AGENT_DIR") {
            Ok(dir) if !dir.trim().is_empty() => roots.push(PathBuf::from(dir.trim())),
            _ => roots.push(config_dir.join("agent")),
        }
        if let Some(xdg) = xdg_omp {
            roots.push(xdg);
        }
        roots.push(home.join(".pi").join("agent"));
    }
    roots.sort();
    roots.dedup();
    roots
}

struct FileState {
    session_id: Option<String>,
    project: Option<String>,
    model: Option<String>,
}

impl FileState {
    fn new(path: &Path, sessions_root: &Path) -> Self {
        // Fallbacks for damaged/headerless files: `<ts>_<id>` stem and the
        // `<encoded-cwd>` bucket dir directly under sessions/.
        let session_id = path.file_stem().and_then(|s| s.to_str()).map(|stem| {
            stem.split_once('_')
                .map(|(_, id)| id)
                .unwrap_or(stem)
                .to_string()
        });
        let slug = path
            .strip_prefix(sessions_root)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            session_id,
            project: slug_project(&slug),
            model: None,
        }
    }

    fn apply(&mut self, record: &Value, fallback_ts: f64) -> Vec<TokenEvent> {
        match record.get("type").and_then(|v| v.as_str()) {
            Some("session") => {
                // The header id wins over the `<ts>_<id>` filename fallback.
                if let Some(id) = record.get("id").and_then(|v| v.as_str()) {
                    self.session_id = Some(id.to_string());
                }
                // The header cwd wins over the bucket-slug fallback.
                if let Some(cwd) = record.get("cwd").and_then(|v| v.as_str())
                    && let Some(name) = project_name(cwd, "")
                {
                    self.project = Some(name);
                }
            }
            Some("model_change") => {
                if let Some(m) = str_on_record(record, &["model", "modelId", "model_id"]) {
                    self.model = Some(m);
                }
            }
            Some("message") => {
                let Some(message) = record.get("message") else {
                    return Vec::new();
                };
                if let Some(m) = str_on_record(message, &["model"]) {
                    self.model = Some(m);
                }
                if message.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                    return Vec::new();
                }
                let Some(counts) = message.get("usage").and_then(omp_usage_counts) else {
                    return Vec::new();
                };
                let ts = message
                    .get("timestamp")
                    .and_then(coerce_unix_seconds)
                    .or_else(|| ts_on_record(record))
                    .unwrap_or(fallback_ts);
                return vec![TokenEvent {
                    provider: PROVIDER.into(),
                    model: self.model.clone(),
                    session_id: self.session_id.clone(),
                    project: self.project.clone(),
                    ts_unix: ts,
                    input_tokens: counts.input,
                    output_tokens: counts.output,
                    cache_read_tokens: counts.cache_read,
                    cache_write_tokens: counts.cache_write,
                }];
            }
            _ => {}
        }
        Vec::new()
    }
}

fn str_on_record(record: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        record
            .get(*k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// pi-ai `Usage`: `{input, output, cacheRead, cacheWrite, totalTokens, cost}`.
/// The four buckets are disjoint and `reasoning` is already inside `output`
/// (pi's `totalTokens` has no reasoning bucket), so it is intentionally not
/// read — mapping it would double count.
fn omp_usage_counts(usage: &Value) -> Option<UsageCounts> {
    let num = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| {
                usage
                    .get(*k)
                    .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
            })
            .unwrap_or(0)
    };
    let counts = UsageCounts {
        input: num(&[
            "input",
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
        ]),
        output: num(&[
            "output",
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
        ]),
        cache_read: num(&[
            "cacheRead",
            "cache_read",
            "cachedReadTokens",
            "cacheReadTokens",
            "cache_read_input_tokens",
            "cached",
            "cached_tokens",
            "cachedTokens",
        ]),
        cache_write: num(&[
            "cacheWrite",
            "cache_write",
            "cachedWriteTokens",
            "cacheWriteTokens",
            "cacheCreationTokens",
            "cache_creation_input_tokens",
        ]),
    };
    (!counts.is_empty()).then_some(counts)
}

/// Last non-empty `-` segment of a bucket dir name, skipping the trailing
/// sha256 of the short-lived hashed scheme (`<scope>-<name>-<digest>`).
fn slug_project(slug: &str) -> Option<String> {
    let segs: Vec<&str> = slug.split('-').filter(|s| !s.is_empty()).collect();
    for seg in segs.iter().rev() {
        if seg.len() == 64 && seg.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        return Some((*seg).to_string());
    }
    None
}

fn parse_session(sessions_root: &Path, path: &Path, offset: u64) -> (Vec<TokenEvent>, u64) {
    let fallback_ts = file_mtime(path);
    let state = RefCell::new(FileState::new(path, sessions_root));
    if offset > 0 {
        // The stored offset is past the header; re-apply the first records so
        // resumed scans keep session_id/project/model.
        replay_head(path, &mut state.borrow_mut(), fallback_ts);
    }
    read_jsonl_events(path, offset, |record| {
        state.borrow_mut().apply(record, fallback_ts)
    })
}

/// Apply the file's first records (title slot + header) until the session
/// header is seen. Emitted events are discarded — that span was already
/// counted when the offset was stored.
fn replay_head(path: &Path, state: &mut FileState, fallback_ts: f64) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let mut line = String::new();
    let mut reader = BufReader::new(file);
    for _ in 0..4 {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(record) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let _ = state.apply(&record, fallback_ts);
        if state.session_id.is_some() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("usg-omp-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        let sessions = dir.join("agent/sessions");
        fs::create_dir_all(sessions.join("-work-proj")).unwrap();
        (dir, sessions)
    }

    const HEADER: &str = r#"{"type":"session","version":3,"id":"sess-abc","timestamp":"2026-09-20T10:00:00.000Z","cwd":"/work/proj"}"#;

    #[test]
    fn parses_header_and_assistant_usage() {
        let (_dir, sessions) = fixture("full");
        let file = sessions.join("-work-proj/2026-09-20T10-00_sess-abc.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        // Current-format 256-byte title slot before the header.
        writeln!(f, r#"{{"type":"title","v":1,"title":"x","updatedAt":"2026-09-20T10:00:01.000Z","pad":"    "}}"#).unwrap();
        writeln!(f, "{HEADER}").unwrap();
        writeln!(f, r#"{{"type":"message","id":"e1","parentId":null,"timestamp":"2026-09-20T10:01:00.000Z","message":{{"role":"user","content":"hi","timestamp":1790000000000}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"model_change","id":"e2","parentId":"e1","timestamp":"2026-09-20T10:01:30.000Z","model":"anthropic/claude-sonnet-4-5"}}"#).unwrap();
        // Assistant turn on an abandoned branch: model falls back to model_change.
        writeln!(f, r#"{{"type":"message","id":"e4","parentId":"e1","timestamp":"2026-09-20T10:03:00.000Z","message":{{"role":"assistant","provider":"anthropic","usage":{{"input":20,"output":8,"cacheRead":0,"cacheWrite":0,"totalTokens":28}},"timestamp":1790000180000}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"message","id":"e3","parentId":"e2","timestamp":"2026-09-20T10:02:00.000Z","message":{{"role":"assistant","provider":"openai","model":"gpt-5.1-codex","usage":{{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165,"cost":{{"input":0.1,"output":0.5,"cacheRead":0,"cacheWrite":0,"total":0.6}}}},"timestamp":1790000120000}}}}"#).unwrap();
        // Usage-less assistant (error turn) and non-message entries: nothing.
        writeln!(f, r#"{{"type":"message","id":"e5","parentId":"e4","timestamp":"2026-09-20T10:04:00.000Z","message":{{"role":"assistant","provider":"openai","model":"gpt-5.1-codex","stopReason":"error","timestamp":1790000240000}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"compaction","id":"e6","parentId":"e5","timestamp":"2026-09-20T10:05:00.000Z","summary":"...","tokensBefore":42000,"usage":{{"input":9,"output":9}}}}"#).unwrap();
        writeln!(f, "not json {{").unwrap();
        drop(f);

        let (events, _next) = parse_session(&sessions, &file, 0);
        assert_eq!(events.len(), 2);
        // Branch entry: model_change fallback, session/project from header.
        assert_eq!(
            events[0].model.as_deref(),
            Some("anthropic/claude-sonnet-4-5")
        );
        assert_eq!(events[0].input_tokens, 20);
        assert_eq!(events[0].output_tokens, 8);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-abc"));
        assert_eq!(events[0].project.as_deref(), Some("proj"));
        let e = &events[1];
        assert_eq!(e.provider, "omp");
        assert_eq!(e.input_tokens, 100);
        assert_eq!(e.output_tokens, 50);
        assert_eq!(e.cache_read_tokens, 10);
        assert_eq!(e.cache_write_tokens, 5);
        assert_eq!(e.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(e.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(e.project.as_deref(), Some("proj"));
        assert_eq!(e.ts_unix, 1_790_000_120.0);
        let _ = fs::remove_dir_all(&_dir);
    }

    #[test]
    fn resumes_from_stored_offset() {
        let (_dir, sessions) = fixture("resume");
        let file = sessions.join("-work-proj/2026-09-20T11-00_sess-9.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        writeln!(f, "{HEADER}").unwrap();
        writeln!(f, r#"{{"type":"message","id":"e1","parentId":null,"timestamp":"2026-09-20T11:01:00.000Z","message":{{"role":"assistant","model":"gpt-5","usage":{{"input":10,"output":5}},"timestamp":1790001000000}}}}"#).unwrap();
        drop(f);
        let (first, next) = parse_session(&sessions, &file, 0);
        assert_eq!(first.len(), 1);

        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(f, r#"{{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-09-20T11:02:00.000Z","message":{{"role":"assistant","model":"gpt-5","usage":{{"input":7,"output":3,"cacheRead":2}},"timestamp":1790001060000}}}}"#).unwrap();
        drop(f);
        let (more, _) = parse_session(&sessions, &file, next);
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].input_tokens, 7);
        assert_eq!(more[0].cache_read_tokens, 2);
        // Header state survived the resume.
        assert_eq!(more[0].session_id.as_deref(), Some("sess-abc"));
        assert_eq!(more[0].project.as_deref(), Some("proj"));
        let _ = fs::remove_dir_all(&_dir);
    }

    #[test]
    fn subagent_files_and_slug_fallback() {
        let (_dir, sessions) = fixture("sub");
        // Subagent session under a `<ts>_<id>/` dir inside a hashed bucket;
        // header cwd missing → project falls back to the bucket's readable
        // segment (the trailing sha256 is skipped).
        let sub = sessions.join(
            "home-me-proj-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/2026-09-20T12-00_sess-p/scout.jsonl",
        );
        fs::create_dir_all(sub.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&sub).unwrap();
        writeln!(f, r#"{{"type":"session","version":3,"id":"sess-child","timestamp":"2026-09-20T12:00:00.000Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"message","id":"e1","parentId":null,"timestamp":"2026-09-20T12:01:00.000Z","message":{{"role":"assistant","model":"glm-5.2","usage":{{"input":3,"output":2}},"timestamp":1790002000000}}}}"#).unwrap();
        drop(f);
        let (events, _) = parse_session(&sessions, &sub, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-child"));
        assert_eq!(events[0].project.as_deref(), Some("proj"));
        let _ = fs::remove_dir_all(&_dir);
    }

    #[test]
    fn hashed_bucket_skips_sha_for_project() {
        assert_eq!(
            slug_project(
                "home-myproj-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            Some("myproj".into())
        );
        assert_eq!(slug_project("--work-proj--"), Some("proj".into()));
        assert_eq!(slug_project("-tmp-build-thing-"), Some("thing".into()));
        assert_eq!(slug_project("-"), None);
    }

    #[test]
    fn no_usage_fields_means_no_events() {
        let (_dir, sessions) = fixture("none");
        let file = sessions.join("-work-proj/2026-09-20T13-00_sess-0.jsonl");
        fs::write(
            &file,
            format!("{HEADER}\n{{\"type\":\"message\",\"id\":\"e1\",\"parentId\":null,\"timestamp\":\"2026-09-20T13:01:00.000Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"gpt-5\",\"timestamp\":1790003000000}}}}\n"),
        )
        .unwrap();
        let (events, _) = parse_session(&sessions, &file, 0);
        assert!(events.is_empty());
        let _ = fs::remove_dir_all(&_dir);
    }
}
