//! Shared helpers for file-based token scanners. Everything here is
//! read-only and failure-tolerant by contract: a bad file, line, or directory
//! yields nothing rather than failing the scan.

use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::providers::types::coerce_unix_seconds;
use crate::tokens::{TokenEvent, TokenStore};

/// Keep scans bounded on machines with large agent data dirs.
const MAX_FILES: usize = 4096;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Token buckets extracted from a usage object, additive by convention:
/// `input`/`output` exclude nested cache/reasoning subsets when the record
/// reports an inclusive total.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UsageCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl UsageCounts {
    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0
    }
}

fn u64_at(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| {
        value
            .get(*k)
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
    })
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

/// Extract token counts from a usage-shaped JSON object. Recognizes the
/// field spellings agents actually persist:
///
/// - Grok Build JSON-RPC usage: `inputTokens`, `outputTokens`,
///   `cachedReadTokens`, `cachedWriteTokens`, `reasoningTokens`,
///   `totalTokens`
/// - Anthropic-style usage: `input_tokens`, `output_tokens`,
///   `cache_read_input_tokens`, `cache_creation_input_tokens`
/// - OpenAI-style usage: `prompt_tokens`, `completion_tokens`,
///   `prompt_tokens_details.cached_tokens`
/// - Gemini `usageMetadata` / recorded `tokens`: `promptTokenCount`,
///   `candidatesTokenCount`, `cachedContentTokenCount`,
///   `thoughtsTokenCount`, `toolUsePromptTokenCount`, or the short forms
///   `input`/`output`/`cached`/`thoughts`/`tool`/`total`
/// - Cursor bubbles: `tokenCount` → `inputTokens`/`outputTokens`
///
/// Returns `None` when no known token field is present, so callers never
/// emit fabricated events.
pub fn usage_counts(usage: &Value) -> Option<UsageCounts> {
    if !usage.is_object() {
        return None;
    }
    let input = u64_at(
        usage,
        &[
            "input",
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
            "promptTokenCount",
        ],
    )
    .unwrap_or(0);
    let output = u64_at(
        usage,
        &[
            "output",
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
            "candidatesTokenCount",
        ],
    )
    .unwrap_or(0);
    let cache_read = u64_at(
        usage,
        &[
            "cached",
            "cache_read",
            "cached_tokens",
            "cachedTokens",
            "cachedReadTokens",
            "cacheReadTokens",
            "cache_read_input_tokens",
            "cachedContentTokenCount",
        ],
    )
    .unwrap_or(0)
        + usage
            .get("prompt_tokens_details")
            .and_then(|d| u64_at(d, &["cached_tokens"]))
            .unwrap_or(0);
    let cache_write = u64_at(
        usage,
        &[
            "cache_write",
            "cachedWriteTokens",
            "cacheWriteTokens",
            "cacheCreationTokens",
            "cache_creation_input_tokens",
        ],
    )
    .unwrap_or(0);
    let reasoning = u64_at(
        usage,
        &[
            "thoughts",
            "reasoning",
            "reasoning_tokens",
            "reasoningTokens",
            "thoughtsTokenCount",
            "thoughtTokens",
            "thinkingTokens",
        ],
    )
    .unwrap_or(0);
    // Input-side extras some formats report separately.
    let tool_input =
        u64_at(usage, &["tool", "tool_tokens", "toolUsePromptTokenCount"]).unwrap_or(0);
    let total = u64_at(
        usage,
        &["total", "total_tokens", "totalTokens", "totalTokenCount"],
    );

    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 && reasoning == 0 {
        return None;
    }

    // When the record's total equals input+output, those fields are
    // inclusive of their nested cache/reasoning subsets: move cache reads
    // into their own bucket, while reasoning stays inside output (every
    // current provider bills it as output and the schema has no reasoning
    // bucket).
    let inclusive = total == Some(input.saturating_add(output));
    let counts = UsageCounts {
        input: if inclusive {
            input.saturating_sub(cache_read)
        } else {
            input
        }
        .saturating_add(tool_input),
        output: if total.is_none() {
            output.saturating_add(reasoning)
        } else {
            output
        },
        cache_read,
        cache_write,
    };
    if counts.is_empty() {
        return None;
    }
    Some(counts)
}

/// Locations where a usage object commonly sits on a usage-bearing record.
pub const USAGE_PATHS: &[&[&str]] = &[
    &["usage"],
    &["tokenUsage"],
    &["token_usage"],
    &["usageMetadata"],
    &["tokenCount"],
    &["tokens"],
    &["params", "update", "usage"],
    &["params", "usage"],
    &["message", "usage"],
    &["response", "usage"],
    &["result", "usage"],
];

