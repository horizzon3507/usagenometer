//! Command Code session scanner — token ledger only.
//!
//! Transcripts live at `~/.commandcode/projects/<cwd-slug>/<session-id>.jsonl`
//! (`<cwd-slug>` is the session cwd with `/` → `-`). Sibling files ending in
//! `.checkpoints.jsonl` are editor checkpoints and carry no token usage.
//!
//! File layout (verified against the CLI's own transcripts):
//! - First line: `{"type":"session","version":3,"id","timestamp","cwd"}` —
//!   session id + project (cwd basename, else the slug's last `-` segment).
//! - `type:"message"` entries whose `message.role` is `"assistant"` carry
//!   top-level `usage {inputTokens, outputTokens, cacheReadTokens,
//!   cacheWriteTokens, costUsd}` and `model` (`provider/model`). User and
//!   tool-result entries carry no usage.
//! - `timestamp` is RFC3339; `message.meta.createdAt` is Unix ms.
//!
//! Files are append-only, so byte-offset incremental scans are safe. The
//! session header is re-read on every scan to keep attribution after a
//! resumed offset. No auth files are ever read.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::project_name;
use super::util::{self, collect_files, file_mtime, read_jsonl_events};
use crate::providers::types::coerce_unix_seconds;
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "commandcode";
const HEADER_MAX_BYTES: u64 = 64 * 1024;

pub fn scan() -> Vec<TokenEvent> {
    scan_with_store(TokenStore::open().ok().as_ref())
}

pub(crate) fn scan_with_store(store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for root in scan_roots() {
        events.extend(scan_root(&root, store));
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    vec![util::home().join(".commandcode")]
}

/// One Command Code data root: every project transcript under `projects/`.
pub(crate) fn scan_root(root: &Path, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let projects = root.join("projects");
    if !projects.is_dir() {
        return Vec::new();
    }
    let mut events = Vec::new();
    for path in collect_files(std::slice::from_ref(&projects), &["jsonl"]) {
        if is_checkpoint(&path) {
            continue;
        }
        let ctx = file_context(&path);
        let fallback_ts = file_mtime(&path);
        let offset = util::resume_offset(store, &path);
        let (mut found, next) = read_jsonl_events(&path, offset, |record| {
            record_events(record, &ctx, fallback_ts)
        });
        events.append(&mut found);
        util::mark_scanned(store, &path, next);
    }
    events
}

/// Editor checkpoint sidecars share the `.jsonl` extension but hold no usage.
fn is_checkpoint(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".checkpoints.jsonl"))
}

struct FileCtx {
    session: Option<String>,
    project: Option<String>,
}

/// Header-derived context with filename/slug fallbacks for damaged files.
fn file_context(path: &Path) -> FileCtx {
    let slug = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let mut ctx = FileCtx {
        session: path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .map(str::to_string),
        project: project_name("", slug),
    };
    let Some(header) = first_line_json(path) else {
        return ctx;
    };
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return ctx;
    }
    if let Some(id) = nonempty_str(header.get("id")) {
        ctx.session = Some(id);
    }
    if let Some(cwd) = header.get("cwd").and_then(Value::as_str) {
        ctx.project = project_name(cwd, slug);
    }
    ctx
}

