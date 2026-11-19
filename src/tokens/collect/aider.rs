//! Aider local analytics JSONL scanner (`~/.aider/analytics.jsonl`).

use std::path::PathBuf;

use serde_json::Value;

use super::util::{
    self, collect_files, file_mtime, model_on_record, read_jsonl_events, resume_offset,
    session_on_record, ts_on_record, usage_counts, usage_on_record,
};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "aider";

pub fn scan() -> Vec<TokenEvent> {
    scan_with_store(TokenStore::open().ok().as_ref())
}

pub(crate) fn scan_with_store(store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for root in scan_roots() {
        if !root.is_dir() {
            continue;
        }
        for file in collect_files(std::slice::from_ref(&root), &["jsonl", "ndjson"]) {
            let offset = resume_offset(store, &file);
            let fallback_ts = file_mtime(&file);
            let (mut found, next) =
                read_jsonl_events(&file, offset, |v| parse_record(v, fallback_ts));
            events.append(&mut found);
            util::mark_scanned(store, &file, next);
        }
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    if let Ok(custom) = std::env::var("AIDER_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return vec![PathBuf::from(trimmed)];
        }
    }
    vec![util::home().join(".aider")]
}

fn parse_record(record: &Value, fallback_ts: f64) -> Vec<TokenEvent> {
    let counts = usage_on_record(record)
        .or_else(|| record.get("properties").and_then(usage_counts))
        .or_else(|| usage_counts(record));
    let Some(counts) = counts else {
        return Vec::new();
    };
    let model =
        model_on_record(record).or_else(|| record.get("properties").and_then(model_on_record));
    let session_id =
        session_on_record(record).or_else(|| record.get("properties").and_then(session_on_record));
    let ts = ts_on_record(record).unwrap_or(fallback_ts);
    vec![TokenEvent {
        provider: PROVIDER.into(),
        model,
        session_id,
        project: None,
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
    use std::fs;
    use std::io::Write;

    #[test]
    fn parses_aider_analytics_jsonl() {
        let dir = std::env::temp_dir().join(format!("usg-aider-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("analytics.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"event":"message_send","timestamp":"2026-10-01T12:00:00Z","properties":{{"model":"gpt-4o","prompt_tokens":300,"completion_tokens":120}}}}"#
        )
        .unwrap();
        drop(f);
        let (events, _) = read_jsonl_events(&file, 0, |v| parse_record(v, 0.0));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "aider");
        assert_eq!(events[0].model.as_deref(), Some("gpt-4o"));
        assert_eq!(events[0].input_tokens, 300);
        assert_eq!(events[0].output_tokens, 120);
        let _ = fs::remove_dir_all(&dir);
    }
}
