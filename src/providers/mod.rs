//! Provider fetch orchestration + short cache / history side effects.

pub mod antigravity;
pub mod availability;
pub mod claude;
pub mod codex;
pub mod commandcode;
pub mod cursor;
pub mod devin_cloud;
pub mod fx;
pub mod glm;
pub mod grok;
pub mod kimi;
pub mod opencode;
mod opencode_console;
pub mod types;

use crate::cache::SnapshotCache;
use crate::cli::ProviderArg;
use crate::history::HistoryStore;
use crate::http::HttpClient;
use types::{ProviderSnapshot, SnapshotStatus};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentSelection {
    #[default]
    Ready,
    IncludeUnconfigured,
    OnlyUnconfigured,
    All,
}

impl AgentSelection {
    pub fn matches(self, state: &availability::Availability) -> bool {
        match self {
            Self::Ready => state.ready(),
            Self::IncludeUnconfigured => state.installed,
            Self::OnlyUnconfigured => state.installed && !state.ready(),
            Self::All => true,
        }
    }
}

pub fn selected_ids(candidates: &[&'static str], selection: AgentSelection) -> Vec<&'static str> {
    if selection == AgentSelection::All {
        return candidates.to_vec();
    }
    candidates
        .iter()
        .copied()
        .filter(|id| selection.matches(&availability::inspect(id)))
        .collect()
}

pub fn selected_ledger_ids(filter: &[ProviderArg], selection: AgentSelection) -> Vec<&'static str> {
    let known = crate::tokens::collect::known_providers();
    let candidates: Vec<_> = if filter.is_empty() {
        known.to_vec()
    } else {
        filter
            .iter()
            .map(|p| p.id())
            .filter(|id| known.contains(id))
            .collect()
    };
    let mut ids = selected_ids(&candidates, selection);
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Honest provider contract for surfaces that need to explain a result instead
/// of implying that every provider exposes the same kind of quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub real_quota: bool,
    pub money_balance: bool,
    pub reset_windows: bool,
    pub local_history: bool,
    /// A working local token-ledger scanner ships for this provider
    /// (`src/tokens/collect/`). True only where the scanner emits real
    /// token fields — not where a parser stub exists.
    pub token_ledger: bool,
}

pub fn provider_capabilities(id: &str) -> ProviderCapabilities {
    match id {
        "codex" | "claude" => ProviderCapabilities {
            real_quota: true,
            money_balance: false,
            reset_windows: true,
            local_history: true,
            token_ledger: true,
        },
        "cursor" | "antigravity" => ProviderCapabilities {
            real_quota: true,
            money_balance: false,
            reset_windows: true,
            local_history: true,
            token_ledger: true,
        },
        "grok" => ProviderCapabilities {
            real_quota: true,
            money_balance: true,
            reset_windows: true,
            local_history: true,
            token_ledger: true,
        },
        "opencode-go" => ProviderCapabilities {
            real_quota: true,
            money_balance: false,
            reset_windows: true,
            local_history: true,
            token_ledger: false,
        },
        "opencode-zen" => ProviderCapabilities {
            real_quota: false,
            money_balance: true,
            reset_windows: false,
            local_history: true,
            token_ledger: false,
        },
        "opencode" | "gemini" => ProviderCapabilities {
            real_quota: false,
            money_balance: false,
            reset_windows: false,
            local_history: true,
            token_ledger: true,
        },
        "glm" => ProviderCapabilities {
            real_quota: true,
            money_balance: false,
            reset_windows: true,
            local_history: true,
            token_ledger: true,
        },
        "omp" => ProviderCapabilities {
            real_quota: false,
            money_balance: false,
            reset_windows: false,
            local_history: true,
            token_ledger: true,
        },
        // Factory's only documented usage endpoints are org-admin credit
        // limits behind a `fk-` API key — nothing the local CLI auth can read.
        "droid" => ProviderCapabilities {
            real_quota: false,
            money_balance: false,
            reset_windows: false,
            local_history: true,
            token_ledger: true,
        },
        // Command Code exposes credit balance plus five-hour / weekly spend
        // windows through the same alpha endpoints its `/usage` view uses.
        "commandcode" => ProviderCapabilities {
            real_quota: true,
            money_balance: true,
            reset_windows: true,
            local_history: true,
            token_ledger: true,
        },
        // Ledger-only providers (no quota surface, no ProviderArg).
        "pi" => ProviderCapabilities {
            real_quota: false,
            money_balance: false,
            reset_windows: false,
            local_history: true,
            token_ledger: true,
        },
        "kimi" => ProviderCapabilities {
            real_quota: true,
            money_balance: true,
            reset_windows: true,
            local_history: true,
            token_ledger: true,
        },
        "devin-local" => ProviderCapabilities {
            real_quota: false,
            money_balance: false,
            reset_windows: false,
            local_history: true,
            token_ledger: true,
        },
        // Devin bills in ACUs: a real org cycle cap exists when configured,
        // but the API exposes no token counts — no ledger.
        "devin-cloud" => ProviderCapabilities {
            real_quota: true,
            money_balance: false,
            reset_windows: false,
            local_history: false,
            token_ledger: false,
        },
        // fx: AI Gateway credit balance (no capped window, no resets) plus
        // its own local usage ledger (`~/.fx/usage.jsonl`).
        "fx" => ProviderCapabilities {
            real_quota: false,
            money_balance: true,
            reset_windows: false,
            local_history: true,
            token_ledger: true,
        },
        _ => ProviderCapabilities {
            real_quota: false,
            money_balance: false,
            reset_windows: false,
            local_history: false,
            token_ledger: false,
        },
    }
}

pub fn provider_label(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "cursor" => "Cursor",
        "antigravity" => "Antigravity",
        "claude" => "Claude",
        "grok" => "Grok",
        "opencode" => "OpenCode",
        "opencode-go" => "OpenCode Go",
        "opencode-zen" => "OpenCode Zen",
        "glm" => "GLM",
        "omp" => "OMP",
        "droid" => "Droid",
        "pi" => "Pi",
        "kimi" => "Kimi",
        "commandcode" => "CommandCode",
        "devin-local" => "Devin (local)",
        "devin-cloud" => "Devin Cloud",
        "fx" => "fx",
        "gemini" => "Gemini",
        _ => "Unknown",
    }
}

