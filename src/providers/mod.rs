pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod types;

use crate::cli::ProviderArg;
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
    let ids = resolve_providers(filter);
    let client = HttpClient::new().ok();
    ids.into_iter()
        .map(|id| fetch_one(id, client.as_ref()))
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
