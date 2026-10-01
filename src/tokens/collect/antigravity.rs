//! Antigravity (Google's agentic IDE) local scanner.
//!
//! Antigravity is a VS Code-fork family app: its state may live under
//! `~/.antigravity/` (also `$XDG_CONFIG_HOME/Antigravity` /
//! `~/Library/Application Support/Antigravity`) as `*.vscdb` stores plus
//! JSON/JSONL session or telemetry files. Coverage is deliberately
//! honest: any `*.vscdb` is probed read-only through the same machinery as
//! the Cursor scanner, and JSON/JSONL files are run through the generic
//! usage extractor (`usage`/`tokenUsage`/`usageMetadata`/`tokenCount`
//! objects with real numbers). When a build does not persist token fields
//! the scanner emits nothing — we never synthesize usage.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::cursor;
use super::util::{
    self, collect_files, file_mtime, model_on_record, read_jsonl_events, resume_offset,
    session_on_record, ts_on_record, usage_on_record,
};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "antigravity";

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for root in scan_roots() {
        if !root.is_dir() {
            continue;
        }
        for db in collect_files(std::slice::from_ref(&root), &["vscdb", "vscdb.backup"]) {
            if util::whole_file_changed(store.as_ref(), &db) {
                events.extend(cursor::scan_db_as(PROVIDER, &db));
                if let Ok(meta) = fs::metadata(&db) {
                    util::mark_scanned(store.as_ref(), &db, meta.len());
                }
            }
        }
        for file in collect_files(std::slice::from_ref(&root), &["jsonl", "ndjson"]) {
            let offset = resume_offset(store.as_ref(), &file);
            let fallback_ts = file_mtime(&file);
            let (mut found, next) =
                read_jsonl_events(&file, offset, |v| record_event(v, fallback_ts));
            events.append(&mut found);
            util::mark_scanned(store.as_ref(), &file, next);
        }
        for file in collect_files(std::slice::from_ref(&root), &["json"]) {
            if !util::whole_file_changed(store.as_ref(), &file) {
                continue;
            }
            events.extend(json_file_events(&file));
            if let Ok(meta) = fs::metadata(&file) {
                util::mark_scanned(store.as_ref(), &file, meta.len());
            }
        }
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    let home = util::home();
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    let mut roots = vec![home.join(".antigravity"), cfg.join("Antigravity")];
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

fn json_file_events(path: &PathBuf) -> Vec<TokenEvent> {
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
    fn jsonl_records_with_usage_emit_events() {
        let dir = std::env::temp_dir().join(format!("usg-ag-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"type":"inference","model":"gemini-3-pro","sessionId":"s1","timestamp":"2026-09-02T12:00:00Z","usage":{{"inputTokens":100,"outputTokens":40,"cachedReadTokens":10,"totalTokens":140}}}}"#
        )
        .unwrap();
        f.write_all(
            br#"{"type":"ping"}
"#,
        )
        .unwrap();
        drop(f);
        let (events, _) = read_jsonl_events(&file, 0, |v| record_event(v, 0.0));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "antigravity");
        assert_eq!(events[0].input_tokens, 90); // 100 − 10 cache subset
        assert_eq!(events[0].output_tokens, 40);
        assert_eq!(events[0].cache_read_tokens, 10);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_without_usage_is_empty() {
        let dir = std::env::temp_dir().join(format!("usg-ag-nousage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");
        fs::write(
            &file,
            r#"{"settings":{"theme":"dark"},"history":[{"text":"hi"}]}"#,
        )
        .unwrap();
        assert!(json_file_events(&file).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
