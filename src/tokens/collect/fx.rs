//! fx (vercel-labs/fx, fx.sh) usage-ledger scanner.
//!
//! fx keeps its own profile usage ledger at `~/.fx/usage.jsonl` (fx resolves
//! the profile from `$HOME` only — no env override). Verified against
//! `src/core/session/profile_usage_store.zig` + `generation_fact_codec.zig`:
//! every line is `{"schema_version":1,"kind":...}` with kinds
//! - `coverage` `{started_at_ms}` — tracking start, no usage;
//! - `generation` `{fact:{id,created_at_ms,model,input_tokens,output_tokens,
//!   cache_read_tokens,cache_write_tokens,reasoning_tokens,
//!   billable_web_search_calls,total_cost}}` — one settled model request;
//! - `pending` / `incident` — usage awaiting Gateway data or coverage gaps,
//!   never counted.
//!
//! Every provider fx talks to (AI Gateway, Codex and Grok subscriptions,
//! configured endpoints) lands here: non-Gateway ids are hashed into the same
//! `gen_<26>` shape. `input_tokens` includes both cache buckets (fx's
//! validator enforces `cache_* <= input`) and `reasoning_tokens` is a subset
//! of `output_tokens`, so the ledger's disjoint input is
//! `input - cache_read - cache_write`. A fact id may appear twice (exact
//! duplicate or a conflicting variant fx keeps as incomplete evidence); the
//! first occurrence wins, matching fx's own report.
//!
//! fx compacts the file in place (records older than 35 days are dropped via
//! an atomic replace), so byte offsets are not stable — the whole file is
//! re-read whenever its size or mtime changes and per-event hashes dedupe.
//! Facts carry no session or workspace, so neither is attributed.
//! `auth.json` / `api-key` are credentials and are never read here.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util;
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "fx";
const USAGE_FILE: &str = "usage.jsonl";
/// fx caps the ledger at 32 MiB itself; never read past that.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

pub fn scan() -> Vec<TokenEvent> {
    scan_with_store(TokenStore::open().ok().as_ref())
}

pub(crate) fn scan_with_store(store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    for root in scan_roots() {
        let path = root.join(USAGE_FILE);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        if !util::whole_file_changed(store, &path) {
            continue;
        }
        events.extend(parse_file(&path));
        util::mark_scanned(store, &path, meta.len());
    }
    events
}

pub fn scan_roots() -> Vec<PathBuf> {
    vec![util::home().join(".fx")]
}

fn parse_file(path: &Path) -> Vec<TokenEvent> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some((id, event)) = generation_event(&record)
            && seen.insert(id)
        {
            events.push(event);
        }
    }
    events
}

/// `(fact id, event)` for a `kind:"generation"` record; `None` otherwise.
fn generation_event(record: &Value) -> Option<(String, TokenEvent)> {
    if record.get("kind").and_then(Value::as_str) != Some("generation") {
        return None;
    }
    let fact = record.get("fact")?;
    let id = fact.get("id").and_then(Value::as_str)?.to_string();
    let created_at_ms = fact.get("created_at_ms").and_then(Value::as_u64)?;
    let num = |key: &str| fact.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input_total = num("input_tokens");
    let output = num("output_tokens");
    let cache_read = num("cache_read_tokens");
    let cache_write = num("cache_write_tokens");
    if input_total + output == 0 {
        return None;
    }
    let model = fact
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    Some((
        id,
        TokenEvent {
            provider: PROVIDER.into(),
            model,
            session_id: None,
            project: None,
            ts_unix: created_at_ms as f64 / 1000.0,
            input_tokens: input_total.saturating_sub(cache_read + cache_write),
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fact_line(id: &str, ts: u64, input: u64, output: u64, cr: u64, cw: u64) -> String {
        format!(
            r#"{{"schema_version":1,"kind":"generation","fact":{{"id":"{id}","created_at_ms":{ts},"model":"openai/gpt-5.5","input_tokens":{input},"output_tokens":{output},"cache_read_tokens":{cr},"cache_write_tokens":{cw},"reasoning_tokens":null,"billable_web_search_calls":0,"total_cost":0.0123}}}}"#
        )
    }

    fn ledger(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-fx-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join(USAGE_FILE)
    }

    #[test]
    fn splits_cache_out_of_inclusive_input_and_skips_non_generation_kinds() {
        let path = ledger("split");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"schema_version":1,"kind":"coverage","started_at_ms":1790000000000}}"#
        )
        .unwrap();
        writeln!(
            f,
            "{}",
            fact_line(
                "gen_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                1_790_000_100_000,
                130,
                50,
                20,
                10
            )
        )
        .unwrap();
        writeln!(f, r#"{{"schema_version":1,"kind":"pending","id":"gen_01ARZ3NDEKTSV4RRFFQ69G5FAW","observed_at_ms":1790000200000}}"#).unwrap();
        writeln!(f, r#"{{"schema_version":1,"kind":"incident","occurred_at_ms":1790000300000,"completeness":"incomplete"}}"#).unwrap();
        drop(f);

        let events = parse_file(&path);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.provider, "fx");
        assert_eq!(e.model.as_deref(), Some("openai/gpt-5.5"));
        assert_eq!(e.input_tokens, 100);
        assert_eq!(e.output_tokens, 50);
        assert_eq!(e.cache_read_tokens, 20);
        assert_eq!(e.cache_write_tokens, 10);
        assert_eq!(e.ts_unix, 1_790_000_100.0);
    }

    #[test]
    fn first_variant_of_a_repeated_fact_id_wins() {
        let path = ledger("dupe");
        let mut f = fs::File::create(&path).unwrap();
        let id = "gen_01ARZ3NDEKTSV4RRFFQ69G5FAV";
        writeln!(f, "{}", fact_line(id, 1_790_000_100_000, 10, 5, 0, 0)).unwrap();
        writeln!(f, "{}", fact_line(id, 1_790_000_100_000, 10, 5, 0, 0)).unwrap();
        writeln!(f, "{}", fact_line(id, 1_790_000_100_000, 10, 9, 0, 0)).unwrap();
        writeln!(f, "not json").unwrap();
        drop(f);

        let events = parse_file(&path);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].output_tokens, 5);
    }
}
