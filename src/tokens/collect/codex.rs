//! Codex CLI rollouts: `~/.codex/sessions/**/*.jsonl` (`CODEX_HOME` overrides
//! `~/.codex`). `~/.codex/history.jsonl` supplies session/cwd context when the
//! rollout lacks its own `session_meta`.
//!
//! Usage-bearing records vary across versions, so this parser walks
//! `serde_json::Value` tolerantly:
//!   · `type:"event_msg"` + `payload.type:"token_count"` — uses
//!     `info.last_token_usage`; when only `info.total_token_usage` exists the
//!     event is the delta against the previous total in the same file.
//!   · Responses-API payloads — `payload.response.usage`, `payload.usage`,
//!     or a top-level `usage` object.
//! `session_meta` / `turn_context` lines provide session id, cwd, and model.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::tokens::TokenEvent;
use crate::tokens::collect::{ScanOffsets, appended_lines, jsonl_files, parse_rfc3339, project_name};

type UsageTuple = (u64, u64, u64, u64); // input, output, cache_read, cache_write

fn codex_root() -> PathBuf {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
}

/// session_id → cwd, gleaned from `~/.codex/history.jsonl` when it carries
/// location fields. Tolerant: entries without them just don't map.
fn load_history(path: &std::path::Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session) = v
            .get("session_id")
            .or_else(|| v.get("sessionId"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(cwd) = v
            .get("cwd")
            .or_else(|| v.get("workdir"))
            .and_then(Value::as_str)
        {
            map.insert(session.to_string(), cwd.to_string());
        }
    }
    map
}

/// Per-file parsing context carried across lines.
struct Ctx {
    session: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    last_ts: Option<f64>,
    prev_total: Option<UsageTuple>,
}

/// Scan Codex rollout files; returns newly appended events.
pub fn collect(offsets: &mut ScanOffsets) -> Vec<TokenEvent> {
    let root = codex_root();
    let history = load_history(&root.join("history.jsonl"));
    let mut events = Vec::new();
    for file in jsonl_files(&root.join("sessions")) {
        let fallback_session = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());
        let mut ctx = Ctx {
            session: None,
            cwd: None,
            model: None,
            last_ts: None,
            prev_total: None,
        };
        for line in appended_lines(&file, offsets) {
            collect_line(&line, &mut ctx, &history, fallback_session.as_deref(), &mut events);
        }
    }
    events
}

fn collect_line(
    line: &str,
    ctx: &mut Ctx,
    history: &HashMap<String, String>,
    fallback_session: Option<&str>,
    events: &mut Vec<TokenEvent>,
) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let line_ts = line_timestamp(&v, &payload);
    if line_ts.is_some() {
        ctx.last_ts = line_ts;
    }
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "session_meta" => {
            set_str(&mut ctx.session, payload.get("id"));
            set_str(&mut ctx.cwd, payload.get("cwd"));
            set_str(&mut ctx.model, payload.get("model"));
        }
        "turn_context" => {
            set_str(&mut ctx.cwd, payload.get("cwd"));
            set_str(&mut ctx.model, payload.get("model"));
        }
        _ => {}
    }
    let Some(usage) = find_usage(&v, &payload, ctx) else {
        return;
    };
    let session = ctx
        .session
        .clone()
        .or_else(|| fallback_session.map(str::to_string));
    let cwd = ctx
        .cwd
        .clone()
        .or_else(|| session.as_ref().and_then(|s| history.get(s).cloned()));
    events.push(TokenEvent {
        provider: "codex".into(),
        model: ctx.model.clone(),
        session_id: session,
        project: cwd.as_deref().and_then(|c| project_name(c, "")),
        ts_unix: line_ts.or(ctx.last_ts).unwrap_or(0.0),
        input_tokens: usage.0,
        output_tokens: usage.1,
        cache_read_tokens: usage.2,
        cache_write_tokens: usage.3,
    });
}

fn set_str(slot: &mut Option<String>, value: Option<&Value>) {
    if let Some(s) = value.and_then(Value::as_str).filter(|s| !s.is_empty()) {
        *slot = Some(s.to_string());
    }
}

fn line_timestamp(v: &Value, payload: &Value) -> Option<f64> {
    for t in [v.get("timestamp"), payload.get("timestamp")]
        .into_iter()
        .flatten()
    {
        if let Some(s) = t.as_str()
            && let Some(ts) = parse_rfc3339(s)
        {
            return Some(ts);
        }
        if let Some(n) = t.as_f64() {
            // ms timestamps are common; normalize to seconds
            return Some(if n > 1e12 { n / 1000.0 } else { n });
        }
    }
    None
}

