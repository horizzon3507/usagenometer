//! Grok (Grok Build) session scanner.
//!
//! What the CLI persists locally (verified against tokscale's grok adapter):
//! - `~/.grok/sessions/<workspace>/<session>/updates.jsonl` — JSON-RPC session
//!   updates. `params.update.usage` carries either a split usage object
//!   (`inputTokens`/`outputTokens`/`cachedReadTokens`/`cachedWriteTokens`/
//!   `reasoningTokens`/`totalTokens`) or only a cumulative `totalTokens`
//!   counter; the latter is converted to positive per-turn input deltas.
//! - `signals.json` (same dir) — end-of-session rollup with
//!   `totalTokensBeforeCompaction` / `contextTokensUsed` / `totalTokens` /
//!   `primaryModelId`. When its effective total exceeds the sum of parsed
//!   update deltas, one reconciliation event covers the compaction gap.
//! - `~/.grok/logs/*.jsonl` — newer per-inference breakdowns; read through the
//!   generic usage extractor.
//!
//! `auth.json` holds OAuth material only — it is never parsed for usage.
//! Without these session files nothing is emitted: we never invent numbers.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::util::{
    self, collect_files, file_mtime, get_path, model_on_record, percent_decode, read_jsonl_events,
    session_on_record, ts_on_record, usage_counts, usage_on_record,
};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "grok";

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for root in scan_roots() {
        if !root.is_dir() {
            continue;
        }
        events.extend(scan_sessions(&root.join("sessions"), store.as_ref()));
        events.extend(scan_logs(&root.join("logs"), store.as_ref()));
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(custom) = std::env::var("GROK_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            roots.push(PathBuf::from(trimmed));
        }
    }
    let home = util::home();
    roots.push(home.join(".grok"));
    roots.push(
        dirs::config_dir()
            .unwrap_or_else(|| home.join(".config"))
            .join("grok"),
    );
    roots.sort();
    roots.dedup();
    roots
}

/// `sessions/…/updates.jsonl` + sibling `signals.json`.
fn scan_sessions(root: &PathBuf, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for updates in collect_files(std::slice::from_ref(root), &["jsonl"]) {
        if updates.file_name().and_then(|n| n.to_str()) != Some("updates.jsonl") {
            continue;
        }
        let Some(session_dir) = updates.parent().map(|p| p.to_path_buf()) else {
            continue;
        };
        // A finished session is unchanged only when both files are; skip early.
        let signals = session_dir.join("signals.json");
        let updates_changed = util::whole_file_changed(store, &updates);
        let signals_changed = !signals.exists() || util::whole_file_changed(store, &signals);
        if !updates_changed && !signals_changed {
            continue;
        }
        let (mut session_events, updates_total) = parse_updates(&updates);
        reconcile_signals(&signals, updates_total, &mut session_events);
        events.extend(session_events);
        if let Ok(meta) = fs::metadata(&updates) {
            util::mark_scanned(store, &updates, meta.len());
        }
        if signals.exists() {
            if let Ok(meta) = fs::metadata(&signals) {
                util::mark_scanned(store, &signals, meta.len());
            }
        }
    }
    events
}

/// `logs/*.jsonl` — append-only inference logs; incremental by byte offset.
fn scan_logs(root: &PathBuf, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for path in collect_files(std::slice::from_ref(root), &["jsonl"]) {
        let offset = util::resume_offset(store, &path);
        let fallback_ts = file_mtime(&path);
        let (mut found, next) = read_jsonl_events(&path, offset, |record| {
            log_event(record, &path, fallback_ts)
        });
        events.append(&mut found);
        util::mark_scanned(store, &path, next);
    }
    events
}

