//! Droid (Factory AI) session scanner.
//!
//! What the CLI persists locally (verified against docs.factory.ai,
//! deepwiki factory-ai/factory, and droid-dash/openusage parsers):
//! - `~/.factory/sessions/<encoded-cwd>/<uuid>.settings.json` — one JSON doc
//!   per session carrying cumulative `tokenUsage` counters
//!   (`inputTokens`/`outputTokens`/`cacheCreationTokens`/`cacheReadTokens`/
//!   `thinkingTokens`/`factoryCredits`), plus `model`, `autonomyMode`,
//!   `assistantActiveTimeMs`. The counter only grows, so the event is the
//!   delta against the tuple stored at the last scan.
//! - `~/.factory/sessions/<encoded-cwd>/<uuid>.jsonl` — the transcript.
//!   `session_start` (first line) carries `cwd`; some versions write
//!   per-message `usage` on assistant records. When a transcript yields any
//!   usage record it is authoritative for that session and the settings
//!   delta stays off — otherwise both would count the same tokens.
//! - `<project>/.factory/sessions/<uuid>.json` — older single-document
//!   sessions (pre directory-scoped layout). A doc with per-message usage
//!   arrays emits per record; a cumulative-only doc uses the same delta.
//!
//! `auth.json`/`auth.v2.*` are OAuth material only — never opened here.
//! Without token fields nothing is emitted: we never invent numbers.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util::{
    self, collect_files, file_mtime, get_path, model_on_record, read_jsonl_events, resume_offset,
    session_on_record, ts_on_record, usage_counts, usage_on_record, UsageCounts,
};
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "droid";

/// Where a cumulative usage object sits inside a session document.
const USAGE_DOC_PATHS: &[&[&str]] = &[
    &["tokenUsage"],
    &["usage"],
    &["token_usage"],
    &["tokens"],
    &["sessionStats", "tokenUsage"],
    &["settings", "tokenUsage"],
    &["stats", "tokenUsage"],
];

/// Document array fields that may hold per-message records.
const RECORD_ARRAYS: &[&str] = &["messages", "history", "turns", "records", "entries", "events"];

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for (root, hint) in roots_with_hints() {
        if !root.is_dir() {
            continue;
        }
        events.extend(scan_root(&root, hint.as_deref(), store.as_ref()));
    }
    events
}

/// Local roots the scanner reads (for `usg doctor`).
pub fn scan_roots() -> Vec<PathBuf> {
    roots_with_hints().into_iter().map(|(p, _)| p).collect()
}

/// Sessions roots + a project-name hint for the project-local layout.
/// `~/.factory/sessions` covers both the modern `<encoded-cwd>/` subdirs and
/// flat favorited docs; each encoded dir name also decodes to a project cwd
/// whose own `<project>/.factory/sessions` is scanned (older CLI layout).
/// `FACTORY_HOME` overrides the default `~/.factory`.
fn roots_with_hints() -> Vec<(PathBuf, Option<String>)> {
    let home = util::home();
    let factory_home = std::env::var("FACTORY_HOME")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".factory"));
    let home_sessions = factory_home.join("sessions");
    let mut roots = vec![(home_sessions.clone(), None)];
    if let Ok(entries) = fs::read_dir(&home_sessions) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // `-Users-me-code-myapp` → `/Users/me/code/myapp`.
            let decoded = name.replace('-', "/");
            let proj = PathBuf::from(&decoded);
            if let Some(hint) = proj.file_name().and_then(|n| n.to_str()) {
                roots.push((
                    proj.join(".factory").join("sessions"),
                    Some(hint.to_string()),
                ));
            }
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn scan_root(root: &Path, hint: Option<&str>, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let mut events = Vec::new();
    let mut settings_files = Vec::new();
    for path in collect_files(std::slice::from_ref(&root.to_path_buf()), &["json", "jsonl"]) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".settings.json") {
            settings_files.push(path);
        } else if name.ends_with(".jsonl") {
            events.extend(scan_transcript(&path, root, hint, store));
        } else {
            events.extend(scan_session_doc(&path, root, hint, store));
        }
    }
    // Transcripts first: a `.settings.json` delta is suppressed once its
    // sibling transcript has emitted any per-record usage.
    for path in settings_files {
        events.extend(scan_settings(&path, root, hint, store));
    }
    events
}

