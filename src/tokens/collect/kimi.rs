//! Kimi (Moonshot AI) session scanner — covers both Kimi CLIs.
//!
//! kimi-cli (Python), `~/.kimi` (`KIMI_SHARE_DIR` override):
//! `sessions/<md5(workdir)>/<session-id>/wire.jsonl` — envelope records
//! `{"timestamp": <unix s>, "message": {"type", "payload"}}` behind a
//! `{"type":"metadata","protocol_version":...}` header. `StatusUpdate.payload
//! .token_usage` = `{input_other, output, input_cache_read,
//! input_cache_creation}` is real per-step usage (kimi-cli's own statistics
//! parser sums the same fields). `SubagentEvent.payload.event` wraps a nested
//! `{type, payload}` record — unwrapped so subagent usage counts.
//! `context.jsonl` `{"role":"_usage","token_count":N}` is the cumulative
//! context-size tracker, so the file is never emitted as usage. No model id
//! is persisted. `project` resolves via `kimi.json` `work_dirs[].path` (dir
//! name is `md5(path)`, or `<kaos>_<md5>` off the local kaos), falling back
//! to the work dir embedded in the `_system_prompt` record.
//!
//! kimi-code (TypeScript), `~/.kimi-code` (`KIMI_CODE_HOME` override):
//! `sessions/<workDirKey>/<sessionId>/agents/<agent>/wire.jsonl` — flat
//! records `{"type", "time": <ms epoch>, ...payload}`. `usage.record` =
//! `{agentId, model, usage:{inputOther, output, inputCacheRead,
//! inputCacheCreation}}` is per-call (`byModel` rollups live only in
//! memory). `context.append_message.message.usage` is the fallback when a
//! wire file has no `usage.record`. `subagent.completed.usage` is the
//! cumulative subagent rollup — never emitted (the subagent's own
//! `usage.record`s already cover it). `session_index.jsonl` at the root maps
//! `sessionId` → `workDir`; `wd_<slug>_<hash>` dir names give a basename
//! slug as the fallback project label.
//!
//! `credentials/` holds OAuth material only and is never read for usage.
//! Missing dirs yield zero events; malformed lines are skipped.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::util::{self, UsageCounts, collect_files, file_mtime};
use crate::providers::types::coerce_unix_seconds;
use crate::tokens::{TokenEvent, TokenStore};

const PROVIDER: &str = "kimi";

#[derive(Clone, Copy)]
enum Flavor {
    /// kimi-cli Python layout (`~/.kimi`).
    Cli,
    /// kimi-code TypeScript layout (`~/.kimi-code`).
    Code,
}

fn roots() -> Vec<(PathBuf, Flavor)> {
    let mut roots: Vec<(PathBuf, Flavor)> = Vec::new();
    let mut push = |dir: PathBuf, flavor: Flavor| {
        if !roots.iter().any(|(p, _)| *p == dir) {
            roots.push((dir, flavor));
        }
    };
    if let Ok(custom) = std::env::var("KIMI_SHARE_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            push(PathBuf::from(trimmed), Flavor::Cli);
        }
    }
    if let Ok(custom) = std::env::var("KIMI_CODE_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            push(PathBuf::from(trimmed), Flavor::Code);
        }
    }
    let home = util::home();
    push(home.join(".kimi"), Flavor::Cli);
    push(home.join(".kimi-code"), Flavor::Code);
    roots
}

pub fn scan_roots() -> Vec<PathBuf> {
    roots().into_iter().map(|(p, _)| p).collect()
}

pub fn scan() -> Vec<TokenEvent> {
    let store = TokenStore::open().ok();
    let mut events = Vec::new();
    for (root, flavor) in roots() {
        if !root.join("sessions").is_dir() {
            continue;
        }
        match flavor {
            Flavor::Cli => events.extend(scan_cli_root(&root, store.as_ref())),
            Flavor::Code => events.extend(scan_code_root(&root, store.as_ref())),
        }
    }
    events
}

