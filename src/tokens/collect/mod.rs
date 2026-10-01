//! Per-provider local file scanners.
//!
//! Each scanner module exposes `scan() -> Vec<TokenEvent>` and
//! `scan_roots() -> Vec<PathBuf>`. Scanners only read files the agents write
//! themselves; they never manufacture usage — a provider whose files carry no
//! token fields yields zero events.
//!
//! `claude` and `codex` are owned by the core token-ledger PR; their stubs
//! stay empty here until that lands.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod gemini;
pub mod grok;
mod util;

use std::path::PathBuf;

use crate::tokens::TokenEvent;

/// Every provider id with a local scanner, in stable order.
pub fn known_providers() -> &'static [&'static str] {
    &["claude", "codex", "grok", "gemini", "antigravity", "cursor"]
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
        _ => Vec::new(),
    }
}