/// `*.jsonl` transcript — append-only, incremental by byte offset.
fn scan_transcript(
    path: &Path,
    root: &Path,
    hint: Option<&str>,
    store: Option<&TokenStore>,
) -> Vec<TokenEvent> {
    let offset = resume_offset(store, path);
    let fallback_ts = file_mtime(path);
    let stem = stem_of(path);
    let fallback_model = transcript_model(path);
    let project = project_label(path, root, hint, transcript_cwd(path));
    let (found, next) = read_jsonl_events(path, offset, |record| {
        let Some(counts) = usage_on_record(record) else {
            return Vec::new();
        };
        vec![TokenEvent {
            provider: PROVIDER.into(),
            model: model_on_record(record).or_else(|| fallback_model.clone()),
            session_id: session_on_record(record).or_else(|| stem.clone()),
            project: project.clone(),
            ts_unix: ts_on_record(record).unwrap_or(fallback_ts),
            input_tokens: counts.input,
            output_tokens: counts.output,
            cache_read_tokens: counts.cache_read,
            cache_write_tokens: counts.cache_write,
        }]
    });
    util::mark_scanned(store, path, next);
    if !found.is_empty() {
        mark_usage_seen(store, path);
    }
    found
}

/// `*.settings.json` — cumulative `tokenUsage` → positive delta per rescan.
fn scan_settings(
    path: &Path,
    root: &Path,
    hint: Option<&str>,
    store: Option<&TokenStore>,
) -> Vec<TokenEvent> {
    let stem = stem_of(path);
    let jsonl = path.with_file_name(format!("{}.jsonl", stem.as_deref().unwrap_or("")));
    // A transcript that carries its own usage records owns this session.
    if jsonl.exists() && usage_seen(store, &jsonl) {
        return Vec::new();
    }
    if !util::whole_file_changed(store, path) {
        return Vec::new();
    }
    let Some(doc) = util::read_json(path) else {
        return Vec::new(); // mid-write — retry next scan
    };
    let events = doc_events(&doc, path, root, hint, store, |doc| {
        let cwd = cwd_on_doc(doc).or_else(|| transcript_cwd(&jsonl));
        let last = file_mtime(&jsonl).max(file_mtime(path));
        SessionCtx {
            session_id: session_on_record(doc)
                .or_else(|| str_at(doc, &["id", "sessionId"]))
                .or_else(|| stem.clone()),
            model: model_on_record(doc)
                .or_else(|| nested_model(doc))
                .or_else(|| transcript_model(&jsonl)),
            project: project_label(path, root, hint, cwd),
            last_activity: last,
        }
    });
    if let Ok(meta) = fs::metadata(path) {
        util::mark_scanned(store, path, meta.len());
    }
    events
}

/// Older `<uuid>.json` single-document sessions (and any other non-settings
/// `*.json` under a sessions root): per-record arrays win, else cumulative.
fn scan_session_doc(
    path: &Path,
    root: &Path,
    hint: Option<&str>,
    store: Option<&TokenStore>,
) -> Vec<TokenEvent> {
    if !util::whole_file_changed(store, path) {
        return Vec::new();
    }
    let Some(doc) = util::read_json(path) else {
        return Vec::new();
    };
    let stem = stem_of(path);
    let jsonl = path.with_file_name(format!("{}.jsonl", stem.as_deref().unwrap_or("")));
    let events = doc_events(&doc, path, root, hint, store, |doc| {
        let cwd = cwd_on_doc(doc).or_else(|| transcript_cwd(&jsonl));
        SessionCtx {
            session_id: session_on_record(doc)
                .or_else(|| str_at(doc, &["id", "sessionId"]))
                .or_else(|| stem.clone()),
            model: model_on_record(doc)
                .or_else(|| nested_model(doc))
                .or_else(|| transcript_model(&jsonl)),
            project: project_label(path, root, hint, cwd),
            last_activity: file_mtime(path),
        }
    });
    if let Ok(meta) = fs::metadata(path) {
        util::mark_scanned(store, path, meta.len());
    }
    events
}

struct SessionCtx {
    session_id: Option<String>,
    model: Option<String>,
    project: Option<String>,
    last_activity: f64,
}