pub fn resolve_providers(filter: &[ProviderArg]) -> Vec<&'static str> {
    if filter.is_empty() {
        ProviderArg::all().iter().map(|p| p.id()).collect()
    } else {
        filter.iter().map(|p| p.id()).collect()
    }
}

pub fn fetch_all(filter: &[ProviderArg]) -> Vec<ProviderSnapshot> {
    fetch_all_cached(filter, 0, false, AgentSelection::Ready)
}

/// Fetch providers with optional short TTL cache + history persistence.
///
/// - On success: save cache + optionally record history.
/// - On failure: if a cached snapshot exists, return it marked `(stale)`.
/// - `cache_ttl == 0` disables serving fresh cache (still writes on success for stale fallback).
pub fn fetch_all_cached(
    filter: &[ProviderArg],
    cache_ttl: u64,
    record_history: bool,
    selection: AgentSelection,
) -> Vec<ProviderSnapshot> {
    let candidates = if filter.is_empty() && selection == AgentSelection::All {
        ProviderArg::catalog().iter().map(|p| p.id()).collect()
    } else {
        resolve_providers(filter)
    };
    let ids = selected_ids(&candidates, selection);
    let client = HttpClient::new().ok();
    let cache = SnapshotCache::new(cache_ttl.max(1));
    let history = if record_history {
        HistoryStore::open().ok()
    } else {
        None
    };

    ids.into_iter()
        .map(|id| {
            // Prompt/statusline and scripted consumers must not open five
            // network connections for every render. A fresh cache is a valid
            // snapshot; a cache after a failed fetch remains explicitly stale.
            if cache_ttl > 0 {
                if let Some(snap) = cache.load_fresh(id) {
                    return snap;
                }
            }
            let mut snap = fetch_one(id, client.as_ref());
            if snap.status == SnapshotStatus::Ok {
                snap.stale_age_secs = None;
                let _ = cache.save(&snap);
                if let Some(h) = history.as_ref() {
                    let _ = h.record(&snap);
                }
                snap
            } else if snap.status == SnapshotStatus::Auth {
                let _ = cache.invalidate(id);
                snap
            } else {
                // Prefer last-good meters over a bare error when cache exists.
                // Accept stale up to max(ttl, 24h) so a short TTL still helps briefly.
                let max_stale = cache_ttl.max(60).saturating_mul(48).max(3600);
                if let Some((cached, age)) = cache.load(id)
                    && age <= max_stale
                {
                    let mut s = cached;
                    s.status = SnapshotStatus::Ok;
                    s.stale_age_secs = Some(age);
                    if let Some(err) = snap.error.clone() {
                        s.error = Some(format!("stale fallback ({err})"));
                    }
                    s
                } else {
                    snap
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_contracts_do_not_claim_unknown_quota() {
        assert!(provider_capabilities("codex").real_quota);
        assert!(provider_capabilities("commandcode").real_quota);
        assert!(provider_capabilities("commandcode").money_balance);
        assert!(provider_capabilities("commandcode").token_ledger);
        assert!(provider_capabilities("grok").money_balance);
        assert!(!provider_capabilities("unknown").real_quota);
    }
}

pub fn fetch_one(id: &str, client: Option<&HttpClient>) -> ProviderSnapshot {
    match id {
        "codex" => match client {
            Some(c) => codex::fetch(c),
            None => http_unavailable(id),
        },
        "commandcode" => match client {
            Some(c) => commandcode::fetch(c),
            None => http_unavailable(id),
        },
        "cursor" => match client {
            Some(c) => cursor::fetch(c),
            None => http_unavailable(id),
        },
        "antigravity" => match client {
            Some(c) => antigravity::fetch(c),
            None => http_unavailable(id),
        },
        "claude" => match client {
            Some(c) => claude::fetch(c),
            None => http_unavailable(id),
        },
        "grok" => match client {
            Some(c) => grok::fetch(c),
            None => http_unavailable(id),
        },
        "glm" => match client {
            Some(c) => glm::fetch(c),
            None => http_unavailable(id),
        },
        "kimi" => match client {
            Some(c) => kimi::fetch(c),
            None => http_unavailable(id),
        },
        "devin-cloud" => match client {
            Some(c) => devin_cloud::fetch(c),
            None => http_unavailable(id),
        },
        "fx" => match client {
            Some(c) => fx::fetch(c),
            None => http_unavailable(id),
        },
        "opencode-go" => match client {
            Some(c) => opencode::fetch_go(c),
            None => http_unavailable(id),
        },
        "opencode-zen" => match client {
            Some(c) => opencode::fetch_zen(c),
            None => http_unavailable(id),
        },
        "omp" | "opencode" | "droid" | "pi" | "devin-local" | "gemini" => {
            let mut snap = ProviderSnapshot::ok(id, provider_label(id));
            snap.error = Some(format!("token ledger only — see usg tokens -p {id}"));
            snap
        }
        _ => ProviderSnapshot::fail(
            id,
            provider_label(id),
            SnapshotStatus::Error,
            "Unknown provider",
        ),
    }
}

fn http_unavailable(id: &str) -> ProviderSnapshot {
    ProviderSnapshot::fail(
        id,
        provider_label(id),
        SnapshotStatus::Error,
        "HTTP client unavailable",
    )
}

pub fn test_provider(id: &str) -> (bool, String, ProviderSnapshot) {
    let client = HttpClient::new().ok();
    let snap = fetch_one(id, client.as_ref());
    match snap.status {
        SnapshotStatus::Ok => {
            let msg = if let Some(account) = snap.account.as_deref() {
                format!("Connected as {account}")
            } else if !snap.meters.is_empty() {
                format!("Connected · {} meter(s)", snap.meters.len())
            } else {
                snap.error.clone().unwrap_or_else(|| "Connection OK".into())
            };
            (true, msg, snap)
        }
        _ => (
            false,
            snap.error
                .clone()
                .unwrap_or_else(|| format!("{} failed", provider_label(id))),
            snap,
        ),
    }
}