/// kimi-cli: `sessions/<md5(workdir)>/<session-id>/wire.jsonl`.
fn scan_cli_root(root: &Path, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let work_dirs = cli_work_dirs(root);
    let mut events = Vec::new();
    for file in wire_files(&root.join("sessions")) {
        let session_dir = file.parent().map(|p| p.to_path_buf());
        let session_id = session_dir
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string);
        let hash_dir = session_dir
            .as_ref()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let project = work_dirs.get(hash_dir).cloned().or_else(|| {
            prompt_work_dir(
                &session_dir
                    .as_deref()
                    .unwrap_or(&file)
                    .join("context.jsonl"),
            )
        });
        events.extend(scan_wire(&file, store, session_id, project));
    }
    events
}

/// kimi-code: `sessions/<workDirKey>/<sessionId>/agents/<agent>/wire.jsonl`.
fn scan_code_root(root: &Path, store: Option<&TokenStore>) -> Vec<TokenEvent> {
    let index = code_session_index(&root.join("session_index.jsonl"));
    let mut events = Vec::new();
    for file in wire_files(&root.join("sessions")) {
        // <sid>/agents/<agent>/wire.jsonl → session id is two levels up.
        let agents_dir = file.parent().and_then(|p| p.parent());
        let (session_id, work_dir_key) =
            match agents_dir.filter(|d| d.file_name().and_then(|n| n.to_str()) == Some("agents")) {
                Some(agents) => {
                    let session_dir = agents.parent();
                    (
                        session_dir
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .map(str::to_string),
                        session_dir
                            .and_then(|p| p.parent())
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or(""),
                    )
                }
                // Flat fallback: treat the parent dir as the session.
                None => (
                    file.parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(str::to_string),
                    "",
                ),
            };
        let project = session_id
            .as_deref()
            .and_then(|sid| index.get(sid))
            .cloned()
            .or_else(|| work_dir_slug(work_dir_key));
        events.extend(scan_wire(&file, store, session_id, project));
    }
    events
}

fn wire_files(root: &Path) -> Vec<PathBuf> {
    collect_files(std::slice::from_ref(&root.to_path_buf()), &["jsonl"])
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("wire.jsonl"))
        .collect()
}

/// One append-only wire.jsonl → events. `usage.record`/`StatusUpdate` are the
/// primary per-call records; `context.append_message.message.usage` is only
/// used when a file carries no primary records.
fn scan_wire(
    path: &Path,
    store: Option<&TokenStore>,
    session_id: Option<String>,
    project: Option<String>,
) -> Vec<TokenEvent> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let offset = util::resume_offset(store, path);
    let fallback_ts = file_mtime(path);
    let mut primary = Vec::new();
    let mut fallback = Vec::new();
    // Same append-only recipe as util::read_jsonl_events — duplicated here
    // because the primary/fallback split needs two output buckets.
    let mut pos = offset;
    if let Ok(mut file) = fs::File::open(path)
        && file.seek(SeekFrom::Start(offset)).is_ok()
    {
        let mut reader = BufReader::new(file);
        let mut buf = Vec::with_capacity(8192);
        loop {
            buf.clear();
            let n = match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if buf.last() != Some(&b'\n') {
                break; // trailing partial line — leave for the next scan
            }
            pos = pos.saturating_add(n as u64);
            let text = String::from_utf8_lossy(&buf[..n]);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(line) = serde_json::from_str::<Value>(trimmed) {
                wire_line_events(
                    &line,
                    &session_id,
                    &project,
                    fallback_ts,
                    &mut primary,
                    &mut fallback,
                );
            }
        }
    }
    util::mark_scanned(store, path, pos);
    if primary.is_empty() {
        fallback
    } else {
        primary
    }
}