/// Emit per-record events when the doc carries usage-bearing record arrays,
/// else the cumulative delta. `ctx` builds the session-level metadata.
fn doc_events(
    doc: &Value,
    path: &Path,
    _root: &Path,
    _hint: Option<&str>,
    store: Option<&TokenStore>,
    ctx: impl Fn(&Value) -> SessionCtx,
) -> Vec<TokenEvent> {
    let ctx = ctx(doc);
    let doc_ts = ts_on_record(doc).unwrap_or(ctx.last_activity);

    // Per-record mode: `messages`/`history`/`turns`/… entries with usage.
    let mut per_record = Vec::new();
    for key in RECORD_ARRAYS {
        let Some(items) = doc.get(*key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(counts) = usage_on_record(item) else {
                continue;
            };
            per_record.push(TokenEvent {
                provider: PROVIDER.into(),
                model: model_on_record(item).or_else(|| ctx.model.clone()),
                session_id: session_on_record(item).or_else(|| ctx.session_id.clone()),
                project: ctx.project.clone(),
                ts_unix: ts_on_record(item).unwrap_or(doc_ts),
                input_tokens: counts.input,
                output_tokens: counts.output,
                cache_read_tokens: counts.cache_read,
                cache_write_tokens: counts.cache_write,
            });
        }
    }
    if !per_record.is_empty() {
        // Still record the cumulative tuple: if a later write drops the
        // records array, the delta resumes from here instead of recounting.
        if let Some(cur) = usage_tuple(doc) {
            store_counts(store, path, cur);
        }
        return per_record;
    }

    // Cumulative mode: store the latest tuple; emit only the growth.
    let Some(cur) = usage_tuple(doc) else {
        return Vec::new();
    };
    let prev = stored_counts(store, path);
    store_counts(store, path, cur);
    let delta = UsageCounts {
        input: cur.input.saturating_sub(prev.input),
        output: cur.output.saturating_sub(prev.output),
        cache_read: cur.cache_read.saturating_sub(prev.cache_read),
        cache_write: cur.cache_write.saturating_sub(prev.cache_write),
    };
    if delta.is_empty() {
        return Vec::new();
    }
    vec![TokenEvent {
        provider: PROVIDER.into(),
        model: ctx.model,
        session_id: ctx.session_id,
        project: ctx.project,
        ts_unix: ctx.last_activity,
        input_tokens: delta.input,
        output_tokens: delta.output,
        cache_read_tokens: delta.cache_read,
        cache_write_tokens: delta.cache_write,
    }]
}

/// First cumulative usage object on the doc. `inclusiveTokenUsage` (input
/// already includes cache reads) is the last resort, de-inclusived.
fn usage_tuple(doc: &Value) -> Option<UsageCounts> {
    for path in USAGE_DOC_PATHS {
        if let Some(usage) = get_path(doc, path)
            && let Some(counts) = usage_counts(usage)
        {
            return Some(counts);
        }
    }
    if let Some(usage) = doc.get("inclusiveTokenUsage")
        && let Some(mut counts) = usage_counts(usage)
    {
        counts.input = counts.input.saturating_sub(counts.cache_read);
        return Some(counts);
    }
    None
}

/// The stored cumulative tuple, one scan-offset row per bucket.
fn stored_counts(store: Option<&TokenStore>, path: &Path) -> UsageCounts {
    let mut c = UsageCounts::default();
    if let Some(store) = store {
        for (i, v) in [
            &mut c.input,
            &mut c.output,
            &mut c.cache_read,
            &mut c.cache_write,
        ]
        .into_iter()
        .enumerate()
        {
            *v = store
                .scan_offset(&bucket_key(path, i))
                .ok()
                .flatten()
                .map(|(n, _)| n)
                .unwrap_or(0);
        }
    }
    c
}

fn store_counts(store: Option<&TokenStore>, path: &Path, c: UsageCounts) {
    let Some(store) = store else { return };
    for (i, v) in [c.input, c.output, c.cache_read, c.cache_write]
        .iter()
        .enumerate()
    {
        store.set_scan_offset(&bucket_key(path, i), *v, 0.0);
    }
}

fn bucket_key(path: &Path, bucket: usize) -> PathBuf {
    PathBuf::from(format!("{}#droid-bucket-{}", path.display(), bucket))
}

fn flag_key(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}#droid-transcript-usage", path.display()))
}

fn usage_seen(store: Option<&TokenStore>, transcript: &Path) -> bool {
    store
        .and_then(|s| s.scan_offset(&flag_key(transcript)).ok().flatten())
        .map(|(n, _)| n > 0)
        .unwrap_or(false)
}