fn log_event(record: &Value, _path: &PathBuf, fallback_ts: f64) -> Vec<TokenEvent> {
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

/// Parse one `updates.jsonl`. Returns events + the sum of all emitted buckets
/// (used for signals reconciliation).
fn parse_updates(path: &PathBuf) -> (Vec<TokenEvent>, u64) {
    let fallback_ts = file_mtime(path);
    let session_dir = path.parent().map(|p| p.to_path_buf());
    let session_id = session_dir
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string);
    let project = session_dir
        .as_ref()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|ws| percent_decode(ws));
    let meta_model = session_metadata_model(session_dir.as_deref());
    let meta_ts = session_metadata_ts(session_dir.as_deref()).unwrap_or(fallback_ts);

    let mut events = Vec::new();
    let mut total = 0u64;
    let mut last_total: Option<u64> = None;
    let mut model = meta_model;

    let Ok(bytes) = fs::read(path) else {
        return (events, total);
    };
    for raw_line in bytes.split(|b| *b == b'\n') {
        let text = String::from_utf8_lossy(raw_line);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(m) = model_on_record(&value) {
            model = Some(m);
        }
        let ts = ts_on_record(&value).unwrap_or(meta_ts);
        let usage = get_path(&value, &["params", "update", "usage"])
            .or_else(|| get_path(&value, &["params", "usage"]))
            .or_else(|| value.get("usage"));
        let Some(usage) = usage else { continue };

        if let Some(counts) = usage_counts(usage) {
            // Split usage object: real per-kind numbers.
            let sum = counts
                .input
                .saturating_add(counts.output)
                .saturating_add(counts.cache_read)
                .saturating_add(counts.cache_write);
            total = total.saturating_add(sum);
            if let Some(cum) = cumulative_total(usage) {
                last_total = Some(cum);
            }
            events.push(mk_event(
                model.clone(),
                session_id.clone(),
                project.clone(),
                ts,
                counts,
            ));
            continue;
        }

        // Legacy shape: only a cumulative `totalTokens` counter → deltas.
        if let Some(cum) = cumulative_total(usage) {
            let delta = match last_total {
                Some(prev) if cum > prev => cum - prev,
                Some(_) => 0,
                None => {
                    // First observed counter already reflects earlier turns;
                    // count it once as the session's initial turn.
                    cum
                }
            };
            last_total = Some(cum);
            if delta > 0 {
                total = total.saturating_add(delta);
                events.push(mk_event(
                    model.clone(),
                    session_id.clone(),
                    project.clone(),
                    ts,
                    // No input/output split exists in this format: deltas are
                    // recorded as input, matching other grok consumers.
                    super::util::UsageCounts {
                        input: delta,
                        output: 0,
                        cache_read: 0,
                        cache_write: 0,
                    },
                ));
            }
        }
    }
    (events, total)
}

fn cumulative_total(usage: &Value) -> Option<u64> {
    usage
        .get("totalTokens")
        .or_else(|| usage.get("total_tokens"))
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
}

/// `signals.json` rollup: `totalTokensBeforeCompaction`, `contextTokensUsed`,
/// `totalTokens`, `primaryModelId` / `modelsUsed`.
fn reconcile_signals(path: &PathBuf, updates_total: u64, events: &mut Vec<TokenEvent>) {
    let Some(value) = util::read_json(path) else {
        return;
    };
    let effective = [
        "totalTokensBeforeCompaction",
        "contextTokensUsed",
        "totalTokens",
    ]
    .iter()
    .filter_map(|k| {
        value
            .get(*k)
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
    })
    .max();
    let Some(effective) = effective else {
        return;
    };
    if effective <= updates_total {
        return;
    }
    let session_dir = path.parent();
    events.push(TokenEvent {
        provider: PROVIDER.into(),
        model: model_on_record(&value),
        session_id: session_dir
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string),
        project: session_dir
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(percent_decode),
        ts_unix: file_mtime(path),
        input_tokens: effective - updates_total,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    });
}

/// Model fallback from `summary.json` / `signals.json` in the session dir.
fn session_metadata_model(dir: Option<&std::path::Path>) -> Option<String> {
    let dir = dir?;
    for name in ["summary.json", "signals.json"] {
        if let Some(v) = util::read_json(&dir.join(name))
            && let Some(m) = model_on_record(&v)
        {
            return Some(m);
        }
    }
    None
}

