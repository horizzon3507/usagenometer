//! Cline (`saoudrizwan.claude-dev`) and Roo Code (`rooveterinaryinc.roo-cline`)
//! local task message scanner.
//!
//! Both extensions persist per-turn token counts in VS Code / Cursor /
//! Windsurf / Antigravity `User/globalStorage/<ext_id>/tasks/<task>/ui_messages.json`
//! inside `say: "api_req_started"` records whose `text` holds JSON
//! `{tokensIn, tokensOut, cacheReads, cacheWrites, model, ...}`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util::{self, collect_files, file_mtime, ts_on_record};
use crate::tokens::{TokenEvent, TokenStore};

const CLINE_EXT: &str = "saoudrizwan.claude-dev";
const ROO_EXT: &str = "rooveterinaryinc.roo-cline";

pub fn scan_cline() -> Vec<TokenEvent> {
    scan_cline_with_store(TokenStore::open().ok().as_ref())
}

pub fn scan_roo() -> Vec<TokenEvent> {
    scan_roo_with_store(TokenStore::open().ok().as_ref())
}

pub(crate) fn scan_cline_with_store(store: Option<&TokenStore>) -> Vec<TokenEvent> {
    scan_ext_with_store("cline", &cline_roots(), store)
}

pub(crate) fn scan_roo_with_store(store: Option<&TokenStore>) -> Vec<TokenEvent> {
    scan_ext_with_store("roo-code", &roo_roots(), store)
}

pub fn cline_roots() -> Vec<PathBuf> {
    ext_roots("CLINE_HOME", ".cline", CLINE_EXT)
}

pub fn roo_roots() -> Vec<PathBuf> {
    ext_roots("ROO_CODE_HOME", ".roo-code", ROO_EXT)
}

fn ext_roots(env_var: &str, dot_dir: &str, ext_id: &str) -> Vec<PathBuf> {
    if let Ok(custom) = std::env::var(env_var) {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return vec![PathBuf::from(trimmed)];
        }
    }
    let home = util::home();
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    let mut roots = vec![home.join(dot_dir)];
    for app in ["Code", "Cursor", "Windsurf", "Antigravity", "VSCodium"] {
        roots.push(
            cfg.join(app)
                .join("User")
                .join("globalStorage")
                .join(ext_id)
                .join("tasks"),
        );
    }
    roots
}

fn scan_ext_with_store(
    provider: &str,
    roots: &[PathBuf],
    store: Option<&TokenStore>,
) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for file in collect_files(std::slice::from_ref(root), &["json"]) {
            if file.file_name().and_then(|n| n.to_str()) != Some("ui_messages.json") {
                continue;
            }
            if !util::whole_file_changed(store, &file) {
                continue;
            }
            events.extend(parse_ui_messages(provider, &file));
            if let Ok(meta) = fs::metadata(&file) {
                util::mark_scanned(store, &file, meta.len());
            }
        }
    }
    events
}

pub(crate) fn parse_ui_messages(provider: &str, path: &Path) -> Vec<TokenEvent> {
    let Some(Value::Array(items)) = util::read_json(path) else {
        return Vec::new();
    };
    let fallback_ts = file_mtime(path);
    let session_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string);
    let mut events = Vec::new();
    for item in items {
        let is_req = item.get("say").and_then(Value::as_str) == Some("api_req_started")
            || item.get("ask").and_then(Value::as_str) == Some("api_req_started");
        if !is_req {
            continue;
        }
        let payload_obj = match item.get("text") {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
            Some(v @ Value::Object(_)) => Some(v.clone()),
            _ => None,
        };
        let Some(payload) = payload_obj else {
            continue;
        };
        let num = |keys: &[&str]| -> u64 {
            keys.iter()
                .find_map(|k| payload.get(*k).and_then(Value::as_u64))
                .unwrap_or(0)
        };
        let input = num(&["tokensIn", "inputTokens", "input_tokens"]);
        let output = num(&["tokensOut", "outputTokens", "output_tokens"]);
        let cache_read = num(&["cacheReads", "cacheReadTokens", "cache_read_tokens"]);
        let cache_write = num(&["cacheWrites", "cacheWriteTokens", "cache_write_tokens"]);
        if input + output + cache_read + cache_write == 0 {
            continue;
        }
        let model = payload
            .get("model")
            .or_else(|| payload.get("modelId"))
            .or_else(|| item.get("model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let ts = ts_on_record(&item)
            .or_else(|| ts_on_record(&payload))
            .unwrap_or(fallback_ts);
        events.push(TokenEvent {
            provider: provider.into(),
            model,
            session_id: session_id.clone(),
            project: None,
            ts_unix: ts,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cline_and_roo_ui_messages() {
        let dir = std::env::temp_dir().join(format!("usg-cline-{}", std::process::id()));
        let task = dir.join("tasks/task-42");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&task).unwrap();
        let file = task.join("ui_messages.json");
        let inner = serde_json::json!({
            "tokensIn": 1200,
            "tokensOut": 340,
            "cacheReads": 500,
            "cacheWrites": 100,
            "model": "claude-sonnet-4-5"
        })
        .to_string();
        fs::write(
            &file,
            serde_json::json!([
                { "ts": 1759300000000_u64, "type": "say", "say": "text", "text": "hello" },
                { "ts": 1759300005000_u64, "type": "say", "say": "api_req_started", "text": inner }
            ])
            .to_string(),
        )
        .unwrap();

        let cline_events = parse_ui_messages("cline", &file);
        assert_eq!(cline_events.len(), 1);
        assert_eq!(cline_events[0].provider, "cline");
        assert_eq!(cline_events[0].input_tokens, 1200);
        assert_eq!(cline_events[0].output_tokens, 340);
        assert_eq!(cline_events[0].cache_read_tokens, 500);
        assert_eq!(cline_events[0].cache_write_tokens, 100);
        assert_eq!(cline_events[0].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(cline_events[0].session_id.as_deref(), Some("task-42"));

        let roo_events = parse_ui_messages("roo-code", &file);
        assert_eq!(roo_events.len(), 1);
        assert_eq!(roo_events[0].provider, "roo-code");

        let _ = fs::remove_dir_all(&dir);
    }
}
