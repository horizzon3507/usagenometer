//! Claude Code transcripts: `~/.claude/projects/**/*.jsonl`
//! (`CLAUDE_CONFIG_DIR` overrides `~/.claude`).
//!
//! Assistant entries carry `message.model`, `message.usage`
//! ({input_tokens, output_tokens, cache_creation_input_tokens,
//! cache_read_input_tokens}), `timestamp` (RFC3339), `sessionId`, `cwd`,
//! and a per-entry `uuid`. Lines with other `type`s are skipped.

use std::path::PathBuf;

use serde_json::Value;

use crate::tokens::TokenEvent;
use crate::tokens::collect::{ScanOffsets, appended_lines, jsonl_files, parse_rfc3339, project_name};

/// `~/.claude` (or `$CLAUDE_CONFIG_DIR`). Shared with `collect::glm`, whose
/// sessions live in the same tree.
pub(crate) fn claude_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
}

/// Full (non-incremental) scan of the transcripts tree.
pub fn scan() -> Vec<TokenEvent> {
    collect(&mut ScanOffsets::detached())
}

/// Roots this scanner reads (for `usg doctor`).
pub fn scan_roots() -> Vec<PathBuf> {
    vec![claude_root().join("projects")]
}

/// Scan Claude Code transcript files; returns newly appended events.
pub fn collect(offsets: &mut ScanOffsets) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for file in jsonl_files(&claude_root().join("projects")) {
        let slug = file
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for line in appended_lines(&file, offsets) {
            if let Some(event) = parse_line(&line, &slug) {
                events.push(event);
            }
        }
    }
    events
}

/// GLM-family model ids served through z.ai's Claude-compatible endpoint
/// (glm-4.5, glm-4.6, glm-4.5-air, ...). The GLM Coding Plan writes these
/// transcripts into this same tree; they ledger as provider `glm`.
pub(crate) fn is_glm_model(model: &str) -> bool {
    let tail = model.trim().rsplit('/').next().unwrap_or("");
    tail.to_ascii_lowercase().starts_with("glm-")
}

fn parse_line(line: &str, slug: &str) -> Option<TokenEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let event = parse_record(&v, slug)?;
    (!event.model.as_deref().is_some_and(is_glm_model)).then_some(event)
}

/// One Claude-transcript record → TokenEvent (provider `claude`).
/// Shared with `collect::glm`, which keeps GLM-family models instead.
pub(crate) fn parse_record(v: &Value, slug: &str) -> Option<TokenEvent> {
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let usage = v.get("message")?.get("usage")?;
    let num = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = num("input_tokens");
    let output = num("output_tokens");
    let cache_write = num("cache_creation_input_tokens");
    let cache_read = num("cache_read_input_tokens");
    let model = v
        .get("message")
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let session = v
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let cwd = v.get("cwd").and_then(Value::as_str).unwrap_or("");
    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .unwrap_or(0.0);
    Some(TokenEvent {
        provider: "claude".into(),
        model,
        session_id: session,
        project: project_name(cwd, slug),
        ts_unix: ts,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::collect::ScanOffsets;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fixture() -> &'static str {
        include_str!("../fixtures/claude_transcript.jsonl")
    }

    #[test]
    fn parses_assistant_usage_lines() {
        let line = r#"{"type":"assistant","uuid":"u1","sessionId":"sess-1","timestamp":"2026-09-30T10:00:00.000Z","cwd":"/work/myproj","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":12,"output_tokens":34,"cache_creation_input_tokens":56,"cache_read_input_tokens":78}}}"#;
        let e = parse_line(line, "-work-myproj").unwrap();
        assert_eq!(e.provider, "claude");
        assert_eq!(e.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(e.session_id.as_deref(), Some("sess-1"));
        assert_eq!(e.project.as_deref(), Some("myproj"));
        assert_eq!(e.input_tokens, 12);
        assert_eq!(e.output_tokens, 34);
        assert_eq!(e.cache_write_tokens, 56);
        assert_eq!(e.cache_read_tokens, 78);
        assert!(e.ts_unix > 0.0);
    }

    #[test]
    fn skips_glm_family_records() {
        let line = r#"{"type":"assistant","uuid":"g1","sessionId":"s","timestamp":"2026-09-30T10:00:00Z","message":{"model":"glm-4.6","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        assert!(parse_line(line, "s").is_none(), "glm records ledger as glm");
        assert!(is_glm_model("glm-4.5-air"));
        assert!(!is_glm_model("claude-sonnet-4-5"));
    }

    #[test]
    fn skips_non_assistant_and_malformed_lines() {
        assert!(parse_line(r#"{"type":"user","uuid":"u"}"#, "s").is_none());
        assert!(parse_line("not json", "s").is_none());
        assert!(parse_line(r#"{"type":"assistant","message":{}}"#, "s").is_none());
        assert!(parse_line("", "s").is_none());
    }

    #[test]
    fn slug_fallback_when_no_cwd() {
        let line = r#"{"type":"assistant","uuid":"u","timestamp":"2026-09-30T10:00:00Z","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let e = parse_line(line, "-Users-me-coolproj").unwrap();
        assert_eq!(e.project.as_deref(), Some("coolproj"));
    }

    #[test]
    fn collect_reads_transcript_tree_incrementally() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("usg-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("projects").join("-work-proj");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("sess1.jsonl");
        std::fs::write(&file, fixture()).unwrap();
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", &root);
        }

        let mut offsets = ScanOffsets::detached();
        let events = collect(&mut offsets);
        assert_eq!(events.len(), 2, "fixture has two assistant usage lines");
        assert!(events.iter().all(|e| e.provider == "claude"));
        assert!(events.iter().all(|e| e.project.as_deref() == Some("proj")));

        // Second scan consumes nothing new; appended lines are picked up.
        assert!(collect(&mut offsets).is_empty());
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        use std::io::Write;
        writeln!(
            f,
            r#"{{"type":"assistant","uuid":"u3","sessionId":"s9","timestamp":"2026-09-30T11:00:00Z","cwd":"/work/proj","message":{{"model":"m","usage":{{"input_tokens":1,"output_tokens":2}}}}}}"#
        )
        .unwrap();
        drop(f);
        let more = collect(&mut offsets);
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].input_tokens, 1);

        unsafe {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