/// First JSON record of a transcript (the session header), read bounded.
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
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn record_events(record: &Value, ctx: &FileCtx, fallback_ts: f64) -> Vec<TokenEvent> {
    if record.get("type").and_then(Value::as_str) != Some("message") {
        return Vec::new();
    }
    let message = record.get("message").cloned().unwrap_or(Value::Null);
    let role = message
        .get("role")
        .or_else(|| record.get("role"))
        .and_then(Value::as_str);
    if role != Some("assistant") {
        return Vec::new();
    }
    let Some(counts) = record.get("usage").and_then(util::usage_counts) else {
        return Vec::new();
    };
    let model = nonempty_str(record.get("model"))
        .or_else(|| nonempty_str(message.get("model")))
        .or_else(|| util::model_on_record(record));
    let ts = util::ts_on_record(record)
        .or_else(|| {
            message
                .pointer("/meta/createdAt")
                .and_then(coerce_unix_seconds)
        })
        .unwrap_or(fallback_ts);
    vec![TokenEvent {
        provider: PROVIDER.into(),
        model,
        session_id: ctx.session.clone(),
        project: ctx.project.clone(),
        ts_unix: ts,
        input_tokens: counts.input,
        output_tokens: counts.output,
        cache_read_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("usg-commandcode-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn write_session(root: &Path, project: &str, id: &str, lines: &[&str]) -> PathBuf {
        let dir = root.join("projects").join(project);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        let mut file = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn assistant_usage_emits_events() {
        let dir = fixture("basic");
        write_session(
            &dir,
            "home-u-work-myapp",
            "sess-1",
            &[
                r#"{"type":"session","version":3,"id":"sess-1","timestamp":"2026-08-22T15:20:55.166Z","cwd":"/home/u/work/myapp"}"#,
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-08-22T15:21:03.977Z","message":{"role":"user","content":[]}}"#,
                r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-22T15:21:04.000Z","message":{"role":"assistant","content":[]},"usage":{"inputTokens":19563,"outputTokens":77,"cacheReadTokens":7680,"cacheWriteTokens":0,"costUsd":0.00278},"model":"deepseek/deepseek-v4-flash"}"#,
                r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-08-22T15:22:00.000Z","message":{"role":"assistant","content":[]},"usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0}}"#,
                "not json {",
            ],
        );
        let events = scan_root(&dir, None);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.provider, "commandcode");
        assert_eq!(event.model.as_deref(), Some("deepseek/deepseek-v4-flash"));
        assert_eq!(event.session_id.as_deref(), Some("sess-1"));
        assert_eq!(event.project.as_deref(), Some("myapp"));
        assert_eq!(event.input_tokens, 19563);
        assert_eq!(event.output_tokens, 77);
        assert_eq!(event.cache_read_tokens, 7680);
        assert_eq!(event.cache_write_tokens, 0);
        assert_eq!(event.ts_unix, 1_787_412_064.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoints_and_slug_fallback_are_ignored() {
        let dir = fixture("slug");
        let project = "home-u-work-myapp";
        let path = write_session(
            &dir,
            project,
            "sess-2",
            &[
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-09-01T10:00:00.000Z","message":{"role":"assistant","content":[],"meta":{"createdAt":1788256800000}},"usage":{"inputTokens":10,"outputTokens":5,"cacheReadTokens":2,"cacheWriteTokens":3},"model":"anthropic/claude-sonnet-4-5"}"#,
            ],
        );
        fs::write(
            dir.join("projects")
                .join(project)
                .join("sess-2.checkpoints.jsonl"),
            r#"{"id":"checkpoint","messageCount":0}"#,
        )
        .unwrap();
        let events = scan_root(&dir, None);
        assert_eq!(events.len(), 1);
        // Headerless file: session id comes from the stem, project from slug.
        assert_eq!(
            events[0].session_id.as_deref(),
            Some(path.file_stem().unwrap().to_str().unwrap())
        );
        assert_eq!(events[0].project.as_deref(), Some("myapp"));
        assert_eq!(events[0].cache_read_tokens, 2);
        assert_eq!(events[0].cache_write_tokens, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resumed_scan_keeps_header_attribution() {
        let dir = fixture("resume");
        let path = write_session(
            &dir,
            "home-u-work-myapp",
            "sess-3",
            &[
                r#"{"type":"session","version":3,"id":"sess-3","timestamp":"2026-09-02T10:00:00.000Z","cwd":"/home/u/work/myapp"}"#,
                r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-09-02T10:01:00.000Z","message":{"role":"assistant","content":[]},"usage":{"inputTokens":100,"outputTokens":20},"model":"openai/gpt-5.1-codex"}"#,
            ],
        );
        let ctx = file_context(&path);
        let offset = util::resume_offset(None, &path);
        let (first, next) = read_jsonl_events(&path, offset, |r| record_events(r, &ctx, 0.0));
        assert_eq!(first.len(), 1);

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-09-02T10:02:00.000Z","message":{{"role":"assistant","content":[]}},"usage":{{"inputTokens":7,"outputTokens":3}},"model":"openai/gpt-5.1-codex"}}"#
        )
        .unwrap();
        drop(file);

        let ctx = file_context(&path);
        let (more, _) = read_jsonl_events(&path, next, |r| record_events(r, &ctx, 0.0));
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].input_tokens, 7);
        assert_eq!(more[0].session_id.as_deref(), Some("sess-3"));
        assert_eq!(more[0].project.as_deref(), Some("myapp"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_roots_are_empty() {
        let dir = fixture("empty");
        assert!(scan_root(&dir, None).is_empty());
        assert!(scan_root(&dir.join("nope"), None).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
