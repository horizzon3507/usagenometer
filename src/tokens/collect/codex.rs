//! Codex CLI rollout scanner — owned by the core token-ledger PR.
//!
//! Stub: emits no events until that implementation lands.

use std::path::PathBuf;

use crate::tokens::TokenEvent;

pub fn scan() -> Vec<TokenEvent> {
    Vec::new()
}

pub fn scan_roots() -> Vec<PathBuf> {
    vec![super::util::home().join(".codex").join("sessions")]
}