fn session_metadata_ts(dir: Option<&std::path::Path>) -> Option<f64> {
    let dir = dir?;
    if let Some(v) = util::read_json(&dir.join("summary.json"))
        && let Some(ts) = ts_on_record(&v)
    {
        return Some(ts);
    }
    // events.jsonl is line-delimited — peek at the first record only.
    let path = dir.join("events.jsonl");
    let bytes = fs::read(&path).ok()?;
    let first = bytes.split(|b| *b == b'\n').next()?;
    let text = String::from_utf8_lossy(first);
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    ts_on_record(&v)
}

fn mk_event(
    model: Option<String>,
    session_id: Option<String>,
    project: Option<String>,
    ts: f64,
    counts: util::UsageCounts,
) -> TokenEvent {
    TokenEvent {
        provider: PROVIDER.into(),
        model,
        session_id,
        project,
        ts_unix: ts,
        input_tokens: counts.input,
        output_tokens: counts.output,
        cache_read_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-grok-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sessions/%2Fwork%2Fproj/sess-1")).unwrap();
        dir
    }

    #[test]
    fn parses_split_usage_and_cumulative_deltas() {
        let dir = fixture_dir("split");
        let updates = dir.join("sessions/%2Fwork%2Fproj/sess-1/updates.jsonl");
        let mut f = fs::File::create(&updates).unwrap();
        // Legacy cumulative-only record (first total counts as initial turn).
        writeln!(f, r#"{{"params":{{"update":{{"usage":{{"totalTokens":100}},"timestamp":"2026-09-01T10:00:00Z"}}}}}}"#).unwrap();
        writeln!(f, r#"{{"params":{{"update":{{"usage":{{"totalTokens":250}},"timestamp":"2026-09-01T10:01:00Z"}}}}}}"#).unwrap();
        // Split usage object.
        writeln!(f, r#"{{"modelId":"grok-code-fast-1","params":{{"update":{{"usage":{{"inputTokens":40,"outputTokens":30,"cachedReadTokens":10,"totalTokens":70}},"timestamp":"2026-09-01T10:02:00Z"}}}}}}"#).unwrap();
        writeln!(f, "not json").unwrap();
        let (events, total) = parse_updates(&updates);
        // 100 (initial delta) + 150 (delta) + split record (40-10 in +30 out +10 cr = 70)
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[1].input_tokens, 150);
        assert_eq!(events[2].input_tokens, 30);
        assert_eq!(events[2].output_tokens, 30);
        assert_eq!(events[2].cache_read_tokens, 10);
        assert_eq!(events[2].model.as_deref(), Some("grok-code-fast-1"));
        assert_eq!(events[2].session_id.as_deref(), Some("sess-1"));
        assert_eq!(events[2].project.as_deref(), Some("/work/proj"));
        assert_eq!(total, 100 + 150 + 70);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn signals_reconciles_compaction_gap() {
        let dir = fixture_dir("sig");
        let session = dir.join("sessions/ws/sess-9");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("updates.jsonl"),
            "{\"params\":{\"update\":{\"usage\":{\"totalTokens\":100}}}}\n",
        )
        .unwrap();
        fs::write(
            session.join("signals.json"),
            r#"{"totalTokensBeforeCompaction":900,"contextTokensUsed":200,"totalTokens":150,"primaryModelId":"grok-4"}"#,
        )
        .unwrap();
        let (mut events, total) = parse_updates(&session.join("updates.jsonl"));
        reconcile_signals(&session.join("signals.json"), total, &mut events);
        assert_eq!(events.len(), 2);
        let recon = &events[1];
        assert_eq!(recon.input_tokens, 800); // 900 effective − 100 emitted
        assert_eq!(recon.model.as_deref(), Some("grok-4"));
        assert_eq!(recon.session_id.as_deref(), Some("sess-9"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_token_files_means_no_events() {
        let dir = fixture_dir("empty");
        let session = dir.join("sessions/ws/sess-0");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("updates.jsonl"),
            "{\"params\":{\"update\":{\"text\":\"hi\"}}}\n",
        )
        .unwrap();
        let (events, total) = parse_updates(&session.join("updates.jsonl"));
        assert!(events.is_empty());
        assert_eq!(total, 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
