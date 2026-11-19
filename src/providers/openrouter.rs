//! OpenRouter prepaid credit balance (`GET /api/v1/credits`).
//!
//! Reads `OPENROUTER_API_KEY` or `~/.config/openrouter/api-key` / `~/.openrouter/api-key`.
//! `OPENROUTER_API_URL` overrides the credits endpoint for testing.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, coerce_number, create_meter};

const ID: &str = "openrouter";
const LABEL: &str = "OpenRouter";
const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";

pub fn key_paths() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    vec![
        cfg.join("openrouter").join("api-key"),
        home.join(".openrouter").join("api-key"),
    ]
}

pub fn resolve_api_key() -> Option<String> {
    if let Ok(v) = std::env::var("OPENROUTER_API_KEY") {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    for path in key_paths() {
        if let Ok(raw) = fs::read_to_string(&path) {
            let t = raw.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    let Some(key) = resolve_api_key() else {
        return ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            "OPENROUTER_API_KEY unset and no openrouter api-key file",
        );
    };
    let url = std::env::var("OPENROUTER_API_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| CREDITS_URL.to_string());
    let bearer = format!("Bearer {key}");
    match client.get_json::<Value>(&url, &[("Authorization", bearer.as_str())]) {
        Ok(body) => snapshot_from_credits(&body),
        Err(e) if e.is_auth_error() => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            "OpenRouter rejected the API key",
        ),
        Err(HttpError::Status {
            status, message, ..
        }) => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            format!("OpenRouter HTTP {status}: {message}"),
        ),
        Err(e) => ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Error, e.to_string()),
    }
}

pub(crate) fn snapshot_from_credits(body: &Value) -> ProviderSnapshot {
    let data = body.get("data").unwrap_or(body);
    let total_credits = data.get("total_credits").and_then(coerce_number);
    let total_usage = data.get("total_usage").and_then(coerce_number);

    let mut snap = ProviderSnapshot::ok(ID, LABEL);
    match (total_credits, total_usage) {
        (Some(limit), Some(used)) => {
            let left = (limit - used).max(0.0);
            snap.meters.push(create_meter(
                "credits",
                "Credits",
                None,
                None,
                Some(used),
                Some(left),
                (limit > 0.0).then_some(limit),
                "usd",
                None,
                None,
                None,
            ));
        }
        (Some(limit), None) => {
            snap.meters.push(create_meter(
                "credits",
                "Credits",
                None,
                None,
                None,
                Some(limit.max(0.0)),
                None,
                "usd",
                None,
                None,
                None,
            ));
        }
        _ => {
            snap.error = Some("OpenRouter returned no credit balance fields".into());
        }
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openrouter_credits_payload() {
        let snap = snapshot_from_credits(&json!({
            "data": { "total_credits": 25.0, "total_usage": 5.0 }
        }));
        assert_eq!(snap.status, SnapshotStatus::Ok);
        assert_eq!(snap.meters.len(), 1);
        let m = &snap.meters[0];
        assert_eq!(m.unit, "usd");
        assert!((m.used.unwrap() - 5.0).abs() < 1e-9);
        assert!((m.left.unwrap() - 20.0).abs() < 1e-9);
        assert!((m.percent.unwrap() - 0.2).abs() < 1e-9);
        assert!((m.left_percent.unwrap() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn empty_credits_payload_reports_note() {
        let snap = snapshot_from_credits(&json!({}));
        assert!(snap.meters.is_empty());
        assert!(snap.error.is_some());
    }
}
