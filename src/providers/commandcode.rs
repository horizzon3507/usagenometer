//! Command Code credits and quota windows.
//!
//! Credentials: `COMMANDCODE_API_KEY`, else the `apiKey` in
//! `~/.commandcode/auth.json` (written by `commandcode login`). The base URL
//! honors `COMMANDCODE_API_URL` the same way the CLI does, defaulting to
//! `https://api.commandcode.ai`.
//!
//! Endpoints used by the CLI for `/usage`:
//! - `GET /alpha/whoami?limits=1` — account and optional organization id.
//! - `GET /alpha/billing/credits?orgId=` — remaining credits and the
//!   five-hour / weekly spend windows `{used, cap, resetAt}`.
//! - `GET /alpha/billing/subscriptions?orgId=` — `planId`, period start/end.
//!
//! Credits are USD-denominated; the windows are spend caps, so they become
//! percentage meters without inventing a reset or cap that the API omitted.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_number, coerce_unix_seconds, create_meter,
};

const ID: &str = "commandcode";
const LABEL: &str = "CommandCode";
const DEFAULT_BASE: &str = "https://api.commandcode.ai";

pub fn auth_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".commandcode")
        .join("auth.json")
}

pub fn api_key() -> Option<String> {
    if let Some(key) = std::env::var("COMMANDCODE_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(key);
    }
    let raw = fs::read_to_string(auth_path()).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    value
        .get("apiKey")
        .or_else(|| value.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

fn base_url() -> String {
    std::env::var("COMMANDCODE_API_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    let Some(key) = api_key() else {
        return ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            "no Command Code credential — run commandcode login or set COMMANDCODE_API_KEY",
        );
    };
    let base = base_url();
    let bearer = format!("Bearer {key}");
    let auth = [("Authorization", bearer.as_str())];

    let whoami = match client.get_json::<Value>(&format!("{base}/alpha/whoami?limits=1"), &auth) {
        Ok(body) => body,
        Err(error) => return http_failure(error),
    };
    let account = whoami
        .pointer("/user/userName")
        .or_else(|| whoami.pointer("/user/name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let org = whoami
        .pointer("/org/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let query = org
        .as_deref()
        .map(|id| format!("?orgId={id}"))
        .unwrap_or_default();

    let credits =
        match client.get_json::<Value>(&format!("{base}/alpha/billing/credits{query}"), &auth) {
            Ok(body) => body,
            Err(error) => return http_failure(error),
        };

    let subscription = client
        .get_json::<Value>(&format!("{base}/alpha/billing/subscriptions{query}"), &auth)
        .ok();

    let mut snap = ProviderSnapshot::ok(ID, LABEL);
    snap.account = account;
    snap.plan = subscription
        .as_ref()
        .and_then(|body| body.pointer("/data/planId"))
        .and_then(Value::as_str)
        .map(plan_label);

    let mut meters = Vec::new();
    if let Some(balance) = credit_balance(&credits) {
        meters.push(create_meter(
            "credits",
            "Credits",
            None,
            None,
            None,
            Some(balance),
            None,
            "usd",
            None,
            None,
            None,
        ));
    }
    if let Some(meter) = window_meter(
        "five-hour",
        "5 hour usage limit",
        credits.pointer("/windowLimits/fiveHour"),
        Some(5.0 * 3600.0),
    ) {
        meters.push(meter);
    }
    if let Some(meter) = window_meter(
        "weekly",
        "Weekly usage limit",
        credits.pointer("/windowLimits/weekly"),
        Some(7.0 * 86_400.0),
    ) {
        meters.push(meter);
    }
    if meters.is_empty() {
        snap.error = Some("Command Code returned no credit balance or usage windows".into());
    }
    snap.meters = meters;
    snap
}

fn http_failure(error: HttpError) -> ProviderSnapshot {
    if error.is_auth_error() {
        ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            "Command Code rejected the credential — run commandcode login",
        )
    } else if let HttpError::Status { status, .. } = error {
        ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            format!("Command Code usage HTTP {status}"),
        )
    } else {
        ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Error, error.to_string())
    }
}

/// Remaining credits = monthly + purchased + free.
fn credit_balance(body: &Value) -> Option<f64> {
    let credits = body.get("credits")?;
    let parts = ["monthlyCredits", "purchasedCredits", "freeCredits"]
        .iter()
        .filter_map(|key| credits.get(*key).and_then(coerce_number))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().sum())
}

