//! Windsurf / Codeium local session & vscdb scanner.
//!
//! Reads `~/.codeium/windsurf` and `<config_dir>/Windsurf` (`*.vscdb` +
//! JSON/JSONL records carrying real token usage fields).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::cursor;
use super::util::{
    self, collect_files, file_mtime, model_on_record, read_jsonl_events, resume_offset,
    session_on_record, ts_on_record, usage_on_record,
};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "windsurf";

pub fn scan() -> Vec<TokenEvent> {
    scan_with_store(TokenStore::open().ok().as_ref())
}

pub(crate) fn scan_with_store(store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for root in scan_roots() {
        if !root.is_dir() {
            continue;
        }
        for db in collect_files(std::slice::from_ref(&root), &["vscdb", "vscdb.backup"]) {
            if util::whole_file_changed(store, &db) {
                events.extend(cursor::scan_db_as(PROVIDER, &db));
                if let Ok(meta) = fs::metadata(&db) {
                    util::mark_scanned(store, &db, meta.len());
                }
            }
        }
        for file in collect_files(std::slice::from_ref(&root), &["jsonl", "ndjson"]) {
            let offset = resume_offset(store, &file);
            let fallback_ts = file_mtime(&file);
            let (mut found, next) =
                read_jsonl_events(&file, offset, |v| record_event(v, fallback_ts));
            events.append(&mut found);
            util::mark_scanned(store, &file, next);
        }
        for file in collect_files(std::slice::from_ref(&root), &["json"]) {
            if !util::whole_file_changed(store, &file) {
                continue;
            }
            events.extend(json_file_events(&file));
            if let Ok(meta) = fs::metadata(&file) {
                util::mark_scanned(store, &file, meta.len());
            }
        }
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    if let Ok(custom) = std::env::var("WINDSURF_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return vec![PathBuf::from(trimmed)];
        }
    }
    let home = util::home();
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    let mut roots = vec![home.join(".codeium").join("windsurf"), cfg.join("Windsurf")];
    roots.sort();
    roots.dedup();
    roots
}

fn record_event(record: &Value, fallback_ts: f64) -> Vec<TokenEvent> {
    let Some(counts) = usage_on_record(record) else {
        return Vec::new();
    };
    vec![TokenEvent {
        provider: PROVIDER.into(),
        model: model_on_record(record),
        session_id: session_on_record(record),
        project: None,
        ts_unix: ts_on_record(record).unwrap_or(fallback_ts),
        input_tokens: counts.input,
        output_tokens: counts.output,
        cache_read_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
    }]
}

fn json_file_events(path: &Path) -> Vec<TokenEvent> {
    let Some(value) = util::read_json(path) else {
        return Vec::new();
    };
    let fallback_ts = file_mtime(path);
    let candidates: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        _ => value
            .get("messages")
            .or_else(|| value.get("history"))
            .or_else(|| value.get("records"))
            .and_then(|v| v.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_else(|| vec![&value]),
    };
    candidates
        .into_iter()
        .flat_map(|v| record_event(v, fallback_ts))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn windsurf_jsonl_usage_emits_events() {
        let dir = std::env::temp_dir().join(format!("usg-windsurf-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cascade.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"model":"claude-sonnet-4-5","sessionId":"ws-1","timestamp":"2026-10-01T12:00:00Z","usage":{{"input_tokens":50,"output_tokens":20,"cache_read_input_tokens":10}}}}"#
        )
        .unwrap();
        drop(f);
        let (events, _) = read_jsonl_events(&file, 0, |v| record_event(v, 0.0));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "windsurf");
        assert_eq!(events[0].input_tokens, 50);
        assert_eq!(events[0].output_tokens, 20);
        assert_eq!(events[0].cache_read_tokens, 10);
        let _ = fs::remove_dir_all(&dir);
    }
}
