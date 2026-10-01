//! GLM Coding Plan (z.ai) session scanner.
//!
//! The GLM plan runs inside Claude Code (`ANTHROPIC_BASE_URL` → api.z.ai), so
//! its sessions are Claude-format transcripts under
//! `~/.claude/projects/**/*.jsonl` — only `message.model` differs
//! (`glm-4.5`, `glm-4.6`, `glm-4.5-air`, ...). This scanner reuses the Claude
//! record parser and emits provider `glm` for GLM-family models only;
//! `collect::claude` skips those same records so usage never double-counts.
//!
//! Scan offsets are namespaced (`glm:<path>`) — Claude's own scanner already
//! tracks byte offsets for these same files under the plain path key.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::claude;
use super::util::{file_mtime, read_jsonl_events};
use crate::tokens::TokenEvent;
use crate::tokens::TokenStore;
use crate::tokens::collect::jsonl_files;

const PROVIDER: &str = "glm";

/// Full scan of the shared Claude projects tree; incremental via namespaced
/// byte offsets when the ledger store is reachable.
pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for file in jsonl_files(&claude::claude_root().join("projects")) {
        let slug = file
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let key = offset_key(&file);
        let offset = resume_offset(store.as_ref(), &key, &file);
        let (mut found, next) = read_jsonl_events(&file, offset, |v| glm_event(v, &slug));
        events.append(&mut found);
        if let Some(store) = store.as_ref() {
            store.set_scan_offset(&key, next, file_mtime(&file));
        }
    }
    events
}

/// Roots this scanner reads (for `usg doctor`).
pub fn scan_roots() -> Vec<PathBuf> {
    vec![claude::claude_root().join("projects")]
}

/// GLM offsets live under `glm:<path>` so they stay independent of the byte
/// offsets `collect::claude` keeps for the same files.
fn offset_key(file: &Path) -> PathBuf {
    PathBuf::from(format!("{PROVIDER}:{}", file.display()))
}

/// Stored byte offset for `key`, reset when the file shrank (truncate/rotate).
fn resume_offset(store: Option<&TokenStore>, key: &Path, file: &Path) -> u64 {
    let Some(store) = store else {
        return 0;
    };
    let Ok(Some((offset, _))) = store.scan_offset(key) else {
        return 0;
    };
    let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    if size < offset { 0 } else { offset }
}

fn glm_event(record: &Value, slug: &str) -> Vec<TokenEvent> {
    let Some(mut event) = claude::parse_record(record, slug) else {
        return Vec::new();
    };
    if !event.model.as_deref().is_some_and(claude::is_glm_model) {
        return Vec::new();
    }
    event.provider = PROVIDER.into();
    vec![event]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fixture() -> &'static str {
        include_str!("../fixtures/glm_transcript.jsonl")
    }

    #[test]
    fn emits_glm_records_only() {
        let glm = r#"{"type":"assistant","uuid":"g1","sessionId":"s-glm","timestamp":"2026-09-30T10:00:00Z","cwd":"/work/proj","message":{"model":"glm-4.6","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":50}}}"#;
        let claude = r#"{"type":"assistant","uuid":"c1","sessionId":"s-cl","timestamp":"2026-09-30T10:01:00Z","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let v: Value = serde_json::from_str(glm).unwrap();
        let e = glm_event(&v, "-work-proj");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].provider, "glm");
        assert_eq!(e[0].model.as_deref(), Some("glm-4.6"));
        assert_eq!(e[0].input_tokens, 100);
        assert_eq!(e[0].output_tokens, 20);
        assert_eq!(e[0].cache_read_tokens, 50);
        assert_eq!(e[0].project.as_deref(), Some("proj"));

        let v: Value = serde_json::from_str(claude).unwrap();
        assert!(glm_event(&v, "s").is_empty(), "claude models are not glm");
    }

    #[test]
    fn model_family_matching() {
        assert!(claude::is_glm_model("glm-4.6"));
        assert!(claude::is_glm_model("GLM-4.5-Air"));
        assert!(claude::is_glm_model("zai/glm-4.7"));
        assert!(!claude::is_glm_model("claude-sonnet-4-5"));
        assert!(!claude::is_glm_model("glm")); // bare name, no family suffix
        assert!(!claude::is_glm_model("glmx-4"));
    }

    #[test]
    fn scan_reads_shared_tree_incrementally() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("usg-glm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("projects").join("-work-proj");
        fs::create_dir_all(&proj).unwrap();
        let file = proj.join("sess1.jsonl");
        fs::write(&file, fixture()).unwrap();
        let db = root.join("tokens.sqlite3");
        let store = TokenStore::open_at(db).unwrap();
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", &root);
        }

        // scan() opens the real store path; exercise the same machinery the
        // scanner uses (offsets are namespaced, so claude's are untouched).
        let offset = resume_offset(Some(&store), &offset_key(&file), &file);
        assert_eq!(offset, 0);
        let (events, next) = read_jsonl_events(&file, 0, |v| glm_event(v, "-work-proj"));
        // fixture: 2 glm lines + 1 claude line + non-usage/malformed lines
        assert_eq!(events.len(), 2, "only glm-family records emit");
        assert!(events.iter().all(|e| e.provider == "glm"));
        store.set_scan_offset(&offset_key(&file), next, file_mtime(&file));

        // The claude scanner sees the same file but emits only its own
        // record — glm lines are excluded there (no double counting).
        let claude_events = claude::collect(&mut crate::tokens::collect::ScanOffsets::detached());
        assert_eq!(claude_events.len(), 1);
        assert_eq!(claude_events[0].provider, "claude");
        assert_eq!(
            claude_events[0].model.as_deref(),
            Some("claude-sonnet-4-5-20250929")
        );

        // Rescan consumes nothing; appended glm lines are picked up.
        let offset = resume_offset(Some(&store), &offset_key(&file), &file);
        let (more, _) = read_jsonl_events(&file, offset, |v| glm_event(v, "-work-proj"));
        assert!(more.is_empty());
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","uuid":"g9","sessionId":"s9","timestamp":"2026-09-30T11:00:00Z","cwd":"/work/proj","message":{{"model":"glm-4.5-air","usage":{{"input_tokens":7,"output_tokens":3}}}}}}"#
        )
        .unwrap();
        drop(f);
        let offset = resume_offset(Some(&store), &offset_key(&file), &file);
        let (more, _) = read_jsonl_events(&file, offset, |v| glm_event(v, "-work-proj"));
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].model.as_deref(), Some("glm-4.5-air"));

        unsafe {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
        }
        let _ = fs::remove_dir_all(&root);
    }
}