fn window_meter(
    id: &str,
    title: &str,
    window: Option<&Value>,
    window_seconds: Option<f64>,
) -> Option<crate::providers::types::UsageMeter> {
    let window = window?;
    let used = window.get("used").and_then(coerce_number)?;
    let cap = window.get("cap").and_then(coerce_number)?;
    if cap <= 0.0 {
        return None;
    }
    let reset_at = window.get("resetAt").and_then(coerce_unix_seconds);
    Some(create_meter(
        id,
        title,
        None,
        None,
        Some(used),
        None,
        Some(cap),
        "usd",
        reset_at,
        None,
        window_seconds,
    ))
}

/// `individual-goat` → `GOAT`, `individual-pro` → `Pro`, …
fn plan_label(plan_id: &str) -> String {
    let normalized = plan_id.to_ascii_lowercase().replace('_', "-");
    let stem = normalized.strip_suffix("-v1").unwrap_or(&normalized);
    match stem {
        "individual-goat" => "GOAT",
        "individual-go" => "Go",
        "individual-provider" => "Provider",
        "individual-pro" => "Pro",
        "individual-max" => "Max",
        "individual-ultra" => "Ultra",
        "teams-pro" => "Teams Pro",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credits_and_windows_become_meters() {
        let credits = json!({
            "credits": {"monthlyCredits": 33.25, "purchasedCredits": 1.0, "freeCredits": 0.5},
            "windowLimits": {
                "fiveHour": {"used": 4.0, "cap": 14.0, "resetAt": 1_792_725_252_903_i64},
                "weekly": {"used": 4.0, "cap": 35.0, "resetAt": 1_793_312_052_903_i64}
            }
        });
        let balance = credit_balance(&credits).unwrap();
        assert!((balance - 34.75).abs() < 1e-9);
        let five = window_meter(
            "five-hour",
            "5 hour usage limit",
            credits.pointer("/windowLimits/fiveHour"),
            None,
        )
        .unwrap();
        assert_eq!(five.used, Some(4.0));
        assert_eq!(five.limit, Some(14.0));
        assert!((five.percent.unwrap() - 4.0 / 14.0).abs() < 1e-9);
        assert_eq!(five.reset_at, Some(1_792_725_252.903));
        assert_eq!(five.unit, "usd");

        let weekly = window_meter(
            "weekly",
            "Weekly usage limit",
            credits.pointer("/windowLimits/weekly"),
            None,
        )
        .unwrap();
        assert!((weekly.left.unwrap() - 31.0).abs() < 1e-9);
    }

    #[test]
    fn missing_or_zero_windows_are_skipped() {
        assert!(credit_balance(&json!({})).is_none());
        assert!(window_meter("w", "W", None, None).is_none());
        assert!(window_meter("w", "W", Some(&json!({"used": 1, "cap": 0})), None).is_none());
    }

    #[test]
    fn plan_names_match_the_cli_labels() {
        assert_eq!(plan_label("individual-goat"), "GOAT");
        assert_eq!(plan_label("individual-go-v1"), "Go");
        assert_eq!(plan_label("individual-provider"), "Provider");
        assert_eq!(plan_label("individual-pro"), "Pro");
        assert_eq!(plan_label("teams-pro"), "Teams Pro");
        assert_eq!(plan_label("custom-plan"), "custom-plan");
    }
}