pub fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    Some(cur)
}

/// First non-empty usage object on a record, or `None` when the record
/// carries no token fields at all.
pub fn usage_on_record(record: &Value) -> Option<UsageCounts> {
    for path in USAGE_PATHS {
        if let Some(usage) = get_path(record, path)
            && let Some(counts) = usage_counts(usage)
        {
            return Some(counts);
        }
    }
    None
}

/// Field spellings agents use for the owning model of a usage record.
pub fn model_on_record(record: &Value) -> Option<String> {
    str_at(
        record,
        &[
            "model",
            "modelId",
            "model_id",
            "primaryModelId",
            "modelType",
        ],
    )
    .or_else(|| {
        record
            .get("modelsUsed")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })
    .or_else(|| {
        get_path(record, &["params", "update", "modelId"])
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })
}

/// Field spellings for a session/conversation identifier.
pub fn session_on_record(record: &Value) -> Option<String> {
    str_at(
        record,
        &[
            "sessionId",
            "session_id",
            "conversationId",
            "composerId",
            "generationUUID",
            "bubbleId",
        ],
    )
}

/// Timestamp fields seen in agent logs; returns unix seconds.
/// Falls back to `coerce_unix_seconds` semantics (epoch s/ms, RFC3339).
pub fn ts_on_record(record: &Value) -> Option<f64> {
    for key in [
        "timestamp",
        "ts",
        "time",
        "createdAt",
        "created_at",
        "unixMs",
        "timestampMs",
        "datetime",
        "date",
        "lastUpdatedAt",
    ] {
        if let Some(v) = record.get(key)
            && let Some(ts) = coerce_unix_seconds(v)
        {
            return Some(ts);
        }
    }
    get_path(record, &["params", "update", "timestamp"]).and_then(coerce_unix_seconds)
}

/// Recursively collect `*.json` / `*.jsonl` / `*.vscdb`-style files under
/// `roots`, bounded by count and size. Unreadable entries are skipped.
pub fn collect_files(roots: &[PathBuf], exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = roots.to_vec();
    while let Some(dir) = stack.pop() {
        if out.len() >= MAX_FILES {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
                .unwrap_or(false);
            if ext_ok {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

pub fn file_mtime(path: &Path) -> f64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Read a JSONL file starting at `offset`. Complete lines only: a trailing
/// partial line (no `\n` before EOF) is left unconsumed so the next scan can
/// read it once the writer finishes it. Returns (events, next_offset).
///
/// `parse` maps one parsed JSON record to zero or more events.
pub fn read_jsonl_events<F>(path: &Path, offset: u64, parse: F) -> (Vec<TokenEvent>, u64)
where
    F: Fn(&Value) -> Vec<TokenEvent>,
{
    let mut events = Vec::new();
    let Ok(mut file) = fs::File::open(path) else {
        return (events, offset);
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (events, offset);
    }
    let mut pos = offset;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::with_capacity(8192);
    loop {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let complete = buf.last() == Some(&b'\n');
        if !complete {
            break; // trailing partial line — leave for the next scan
        }
        let line = &buf[..n];
        pos = pos.saturating_add(n as u64);
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let text = String::from_utf8_lossy(line);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            events.extend(parse(&value));
        }
    }
    (events, pos)
}

/// Resume position for append-only files: stored byte offset, reset to 0 when
/// the file shrank (truncated/rotated).
pub fn resume_offset(store: Option<&TokenStore>, path: &Path) -> u64 {
    let Some(store) = store else {
        return 0;
    };
    let Ok(Some((offset, _))) = store.scan_offset(path) else {
        return 0;
    };
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < offset { 0 } else { offset }
}

/// Skip whole-file formats already scanned when size and mtime are unchanged.
pub fn whole_file_changed(store: Option<&TokenStore>, path: &Path) -> bool {
    let Some(store) = store else {
        return true;
    };
    let Ok(Some((offset, mtime))) = store.scan_offset(path) else {
        return true;
    };
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let cur_mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    meta.len() != offset || cur_mtime != mtime
}

pub fn mark_scanned(store: Option<&TokenStore>, path: &Path, offset: u64) {
    if let Some(store) = store {
        store.set_scan_offset(path, offset, file_mtime(path));
    }
}

/// Percent-decode a path-ish string (`%2F` → `/`), used by Grok workspace keys.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read a small JSON file; `None` on any failure.
pub fn read_json(path: &Path) -> Option<Value> {
    let mut file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}