/// Normalize both wire flavors to (type, payload, ts) and dispatch.
fn wire_line_events(
    line: &Value,
    session_id: &Option<String>,
    project: &Option<String>,
    fallback_ts: f64,
    primary: &mut Vec<TokenEvent>,
    fallback: &mut Vec<TokenEvent>,
) {
    // kimi-code flat records carry `type` at top level (`{type, time, ...}`);
    // kimi-cli wraps the record in `{timestamp, message:{type, payload}}`.
    // Check `type` first — a flat record like `context.append_message` also
    // carries a `message` field and must not be misread as the envelope.
    let (ty, payload, ts) = match line.get("type").and_then(Value::as_str) {
        Some(t) => (t, Some(line), wire_ts(line, fallback_ts)),
        None => match line.get("message").and_then(Value::as_object) {
            Some(msg) => (
                msg.get("type").and_then(Value::as_str).unwrap_or(""),
                msg.get("payload"),
                wire_ts(line, fallback_ts),
            ),
            None => ("", None, wire_ts(line, fallback_ts)),
        },
    };
    dispatch(ty, payload, ts, session_id, project, primary, fallback);
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    ty: &str,
    payload: Option<&Value>,
    ts: f64,
    session_id: &Option<String>,
    project: &Option<String>,
    primary: &mut Vec<TokenEvent>,
    fallback: &mut Vec<TokenEvent>,
) {
    let Some(payload) = payload else { return };
    match ty {
        // kimi-cli per-step usage.
        "StatusUpdate" => {
            if let Some(counts) = payload.get("token_usage").and_then(kimi_usage) {
                primary.push(mk_event(None, session_id, project, ts, counts));
            }
        }
        // kimi-cli nested subagent record: payload.event = {type, payload}.
        "SubagentEvent" => {
            if let Some(inner) = payload.get("event") {
                dispatch(
                    inner.get("type").and_then(Value::as_str).unwrap_or(""),
                    inner.get("payload"),
                    ts,
                    session_id,
                    project,
                    primary,
                    fallback,
                );
            }
        }
        // kimi-code per-call usage; `usageScope` marks the call's origin
        // (turn vs session-level like compaction), not a cumulative counter.
        "usage.record" => {
            if let Some(counts) = payload.get("usage").and_then(kimi_usage) {
                let model = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                primary.push(mk_event(model, session_id, project, ts, counts));
            }
        }
        // kimi-code fallback: message.usage on appended context messages.
        "context.append_message" => {
            if let Some(counts) =
                util::get_path(payload, &["message", "usage"]).and_then(kimi_usage)
            {
                let model = util::get_path(payload, &["message", "model"])
                    .and_then(Value::as_str)
                    .map(str::to_string);
                fallback.push(mk_event(model, session_id, project, ts, counts));
            }
        }
        // `subagent.completed.usage` is a cumulative rollup → never emit.
        _ => {}
    }
}

fn mk_event(
    model: Option<String>,
    session_id: &Option<String>,
    project: &Option<String>,
    ts: f64,
    counts: UsageCounts,
) -> TokenEvent {
    TokenEvent {
        provider: PROVIDER.into(),
        model,
        session_id: session_id.clone(),
        project: project.clone(),
        ts_unix: ts,
        input_tokens: counts.input,
        output_tokens: counts.output,
        cache_read_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
    }
}

/// Kimi TokenUsage in either spelling: kimi-cli snake_case
/// (`input_other`/`input_cache_read`/`input_cache_creation`) or kimi-code
/// camelCase (`inputOther`/`inputCacheRead`/`inputCacheCreation`). `None`
/// when no real token fields — events are never fabricated.
fn kimi_usage(usage: &Value) -> Option<UsageCounts> {
    let num = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| {
                usage
                    .get(*k)
                    .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
            })
            .unwrap_or(0)
    };
    let counts = UsageCounts {
        input: num(&["input_other", "inputOther"]),
        output: num(&["output"]),
        cache_read: num(&["input_cache_read", "inputCacheRead"]),
        cache_write: num(&["input_cache_creation", "inputCacheCreation"]),
    };
    (!counts.is_empty()).then_some(counts)
}

