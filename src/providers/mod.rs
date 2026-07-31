//! Provider fetch orchestration + short cache / history side effects.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod types;

use crate::cache::SnapshotCache;
use crate::cli::ProviderArg;
use crate::history::HistoryStore;
use crate::http::HttpClient;
use types::{ProviderSnapshot, SnapshotStatus};

pub fn provider_label(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "cursor" => "Cursor",
        "antigravity" => "Antigravity",
        "claude" => "Claude",
        "grok" => "Grok",
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
    fetch_all_cached(filter, 0, false)
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
) -> Vec<ProviderSnapshot> {
    let ids = resolve_providers(filter);
    let client = HttpClient::new().ok();
    let cache = SnapshotCache::new(cache_ttl.max(1));
    let history = if record_history {
        HistoryStore::open().ok()
    } else {
        None
    };

    let _ = cache_ttl; // reserved: max age for accepting stale fallback
    ids.into_iter()
        .map(|id| {
            let mut snap = fetch_one(id, client.as_ref());
            if snap.status == SnapshotStatus::Ok {
                snap.stale_age_secs = None;
                let _ = cache.save(&snap);
                if let Some(h) = history.as_ref() {
                    let _ = h.record(&snap);
                }
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

pub fn fetch_one(id: &str, client: Option<&HttpClient>) -> ProviderSnapshot {
    match id {
        "codex" => match client {
            Some(c) => codex::fetch(c),
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
        _ => ProviderSnapshot::fail(id, provider_label(id), SnapshotStatus::Error, "Unknown provider"),
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
                snap.error
                    .clone()
                    .unwrap_or_else(|| "Connection OK".into())
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
