//! Gemini CLI session scanner (`~/.gemini/tmp/<project_hash>/chats/`).
//!
//! Two real on-disk formats (matching gemini-cli's ChatRecordingService):
//! - Legacy `*.json` (including UUID-named files): one object with
//!   `{sessionId, projectHash, startTime, messages: [...]}` where each
//!   assistant message may carry `tokens` + `model`.
//! - Current `*.jsonl`: a metadata record (`sessionId`, `projectHash`,
//!   `startTime`, `kind`, `directories`) followed by one record per message;
//!   `type: "gemini"` records carry `tokens` and `model`, `init` records set
//!   the session model.
//!
//! `tokens` accepts every spelling gemini-cli has shipped:
//! `{input|prompt|input_tokens|prompt_tokens|promptTokenCount}`,
//! `{output|candidates|output_tokens|completion_tokens|candidatesTokenCount}`,
//! `{cached|cached_tokens|cachedContentTokenCount}`,
//! `{thoughts|reasoning|thoughts_tokens|thoughtsTokenCount}`,
//! `{tool|tool_tokens|toolUsePromptTokenCount}`, `{total|totalTokenCount}`.
//! `tool` counts toward input, `thoughts` toward output — both real token
//! usage that would otherwise be dropped.
//!
//! Provider id is `gemini`: the ledger keeps file-provenance provider names,
//! while the quota surface reports Google usage under `antigravity`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util::{
    self, UsageCounts, collect_files, file_mtime, read_jsonl_events, resume_offset, ts_on_record,
};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "gemini";

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for root in scan_roots() {
        let tmp = root.join("tmp");
        if !tmp.is_dir() {
            continue;
        }
        for file in collect_files(&[tmp], &["json", "jsonl"]) {
            if !in_chats_dir(&file) {
                continue;
            }
            match file.extension().and_then(|e| e.to_str()) {
                Some("jsonl") => {
                    let offset = resume_offset(store.as_ref(), &file);
                    let (mut found, next) = parse_jsonl_records(&file, offset);
                    events.append(&mut found);
                    util::mark_scanned(store.as_ref(), &file, next);
                }
                _ => {
                    if !util::whole_file_changed(store.as_ref(), &file) {
                        continue;
                    }
                    events.extend(parse_session_file(&file));
                    if let Ok(meta) = fs::metadata(&file) {
                        util::mark_scanned(store.as_ref(), &file, meta.len());
                    }
                }
            }
        }
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    vec![util::home().join(".gemini")]
}

/// Only `tmp/<hash>/chats/<file>` paths are session recordings.
fn in_chats_dir(path: &Path) -> bool {
    let comps: Vec<&std::ffi::OsStr> = path.components().map(|c| c.as_os_str()).collect();
    for i in 0..comps.len().saturating_sub(3) {
        if comps[i] == "tmp" && comps[i + 2] == "chats" {
            return true;
        }
    }
    false
}

fn gemini_tokens(v: &Value) -> Option<UsageCounts> {
    let t = v.get("tokens")?;
    let num = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| {
                t.get(*k)
                    .and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f.max(0.0) as u64)))
            })
            .unwrap_or(0)
    };
    let input = num(&[
        "input",
        "prompt",
        "input_tokens",
        "inputTokens",
        "prompt_tokens",
        "promptTokens",
        "promptTokenCount",
    ]);
    let output = num(&[
        "output",
        "candidates",
        "output_tokens",
        "outputTokens",
        "completion_tokens",
        "completionTokens",
        "candidatesTokenCount",
    ]);
    let cached = num(&[
        "cached",
        "cached_tokens",
        "cachedTokens",
        "cachedContentTokenCount",
    ]);
    let thoughts = num(&[
        "thoughts",
        "reasoning",
        "thoughts_tokens",
        "thoughtsTokenCount",
    ]);
    let tool = num(&["tool", "tool_tokens", "toolUsePromptTokenCount"]);
    let counts = UsageCounts {
        input: input.saturating_add(tool),
        output: output.saturating_add(thoughts),
        cache_read: cached,
        cache_write: 0,
    };
    (!counts.is_empty()).then_some(counts)
}

fn session_id_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn parse_session_file(path: &Path) -> Vec<TokenEvent> {
    let Some(value) = util::read_json(path) else {
        return Vec::new();
    };
    let fallback_ts = file_mtime(path);
    let session_id = value
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| session_id_of(path));
    let project = value
        .get("projectHash")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let Some(messages) = value.get("messages").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for msg in messages {
        let Some(counts) = gemini_tokens(msg) else {
            continue;
        };
        events.push(TokenEvent {
            provider: PROVIDER.into(),
            model: msg
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            session_id: Some(session_id.clone()),
            project: project.clone(),
            ts_unix: ts_on_record(msg).unwrap_or(fallback_ts),
            input_tokens: counts.input,
            output_tokens: counts.output,
            cache_read_tokens: counts.cache_read,
            cache_write_tokens: counts.cache_write,
        });
    }
    events
}

/// Parse a `session-*.jsonl` chat recording: metadata header, then records
/// where `type: "gemini"` (or any record carrying `tokens`) maps to an event.
/// Resuming past the header still recovers sessionId/projectHash by peeking
/// at the first line.
fn parse_jsonl_records(path: &Path, offset: u64) -> (Vec<TokenEvent>, u64) {
    use std::cell::RefCell;
    let fallback_ts = file_mtime(path);
    let state = RefCell::new(JsonlState {
        session_id: session_id_of(path),
        project: None,
        model: None,
    });
    if offset > 0
        && let Some(header) = first_jsonl_line(path)
    {
        let mut st = state.borrow_mut();
        let _ = st.apply_record(&header, path, fallback_ts);
    }
    let (events, next) = read_jsonl_events(path, offset, |record| {
        let mut st = state.borrow_mut();
        st.apply_record(record, path, fallback_ts)
    });
    (events, next)
}