fn wire_ts(line: &Value, fallback: f64) -> f64 {
    for key in ["timestamp", "time"] {
        if let Some(ts) = line.get(key).and_then(coerce_unix_seconds) {
            return ts;
        }
    }
    fallback
}

/// kimi.json `work_dirs[]` → dir-name → work dir map. The dir name is
/// `md5(path)` on the local kaos, `<kaos>_<md5(path)>` elsewhere.
fn cli_work_dirs(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(meta) = util::read_json(&root.join("kimi.json")) else {
        return map;
    };
    for wd in meta
        .get("work_dirs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(path) = wd.get("path").and_then(Value::as_str) else {
            continue;
        };
        let digest = md5_hex(path.as_bytes());
        let key = match wd.get("kaos").and_then(Value::as_str) {
            Some(kaos) if kaos != "local" => format!("{kaos}_{digest}"),
            _ => digest,
        };
        map.insert(key, path.to_string());
    }
    map
}

/// Work dir fallback from the `_system_prompt` record, which renders
/// `The current working directory is `<path>`` (agents/default/system.md).
fn prompt_work_dir(context_file: &Path) -> Option<String> {
    let file = fs::File::open(context_file).ok()?;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut line).ok()?;
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("role").and_then(Value::as_str) != Some("_system_prompt") {
        return None;
    }
    let content = value.get("content").and_then(Value::as_str)?;
    let marker = "current working directory is `";
    let start = content.find(marker)? + marker.len();
    let end = content[start..].find('`')? + start;
    Some(content[start..end].to_string()).filter(|s| !s.is_empty())
}

/// kimi-code `session_index.jsonl` → sessionId → workDir. `{deleted:true}`
/// tombstones remove the mapping.
fn code_session_index(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(bytes) = fs::read(path) else {
        return map;
    };
    for raw in bytes.split(|b| *b == b'\n') {
        let text = String::from_utf8_lossy(raw);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(sid) = value.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        if value.get("deleted").and_then(Value::as_bool) == Some(true) {
            map.remove(sid);
        } else if let Some(wd) = value.get("workDir").and_then(Value::as_str) {
            map.insert(sid.to_string(), wd.to_string());
        }
    }
    map
}