fn mark_usage_seen(store: Option<&TokenStore>, transcript: &Path) {
    if let Some(store) = store {
        store.set_scan_offset(&flag_key(transcript), 1, file_mtime(transcript));
    }
}

/// `abc.settings.json` → `abc`; `abc.jsonl` → `abc`.
fn stem_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".settings.json")
        .or_else(|| name.strip_suffix(".jsonl"))
        .or_else(|| name.strip_suffix(".json"))?;
    Some(stem.to_string())
}

fn cwd_on_doc(doc: &Value) -> Option<String> {
    str_at(doc, &["cwd", "workdir", "workingDirectory", "projectPath"])
}

fn str_at(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        value
            .get(*k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn nested_model(doc: &Value) -> Option<String> {
    get_path(doc, &["sessionSettings", "model"])
        .or_else(|| get_path(doc, &["settings", "model"]))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// First complete line of a transcript (the `session_start` record).
fn first_record(path: &Path) -> Option<Value> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(1024 * 1024));
    let mut buf = Vec::new();
    if reader.read_until(b'\n', &mut buf).ok()? == 0 {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}

fn transcript_cwd(path: &Path) -> Option<String> {
    let rec = first_record(path)?;
    if rec.get("type").and_then(Value::as_str) != Some("session_start") {
        return None;
    }
    cwd_on_doc(&rec)
}

/// First model field seen in the transcript head (bounded, ~64 lines).
fn transcript_model(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(1024 * 1024));
    let mut buf = Vec::new();
    for _ in 0..64 {
        buf.clear();
        if reader.read_until(b'\n', &mut buf).ok()? == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&buf);
        let Ok(rec) = serde_json::from_str::<Value>(text.trim()) else {
            continue;
        };
        if let Some(m) = model_on_record(&rec) {
            return Some(m);
        }
    }
    None
}