/// First present usage object among the known record shapes.
fn find_usage(v: &Value, payload: &Value, ctx: &mut Ctx) -> Option<UsageTuple> {
    if payload.get("type").and_then(Value::as_str) == Some("token_count") {
        let info = payload.get("info")?;
        if let Some(u) = info.get("last_token_usage").and_then(usage_tuple) {
            ctx.prev_total = info.get("total_token_usage").and_then(usage_tuple).or(ctx.prev_total);
            return Some(u);
        }
        if let Some(total) = info.get("total_token_usage").and_then(usage_tuple) {
            let delta = match ctx.prev_total {
                Some(prev) => tuple_delta(total, prev),
                None => total,
            };
            ctx.prev_total = Some(total);
            return Some(delta);
        }
        return None;
    }
    for cand in [
        payload.pointer("/response/usage"),
        payload.get("usage"),
        v.get("usage"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(u) = usage_tuple(cand) {
            return Some(u);
        }
    }
    None
}

fn tuple_delta(total: UsageTuple, prev: UsageTuple) -> UsageTuple {
    (
        total.0.saturating_sub(prev.0),
        total.1.saturating_sub(prev.1),
        total.2.saturating_sub(prev.2),
        total.3.saturating_sub(prev.3),
    )
}

/// Tolerant usage reader. Codex `token_count` usage splits
/// `reasoning_output_tokens` out of `output_tokens` (folded back in here);
/// Responses-API `output_tokens` already includes reasoning.
fn usage_tuple(u: &Value) -> Option<UsageTuple> {
    if !u.is_object() {
        return None;
    }
    let num = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| u.get(*k).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    let input = num(&["input_tokens", "inputTokens", "prompt_tokens"]);
    let mut output = num(&["output_tokens", "outputTokens", "completion_tokens"]);
    output += num(&["reasoning_output_tokens"]);
    let cache_read = num(&[
        "cache_read_input_tokens",
        "cached_input_tokens",
        "cacheReadTokens",
        "cachedTokens",
    ]) + u.pointer("/input_tokens_details/cached_tokens")
        .or_else(|| u.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = num(&[
        "cache_creation_input_tokens",
        "cache_write_tokens",
        "cacheWriteTokens",
    ]);
    if input + output + cache_read + cache_write == 0 {
        return None;
    }
    Some((input, output, cache_read, cache_write))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fixture() -> &'static str {
        include_str!("../fixtures/codex_rollout.jsonl")
    }

    #[test]
    fn token_count_uses_last_token_usage() {
        let mut ctx = Ctx {
            session: Some("s1".into()),
            cwd: Some("/work/proj".into()),
            model: Some("gpt-5-codex".into()),
            last_ts: None,
            prev_total: None,
        };
        let line = r#"{"timestamp":"2026-09-30T10:00:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":5,"reasoning_output_tokens":2,"total_tokens":21},"total_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":5,"reasoning_output_tokens":2,"total_tokens":21},"model_context_window":200000}}}"#;
        let mut events = Vec::new();
        collect_line(line, &mut ctx, &HashMap::new(), None, &mut events);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.provider, "codex");
        assert_eq!(e.input_tokens, 10);
        assert_eq!(e.output_tokens, 7); // 5 output + 2 reasoning
        assert_eq!(e.cache_read_tokens, 4);
        assert_eq!(e.cache_write_tokens, 0);
        assert_eq!(e.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(e.project.as_deref(), Some("proj"));
    }

    #[test]
    fn totals_only_lines_emit_deltas() {
        let mut ctx = Ctx {
            session: None,
            cwd: None,
            model: None,
            last_ts: None,
            prev_total: None,
        };
        let mut events = Vec::new();
        let l1 = r#"{"timestamp":"2026-09-30T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10,"cached_input_tokens":0,"reasoning_output_tokens":0,"total_tokens":110}}}}"#;
        let l2 = r#"{"timestamp":"2026-09-30T10:05:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"output_tokens":25,"cached_input_tokens":3,"reasoning_output_tokens":0,"total_tokens":188}}}}"#;
        collect_line(l1, &mut ctx, &HashMap::new(), None, &mut events);
        collect_line(l2, &mut ctx, &HashMap::new(), None, &mut events);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[1].input_tokens, 60);
        assert_eq!(events[1].output_tokens, 15);
        assert_eq!(events[1].cache_read_tokens, 3);
    }

    #[test]
    fn response_completed_usage_shape() {
        let mut ctx = Ctx {
            session: None,
            cwd: None,
            model: None,
            last_ts: None,
            prev_total: None,
        };
        let line = r#"{"timestamp":"2026-09-30T10:00:00Z","type":"response_item","payload":{"type":"response.completed","response":{"usage":{"input_tokens":50,"output_tokens":20,"total_tokens":70,"input_tokens_details":{"cached_tokens":8}}}}}"#;
        let mut events = Vec::new();
        collect_line(line, &mut ctx, &HashMap::new(), None, &mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 50);
        assert_eq!(events[0].output_tokens, 20);
        assert_eq!(events[0].cache_read_tokens, 8);
    }

    #[test]
    fn collect_reads_sessions_tree_incrementally() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("usg-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("sessions").join("2026").join("09").join("30");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rollout-2026-09-30T10-00-00-abc123.jsonl");
        std::fs::write(&file, fixture()).unwrap();
        std::fs::write(
            root.join("history.jsonl"),
            r#"{"session_id":"sess-codex-1","ts":1790726400,"text":"hi","cwd":"/work/histproj"}"#,
        )
        .unwrap();
        unsafe {
            std::env::set_var("CODEX_HOME", &root);
        }

        let mut offsets = ScanOffsets::detached();
        let events = collect(&mut offsets);
        assert_eq!(events.len(), 2, "fixture has two usage records");
        assert!(events.iter().all(|e| e.provider == "codex"));
        assert_eq!(events[0].session_id.as_deref(), Some("sess-codex-1"));
        assert_eq!(events[0].project.as_deref(), Some("myproj"));
        assert!(collect(&mut offsets).is_empty());

        unsafe {
            std::env::remove_var("CODEX_HOME");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
