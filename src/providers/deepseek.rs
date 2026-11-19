//! DeepSeek API balance (`GET /user/balance`).
//!
//! Reads `DEEPSEEK_API_KEY` or `~/.config/deepseek/api-key` / `~/.deepseek/api-key`.
//! `DEEPSEEK_API_URL` overrides the balance endpoint for testing.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, coerce_number, create_meter};

const ID: &str = "deepseek";
const LABEL: &str = "DeepSeek";
const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

pub fn key_paths() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cfg = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    vec![
        cfg.join("deepseek").join("api-key"),
        home.join(".deepseek").join("api-key"),
    ]
}

pub fn resolve_api_key() -> Option<String> {
    if let Ok(v) = std::env::var("DEEPSEEK_API_KEY") {
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
            "DEEPSEEK_API_KEY unset and no deepseek api-key file",
        );
    };
    let url = std::env::var("DEEPSEEK_API_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| BALANCE_URL.to_string());
    let bearer = format!("Bearer {key}");
    match client.get_json::<Value>(&url, &[("Authorization", bearer.as_str())]) {
        Ok(body) => snapshot_from_balance(&body),
        Err(e) if e.is_auth_error() => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            "DeepSeek rejected the API key",
        ),
        Err(HttpError::Status {
            status, message, ..
        }) => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            format!("DeepSeek HTTP {status}: {message}"),
        ),
        Err(e) => ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Error, e.to_string()),
    }
}

pub(crate) fn snapshot_from_balance(body: &Value) -> ProviderSnapshot {
    let mut snap = ProviderSnapshot::ok(ID, LABEL);
    let Some(infos) = body.get("balance_infos").and_then(Value::as_array) else {
        snap.error = Some("DeepSeek returned no balance_infos".into());
        return snap;
    };
    let chosen = infos
        .iter()
        .find(|b| {
            b.get("currency")
                .and_then(Value::as_str)
                .is_some_and(|c| c.eq_ignore_ascii_case("USD"))
        })
        .or_else(|| infos.first());

    let Some(info) = chosen else {
        snap.error = Some("DeepSeek balance_infos list is empty".into());
        return snap;
    };

    let total = info.get("total_balance").and_then(coerce_number);
    let currency = info
        .get("currency")
        .and_then(Value::as_str)
        .unwrap_or("USD");
    let unit = if currency.eq_ignore_ascii_case("USD") {
        "usd".to_string()
    } else {
        currency.to_ascii_lowercase()
    };

    if let Some(left) = total {
        snap.meters.push(create_meter(
            "balance",
            "Balance",
            None,
            None,
            None,
            Some(left.max(0.0)),
            None,
            &unit,
            None,
            None,
            None,
        ));
    } else {
        snap.error = Some("DeepSeek balance entry missing total_balance".into());
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_usd_balance_entry() {
        let snap = snapshot_from_balance(&json!({
            "is_available": true,
            "balance_infos": [
                { "currency": "CNY", "total_balance": "50.00" },
                { "currency": "USD", "total_balance": "12.34" }
            ]
        }));
        assert_eq!(snap.status, SnapshotStatus::Ok);
        assert_eq!(snap.meters.len(), 1);
        let m = &snap.meters[0];
        assert_eq!(m.unit, "usd");
        assert!((m.left.unwrap() - 12.34).abs() < 1e-9);
    }

    #[test]
    fn empty_balance_infos_reports_note() {
        let snap = snapshot_from_balance(&json!({ "balance_infos": [] }));
        assert!(snap.meters.is_empty());
        assert!(snap.error.is_some());
    }
}