/// `wd_<slug>_<sha256[:12]>` → the slug (a lowercased basename — a label,
/// not a real path).
fn work_dir_slug(key: &str) -> Option<String> {
    let rest = key.strip_prefix("wd_")?;
    let (slug, _) = rest.rsplit_once('_')?;
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Minimal MD5 (RFC 1321) — kimi-cli names each work dir's sessions dir by
/// `md5(workdir path)`, so resolving `project` needs the same digest.
fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usg-kimi-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn write(path: &Path, lines: &[&str]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn cli_wire_status_update_and_subagent() {
        let dir = fixture_dir("cli");
        let wd = "/work/proj";
        let hash = md5_hex(wd.as_bytes());
        write(
            &dir.join("kimi.json"),
            &[&format!(
                r#"{{"work_dirs":[{{"path":"{wd}","kaos":"local"}}]}}"#
            )],
        );
        let wire = dir.join(format!("sessions/{hash}/sess-1/wire.jsonl"));
        write(
            &wire,
            &[
                r#"{"type":"metadata","protocol_version":"1.10"}"#,
                r#"{"timestamp":1759300000.5,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":100,"output":40,"input_cache_read":20,"input_cache_creation":5}}}}"#,
                r#"{"timestamp":1759300001.0,"message":{"type":"SubagentEvent","payload":{"event":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":7,"output":3}}}}}}"#,
                r#"{"timestamp":1759300002.0,"message":{"type":"StatusUpdate","payload":{"context_tokens":999}}}"#,
                "not json",
            ],
        );
        let events = scan_cli_root(&dir, None);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[0].output_tokens, 40);
        assert_eq!(events[0].cache_read_tokens, 20);
        assert_eq!(events[0].cache_write_tokens, 5);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-1"));
        assert_eq!(events[0].project.as_deref(), Some("/work/proj"));
        assert!(events[0].model.is_none());
        assert_eq!(events[0].ts_unix, 1759300000.5);
        assert_eq!(events[1].input_tokens, 7);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_context_usage_is_not_emitted() {
        let dir = fixture_dir("cli-ctx");
        let session = dir.join("sessions/deadbeef/sess-9");
        write(
            &session.join("context.jsonl"),
            &[
                r#"{"role":"_system_prompt","content":"The current working directory is `/work/fallback`. More text."}"#,
                r#"{"role":"_usage","token_count":50000}"#,
            ],
        );
        write(
            &session.join("wire.jsonl"),
            &[
                r#"{"timestamp":1.0,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":1,"output":2}}}}"#,
            ],
        );
        let events = scan_cli_root(&dir, None);
        assert_eq!(events.len(), 1);
        // md5 lookup missed → system-prompt fallback resolves the project.
        assert_eq!(events[0].project.as_deref(), Some("/work/fallback"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_usage_record_flat_envelope() {
        let dir = fixture_dir("code");
        write(
            &dir.join("session_index.jsonl"),
            &[
                r#"{"sessionId":"s-1","sessionDir":"sessions/wd_myproj_0123456789ab/s-1","workDir":"/home/me/myproj"}"#,
                r#"{"sessionId":"s-gone","sessionDir":"sessions/wd_old_111111111111/s-gone","workDir":"/tmp/old"}"#,
                r#"{"sessionId":"s-gone","deleted":true}"#,
            ],
        );
        let wire = dir.join("sessions/wd_myproj_0123456789ab/s-1/agents/agent-1/wire.jsonl");
        write(
            &wire,
            &[
                r#"{"type":"metadata","protocol_version":"1.0","created_at":1759300000000}"#,
                r#"{"type":"usage.record","time":1759300001000,"agentId":"agent-1","model":"kimi-for-coding","usage":{"inputOther":64,"output":8,"inputCacheRead":4,"inputCacheCreation":2},"usageScope":"turn"}"#,
                // cumulative rollup — must not emit
                r#"{"type":"subagent.completed","time":1759300002000,"subagentId":"sub-1","usage":{"inputOther":1000,"output":500}}"#,
                r#"{"type":"usage.record","time":1759300003000,"agentId":"agent-1","model":"kimi-for-coding","usage":{"inputOther":10,"output":5}}"#,
            ],
        );
        let events = scan_code_root(&dir, None);
        assert_eq!(events.len(), 2, "cumulative subagent.completed skipped");
        assert_eq!(events[0].input_tokens, 64);
        assert_eq!(events[0].cache_read_tokens, 4);
        assert_eq!(events[0].cache_write_tokens, 2);
        assert_eq!(events[0].model.as_deref(), Some("kimi-for-coding"));
        assert_eq!(events[0].session_id.as_deref(), Some("s-1"));
        assert_eq!(events[0].project.as_deref(), Some("/home/me/myproj"));
        assert_eq!(events[0].ts_unix, 1759300001.0); // ms → s
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_append_message_fallback() {
        let dir = fixture_dir("code-fb");
        let wire = dir.join("sessions/wd_solo_aaaaaaaaaaaa/s-2/agents/main/wire.jsonl");
        write(
            &wire,
            &[
                r#"{"type":"context.append_message","time":1759300000000,"agentId":"main","message":{"role":"assistant","usage":{"inputOther":11,"output":6}}}"#,
            ],
        );
        let events = scan_code_root(&dir, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 11);
        assert_eq!(events[0].project.as_deref(), Some("solo")); // slug fallback
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dirs_and_bad_lines() {
        let dir = fixture_dir("empty");
        assert!(scan_cli_root(&dir, None).is_empty());
        assert!(scan_code_root(&dir, None).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5_hex(b"/Users/me/project"), md5_hex(b"/Users/me/project"));
    }
}