fn first_jsonl_line(path: &Path) -> Option<Value> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

struct JsonlState {
    session_id: String,
    project: Option<String>,
    model: Option<String>,
}

impl JsonlState {
    fn apply_record(&mut self, record: &Value, _path: &Path, fallback_ts: f64) -> Vec<TokenEvent> {
        // Header/metadata line carries sessionId + projectHash.
        if let Some(id) = record.get("sessionId").and_then(|v| v.as_str()) {
            self.session_id = id.to_string();
        }
        if self.project.is_none()
            && let Some(ph) = record.get("projectHash").and_then(|v| v.as_str())
        {
            self.project = Some(ph.to_string());
        }
        if let Some(m) = record.get("model").and_then(|v| v.as_str()) {
            self.model = Some(m.to_string());
        }
        let is_usage = record.get("type").and_then(|v| v.as_str()) == Some("gemini")
            || record.get("tokens").is_some();
        if !is_usage {
            return Vec::new();
        }
        let Some(counts) = gemini_tokens(record) else {
            return Vec::new();
        };
        vec![TokenEvent {
            provider: PROVIDER.into(),
            model: record
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| self.model.clone()),
            session_id: Some(self.session_id.clone()),
            project: self.project.clone(),
            ts_unix: ts_on_record(record).unwrap_or(fallback_ts),
            input_tokens: counts.input,
            output_tokens: counts.output,
            cache_read_tokens: counts.cache_read,
            cache_write_tokens: counts.cache_write,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-gemini-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("tmp/abc123/chats")).unwrap();
        dir.join("tmp/abc123/chats")
    }

    #[test]
    fn parses_legacy_json_session() {
        let chats = fixture("legacy");
        let file = chats.join("session-2026-09-01T10-00-deadbeef.json");
        fs::write(
            &file,
            serde_json::json!({
                "sessionId": "sess-abc",
                "projectHash": "abc123",
                "startTime": "2026-09-01T10:00:00.000Z",
                "messages": [
                    {"id": "m1", "type": "user", "content": "hi", "timestamp": "2026-09-01T10:00:01.000Z"},
                    {"id": "m2", "type": "gemini", "timestamp": "2026-09-01T10:00:05.000Z",
                     "model": "gemini-2.5-pro",
                     "tokens": {"input": 1000, "output": 200, "cached": 300, "thoughts": 50, "tool": 25, "total": 1575}},
                ],
            })
            .to_string(),
        )
        .unwrap();
        let events = parse_session_file(&file);
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.provider, "gemini");
        assert_eq!(ev.input_tokens, 1025); // input + tool
        assert_eq!(ev.output_tokens, 250); // output + thoughts
        assert_eq!(ev.cache_read_tokens, 300);
        assert_eq!(ev.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(ev.project.as_deref(), Some("abc123"));
        let _ = fs::remove_dir_all(chats.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn parses_jsonl_recording_incrementally() {
        let chats = fixture("jsonl");
        let file = chats.join("session-2026-09-01T10-00-deadbeef.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        writeln!(f, r#"{{"sessionId":"jl-1","projectHash":"h9","startTime":"2026-09-01T10:00:00.000Z","kind":"main"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-09-01T10:00:01.000Z","content":"hi"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"gemini","timestamp":"2026-09-01T10:00:04.000Z","model":"gemini-2.5-flash","tokens":{{"input":10,"output":5,"cached":2}}}}"#).unwrap();
        drop(f);
        let (events, next) = parse_jsonl_records(&file, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 10);
        assert_eq!(events[0].session_id.as_deref(), Some("jl-1"));
        // Appending a line resumes from the stored offset.
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(f, r#"{{"type":"gemini","timestamp":"2026-09-01T10:00:09.000Z","tokens":{{"inputTokens":7,"outputTokens":3}}}}"#).unwrap();
        drop(f);
        let (more, _) = parse_jsonl_records(&file, next);
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].input_tokens, 7);
        assert_eq!(more[0].output_tokens, 3);
        assert_eq!(more[0].session_id.as_deref(), Some("jl-1"));
        let _ = fs::remove_dir_all(chats.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn messages_without_tokens_emit_nothing() {
        let chats = fixture("none");
        let file = chats.join("uuid-like-name.json");
        fs::write(
            &file,
            serde_json::json!({
                "sessionId": "s",
                "projectHash": "h",
                "startTime": "2026-09-01T10:00:00.000Z",
                "messages": [{"id": "m1", "timestamp": "2026-09-01T10:00:01.000Z", "type": "user", "content": "hi"}],
            })
            .to_string(),
        )
        .unwrap();
        assert!(parse_session_file(&file).is_empty());
        let _ = fs::remove_dir_all(chats.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn chats_dir_filter() {
        assert!(in_chats_dir(Path::new(
            "/home/u/.gemini/tmp/h/chats/a.json"
        )));
        assert!(!in_chats_dir(Path::new("/home/u/.gemini/tmp/h/a.json")));
        assert!(!in_chats_dir(Path::new("/home/u/.gemini/credentials.json")));
    }
}
