//! Block Goose local session JSONL scanner (`~/.local/share/goose/sessions`).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util::{
    self, collect_files, file_mtime, model_on_record, read_jsonl_events, resume_offset,
    session_on_record, ts_on_record, usage_counts, usage_on_record,
};
use crate::tokens::collect::project_name;
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "goose";

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
            let session_fallback = file
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string);
            let (mut found, next) = read_jsonl_events(&file, offset, |v| {
                parse_record(v, fallback_ts, session_fallback.as_deref(), &file)
            });
            events.append(&mut found);
            util::mark_scanned(store, &file, next);
        }
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    if let Ok(custom) = std::env::var("GOOSE_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return vec![PathBuf::from(trimmed).join("sessions")];
        }
    }
    let home = util::home();
    let data = dirs::data_local_dir().unwrap_or_else(|| home.join(".local/share"));
    let mut roots = vec![
        data.join("goose").join("sessions"),
        home.join(".local/share/goose/sessions"),
    ];
    roots.sort();
    roots.dedup();
    roots
}

fn parse_record(
    record: &Value,
    fallback_ts: f64,
    session_fallback: Option<&str>,
    _path: &Path,
) -> Vec<TokenEvent> {
    let counts = usage_on_record(record).or_else(|| {
        record
            .get("metadata")
            .and_then(|m| usage_on_record(m).or_else(|| usage_counts(m)))
    });
    let Some(counts) = counts else {
        return Vec::new();
    };
    let cwd = record
        .get("working_dir")
        .or_else(|| record.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or("");
    vec![TokenEvent {
        provider: PROVIDER.into(),
        model: model_on_record(record),
        session_id: session_on_record(record).or_else(|| session_fallback.map(str::to_string)),
        project: project_name(cwd, ""),
        ts_unix: ts_on_record(record).unwrap_or(fallback_ts),
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
    fn parses_goose_session_jsonl() {
        let dir = std::env::temp_dir().join(format!("usg-goose-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sess-99.jsonl");
        let mut f = fs::File::create(&file).unwrap();
        writeln!(
            f,
            r#"{{"model":"claude-sonnet-4-5","working_dir":"/work/goose-proj","timestamp":"2026-10-01T12:00:00Z","usage":{{"input_tokens":80,"output_tokens":25,"cache_read_input_tokens":15}}}}"#
        )
        .unwrap();
        drop(f);
        let (events, _) =
            read_jsonl_events(&file, 0, |v| parse_record(v, 0.0, Some("sess-99"), &file));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider, "goose");
        assert_eq!(events[0].project.as_deref(), Some("goose-proj"));
        assert_eq!(events[0].session_id.as_deref(), Some("sess-99"));
        assert_eq!(events[0].input_tokens, 80);
        let _ = fs::remove_dir_all(&dir);
    }
}