/// Project label: session cwd basename when known, else the encoded dir
/// name or the project-root hint for project-local session dirs.
fn project_label(
    file: &Path,
    root: &Path,
    hint: Option<&str>,
    cwd: Option<String>,
) -> Option<String> {
    if let Some(cwd) = cwd
        && let Some(name) = Path::new(&cwd).file_name().and_then(|n| n.to_str())
        && !name.is_empty()
    {
        return Some(name.to_string());
    }
    let parent = file.parent()?;
    if parent == root {
        return hint.map(str::to_string);
    }
    parent
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| {
            // `-Users-me-code-myapp` decodes to `/Users/me/code/myapp`.
            Path::new(&name.replace('-', "/"))
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-droid-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sessions/-Users-t-code-myapp")).unwrap();
        dir
    }

    fn store_at(dir: &Path) -> TokenStore {
        TokenStore::open_at(dir.join("test.sqlite3")).unwrap()
    }

    fn write_settings(dir: &Path, input: u64, output: u64, cache: u64) -> PathBuf {
        let p = dir.join("sessions/-Users-t-code-myapp/sess-1.settings.json");
        fs::write(
            &p,
            format!(
                r#"{{"model":"claude-sonnet-4-5-20250929","autonomyMode":"auto-low","assistantActiveTimeMs":60000,"tokenUsage":{{"inputTokens":{input},"outputTokens":{output},"cacheCreationTokens":{cache},"cacheReadTokens":{cache},"thinkingTokens":10,"factoryCredits":2}}}}"#
            ),
        )
        .unwrap();
        p
    }

    #[test]
    fn settings_cumulative_emits_deltas() {
        let dir = fixture_dir("delta");
        let root = dir.join("sessions");
        let store = store_at(&dir);
        let p = write_settings(&dir, 1000, 500, 200);
        // sibling transcript with no usage → settings delta is authoritative
        fs::write(
            dir.join("sessions/-Users-t-code-myapp/sess-1.jsonl"),
            "{\"type\":\"session_start\",\"cwd\":\"/Users/t/code/myapp\"}\n{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":[]}}\n",
        )
        .unwrap();

        let events = scan_settings(&p, &root, None, Some(&store));
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.provider, "droid");
        assert_eq!(e.input_tokens, 1000);
        assert_eq!(e.output_tokens, 510); // output + thinking
        assert_eq!(e.cache_read_tokens, 200);
        assert_eq!(e.cache_write_tokens, 200);
        assert_eq!(e.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(e.session_id.as_deref(), Some("sess-1"));
        assert_eq!(e.project.as_deref(), Some("myapp"));

        // Rescan unchanged → nothing.
        assert!(scan_settings(&p, &root, None, Some(&store)).is_empty());

        // Counters grew → only the growth.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let p = write_settings(&dir, 1500, 900, 400);
        let events = scan_settings(&p, &root, None, Some(&store));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 500);
        assert_eq!(events[0].output_tokens, 400);
        assert_eq!(events[0].cache_read_tokens, 200);
        assert_eq!(events[0].cache_write_tokens, 200);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcript_records_win_over_settings_delta() {
        let dir = fixture_dir("transcript");
        let root = dir.join("sessions");
        let store = store_at(&dir);
        let sess = dir.join("sessions/-Users-t-code-myapp");
        let mut f = fs::File::create(sess.join("s-2.jsonl")).unwrap();
        writeln!(f, r#"{{"type":"session_start","cwd":"/Users/t/code/myapp","sessionId":"s-2"}}"#).unwrap();
        writeln!(f, r#"{{"type":"message","model":"glm-4.6","timestamp":"2026-09-01T10:00:00Z","message":{{"role":"assistant","usage":{{"input_tokens":100,"output_tokens":40,"cache_read_input_tokens":20}}}}}}"#).unwrap();
        writeln!(f, "not json").unwrap();
        write_settings(&dir, 9999, 9999, 9999);

        let transcript = sess.join("s-2.jsonl");
        let events = scan_transcript(&transcript, &root, None, Some(&store));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[0].output_tokens, 40);
        assert_eq!(events[0].cache_read_tokens, 20);
        assert_eq!(events[0].model.as_deref(), Some("glm-4.6"));
        assert!(usage_seen(Some(&store), &transcript));
        // A settings file for THIS session is suppressed by the flag; one
        // for a different session (no sibling transcript) is not.
        let suppressed = sess.join("s-2.settings.json");
        fs::write(
            &suppressed,
            r#"{"model":"glm-4.6","tokenUsage":{"inputTokens":900,"outputTokens":400}}"#,
        )
        .unwrap();
        assert!(scan_settings(&suppressed, &root, None, Some(&store)).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_doc_messages_array_or_cumulative() {
        let dir = fixture_dir("doc");
        let root = dir.join("sessions");
        let store = store_at(&dir);

        // Per-message records win over the doc's own cumulative field.
        let doc_path = dir.join("sessions/-Users-t-code-myapp/sess-3.json");
        fs::write(
            &doc_path,
            r#"{"sessionId":"sess-3","cwd":"/Users/t/code/other","tokenUsage":{"inputTokens":999,"outputTokens":1},"messages":[{"timestamp":"2026-09-01T09:00:00Z","usage":{"inputTokens":10,"outputTokens":5}},{"usage":{"inputTokens":20,"outputTokens":7}}]}"#,
        )
        .unwrap();
        let events = scan_session_doc(&doc_path, &root, None, Some(&store));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].input_tokens, 10);
        assert_eq!(events[1].input_tokens, 20);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-3"));
        assert_eq!(events[0].project.as_deref(), Some("other"));

        // Cumulative-only doc → one delta event.
        let cum_path = dir.join("sessions/-Users-t-code-myapp/sess-4.json");
        fs::write(
            &cum_path,
            r#"{"id":"sess-4","tokenUsage":{"inputTokens":300,"outputTokens":120,"cacheReadTokens":50}}"#,
        )
        .unwrap();
        let events = scan_session_doc(&cum_path, &root, None, Some(&store));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 300);
        assert_eq!(events[0].output_tokens, 120);
        assert_eq!(events[0].cache_read_tokens, 50);
        assert_eq!(events[0].project.as_deref(), Some("myapp"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_and_zero_usage_emit_nothing() {
        let dir = fixture_dir("empty");
        let root = dir.join("sessions");
        let store = store_at(&dir);
        let bad = dir.join("sessions/-Users-t-code-myapp/bad.json");
        fs::write(&bad, "{not json").unwrap();
        assert!(scan_session_doc(&bad, &root, None, Some(&store)).is_empty());
        let zero = dir.join("sessions/-Users-t-code-myapp/zero.settings.json");
        fs::write(
            &zero,
            r#"{"model":"m","tokenUsage":{"inputTokens":0,"outputTokens":0,"cacheCreationTokens":0,"cacheReadTokens":0,"thinkingTokens":0}}"#,
        )
        .unwrap();
        assert!(scan_settings(&zero, &root, None, Some(&store)).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
