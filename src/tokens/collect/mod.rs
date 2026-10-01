//! Per-agent local file scanners. All scanning is local-only, tolerant, and
//! cheap: missing dirs yield zero events, malformed lines are skipped, and a
//! bad file never fails the whole scan.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod droid;
pub mod gemini;
pub mod glm;
pub mod grok;
pub mod opencode;
pub mod omp;
pub mod pi;
mod util;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::tokens::{TokenEvent, TokenStore};

/// Every provider id with a local scanner, in stable order.
pub fn known_providers() -> &'static [&'static str] {
    &[
        "claude",
        "codex",
        "grok",
        "gemini",
        "antigravity",
        "cursor",
        "opencode",
        "glm",
        "omp",
        "droid",
        "pi",
    ]
}

/// Parse the provider's local files into events (no persistence).
/// Unknown providers yield an empty vec.
pub fn scan_provider_files(provider: &str) -> Vec<TokenEvent> {
    match provider {
        "claude" => claude::scan(),
        "codex" => codex::scan(),
        "grok" => grok::scan(),
        "gemini" => gemini::scan(),
        "antigravity" => antigravity::scan(),
        "cursor" => cursor::scan(),
        "opencode" => opencode::scan(),
        "glm" => glm::scan(),
        "omp" => omp::scan(),
        "droid" => droid::scan(),
        "pi" => pi::scan(),
        _ => Vec::new(),
    }
}

/// Local roots a provider's scanner reads (for `usg doctor`).
pub fn scan_roots(provider: &str) -> Vec<PathBuf> {
    match provider {
        "claude" => claude::scan_roots(),
        "codex" => codex::scan_roots(),
        "grok" => grok::scan_roots(),
        "gemini" => gemini::scan_roots(),
        "antigravity" => antigravity::scan_roots(),
        "cursor" => cursor::scan_roots(),
        "opencode" => opencode::scan_roots(),
        "glm" => glm::scan_roots(),
        "omp" => omp::scan_roots(),
        "droid" => droid::scan_roots(),
        "pi" => pi::scan_roots(),
        _ => Vec::new(),
    }
}

/// Per-file byte offsets so repeat scans only read appended data.
/// Loaded from / flushed to the `token_scan_offsets` table; a full re-scan
/// (offset reset) stays dedupe-safe via `TokenEvent::event_hash`.
pub struct ScanOffsets {
    saved: HashMap<String, u64>,
    pending: HashMap<String, u64>,
}

impl ScanOffsets {
    pub(crate) fn load(store: &TokenStore) -> Self {
        Self {
            saved: store.scan_offsets().unwrap_or_default(),
            pending: HashMap::new(),
        }
    }

    /// Offsets that never persist — for tests and ad-hoc full scans.
    pub fn detached() -> Self {
        Self {
            saved: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    pub fn get(&self, path: &Path) -> u64 {
        let key = path.to_string_lossy();
        self.pending
            .get(key.as_ref())
            .or_else(|| self.saved.get(key.as_ref()))
            .copied()
            .unwrap_or(0)
    }

    pub fn set(&mut self, path: &Path, offset: u64) {
        self.pending
            .insert(path.to_string_lossy().into_owned(), offset);
    }

    pub(crate) fn flush(self, store: &TokenStore) {
        for (path, offset) in &self.pending {
            let _ = store.save_scan_offset(path, *offset);
        }
    }
}

/// Recursively list `*.jsonl` files under `root` (missing dir → empty).
pub(crate) fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Read complete (newline-terminated) lines appended since the last scan.
/// Advances the stored offset past the last full line so a partially-written
/// trailing line is picked up once the writer finishes it. A truncated or
/// rotated file (size < stored offset) is re-read from the start.
pub(crate) fn appended_lines(path: &Path, offsets: &mut ScanOffsets) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let len = meta.len();
    let mut start = offsets.get(path);
    if len < start {
        start = 0;
    }
    if len == start {
        return Vec::new();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return Vec::new();
    };
    offsets.set(path, start + last_nl as u64 + 1);
    String::from_utf8_lossy(&buf[..=last_nl])
        .lines()
        .map(str::to_string)
        .collect()
}

/// RFC3339 timestamp → unix seconds (fraction preserved); tolerant, None on junk.
pub(crate) fn parse_rfc3339(ts: &str) -> Option<f64> {
    time::OffsetDateTime::parse(ts.trim(), &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|dt| dt.unix_timestamp() as f64 + f64::from(dt.nanosecond()) / 1e9)
}

/// Project label: basename of the agent `cwd`, else derived from the Claude
/// projects-dir slug (`-Users-me-proj` → `proj`).
pub(crate) fn project_name(cwd: &str, slug: &str) -> Option<String> {
    if !cwd.is_empty()
        && let Some(name) = Path::new(cwd).file_name()
    {
        let name = name.to_string_lossy();
        if !name.is_empty() {
            return Some(name.into_owned());
        }
    }
    let derived = slug
        .trim_start_matches('-')
        .rsplit('-')
        .next()
        .unwrap_or("")
        .to_string();
    (!derived.is_empty()).then_some(derived)
}
