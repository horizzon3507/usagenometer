//! Codex / ChatGPT WHAM usage.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{HttpClient, HttpError};
use crate::jwt::{jwt_exp, normalize_bearer};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, UsageMeter, clamp01, coerce_number, create_meter,
};

const API_BASE: &str = "https://chatgpt.com";
const SUMMARY_PATH: &str = "/backend-api/wham/usage";
const RESET_CREDITS_PATH: &str = "/backend-api/wham/rate-limit-reset-credits";
const WHAM_REFERER: &str = "https://chatgpt.com/codex/cloud/settings/analytics";
const EXPIRY_SKEW: f64 = 30.0;
const PRIMARY_WINDOW_HOURS: f64 = 5.0;
const WEEK_WINDOW_DAYS: f64 = 7.0;

const ID: &str = "codex";
const LABEL: &str = "Codex";

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    match fetch_inner(client) {
        Ok(snap) => snap,
        Err(err) => ProviderSnapshot::fail(ID, LABEL, err.status, err.message),
    }
}

struct FetchErr {
    status: SnapshotStatus,
    message: String,
}

fn fetch_inner(client: &HttpClient) -> Result<ProviderSnapshot, FetchErr> {
    let auth = load_auth().map_err(|e| FetchErr {
        status: SnapshotStatus::Auth,
        message: e,
    })?;

    let summary = fetch_summary(client, &auth.access_token).map_err(|e| {
        let status = if e.is_auth_error() {
            SnapshotStatus::Auth
        } else {
            SnapshotStatus::Error
        };
        let message = if e.is_auth_error() {
            "Codex CLI token was rejected. Run codex login.".into()
        } else {
            e.to_string()
        };
        FetchErr { status, message }
    })?;

    let meters = meters_from_summary(&summary);
    Ok(ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: None,
        account: summary
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        plan: summary
            .get("planType")
            .or_else(|| summary.get("plan_type"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        meters,
        stale_age_secs: None,
    })
}

struct Auth {
    access_token: String,
}

fn load_auth() -> Result<Auth, String> {
    let path = auth_path();
    let raw = fs::read_to_string(&path)
        .map_err(|_| format!("Codex CLI auth not found at {}. Run codex login.", path.display()))?;
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|_| format!("Codex CLI auth at {} is not valid JSON.", path.display()))?;

    let tokens = payload.get("tokens").unwrap_or(&Value::Null);
    let access = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(normalize_bearer)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "Codex CLI auth at {} does not contain an access token. Run codex login.",
                path.display()
            )
        })?;

    if let Some(exp) = jwt_exp(&access) {
        let now = now_secs();
        if exp <= now + EXPIRY_SKEW {
            return Err(
                "Codex CLI token is expired. Run codex login or start Codex CLI to refresh it."
                    .into(),
            );
        }
    }

    Ok(Auth {
        access_token: access,
    })
}

fn auth_path() -> PathBuf {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("auth.json");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("auth.json")
}

fn fetch_summary(client: &HttpClient, token: &str) -> Result<Value, HttpError> {
    let usage: Value = get_wham(client, SUMMARY_PATH, token)?;
    let _credits: Result<Value, HttpError> = get_wham(client, RESET_CREDITS_PATH, token);
    Ok(normalize_summary(&usage))
}

fn get_wham(client: &HttpClient, path: &str, token: &str) -> Result<Value, HttpError> {
    let url = format!("{API_BASE}{path}");
    client.get_json(
        &url,
        &[
            ("Accept", "*/*"),
            ("Authorization", &format!("Bearer {token}")),
            ("Cache-Control", "no-cache"),
            ("Pragma", "no-cache"),
            ("Referer", WHAM_REFERER),
            ("oai-language", "en-US"),
            ("x-openai-target-path", path),
            ("x-openai-target-route", path),
        ],
    )
}

/// Focused WHAM normalizer — primary/weekly windows + percent fields.
fn normalize_summary(payload: &Value) -> Value {
    let rate_limit = payload.get("rate_limit");
    let primary = rate_limit
        .and_then(|r| named_window(r.get("primary_window"), "primary"))
        .or_else(|| find_window_by_hours(payload, PRIMARY_WINDOW_HOURS));
    let weekly = rate_limit
        .and_then(|r| named_window(r.get("secondary_window"), "weekly"))
        .or_else(|| {
            rate_limit.and_then(|r| {
                // exhausted weekly often arrives as primary_window with 7d
                named_window(r.get("primary_window"), "weekly").filter(|w| {
                    w.get("windowSeconds")
                        .and_then(|v| v.as_f64())
                        .map(|s| (s - WEEK_WINDOW_DAYS * 86400.0).abs() <= 86400.0)
                        .unwrap_or(false)
                })
            })
        })
        .or_else(|| find_window_by_hours(payload, WEEK_WINDOW_DAYS * 24.0));

    // Prefer explicit secondary; a 7-day primary_window is the weekly pool.
    let (primary, weekly) = match (&primary, &weekly) {
        (Some(p), week) if is_week_window(p) => {
            (None, Some(week.clone().unwrap_or_else(|| {
                let mut w = p.clone();
                if let Some(obj) = w.as_object_mut() {
                    obj.insert("id".into(), Value::String("weekly".into()));
                }
                w
            })))
        }
        other => (other.0.clone(), other.1.clone()),
    };

    let mut out = serde_json::json!({
        "email": first_string(payload, &["email"]),
        "planType": first_string(payload, &["plan_type", "planType"]),
        "primaryWindow": primary,
        "weekWindow": weekly,
        "windows": Value::Array(vec![]),
    });

    if let Some(active) = out
        .get("primaryWindow")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| out.get("weekWindow").cloned().filter(|v| !v.is_null()))
    {
        out["percent"] = active.get("percent").cloned().unwrap_or(Value::Null);
        out["leftPercent"] = active.get("leftPercent").cloned().unwrap_or(Value::Null);
        out["used"] = active.get("used").cloned().unwrap_or(Value::Null);
        out["left"] = active.get("left").cloned().unwrap_or(Value::Null);
        out["limit"] = active.get("limit").cloned().unwrap_or(Value::Null);
        out["resetAt"] = active.get("resetAt").cloned().unwrap_or(Value::Null);
        out["resetAfterSeconds"] = active
            .get("resetAfterSeconds")
            .cloned()
            .unwrap_or(Value::Null);
    }

    out
}

fn is_week_window(window: &Value) -> bool {
    window
        .get("windowSeconds")
        .and_then(|v| v.as_f64())
        .map(|s| (s - WEEK_WINDOW_DAYS * 86400.0).abs() <= 86400.0)
        .unwrap_or(false)
}

fn named_window(value: Option<&Value>, id: &str) -> Option<Value> {
    let value = value?;
    if !value.is_object() {
        return None;
    }
    let used = local_number(value, &["used", "used_tokens", "tokens_used", "usage", "consumed"]);
    let limit = local_number(value, &["limit", "token_limit", "quota", "max", "capacity"]);
    let percent_raw = local_number(
        value,
        &[
            "used_percent",
            "percent",
            "percentage",
            "usage_percent",
            "percent_used",
            "utilization",
        ],
    );
    let percent = normalize_percent(percent_raw, used, limit)?;
    let window_seconds = local_number(
        value,
        &[
            "window_seconds",
            "duration_seconds",
            "limit_window_seconds",
        ],
    )
    .or_else(|| {
        local_number(value, &["window_minutes", "duration_minutes"]).map(|m| m * 60.0)
    })
    .or_else(|| local_number(value, &["window_hours", "duration_hours"]).map(|h| h * 3600.0))
    .or_else(|| local_number(value, &["window_days", "duration_days"]).map(|d| d * 86400.0));

    let reset_at = value.get("reset_at").and_then(coerce_number);
    let reset_after = value.get("reset_after_seconds").and_then(coerce_number);

    Some(serde_json::json!({
        "id": id,
        "used": used,
        "limit": limit,
        "left": used.zip(limit).map(|(u, l)| (l - u).max(0.0)),
        "percent": percent,
        "leftPercent": clamp01(1.0 - percent),
        "resetAt": reset_at,
        "resetAfterSeconds": reset_after,
        "windowSeconds": window_seconds,
    }))
}

fn find_window_by_hours(payload: &Value, hours: f64) -> Option<Value> {
    let target = hours * 3600.0;
    let tolerance = if hours >= 24.0 { 86400.0 } else { 2.0 * 3600.0 };
    let mut found = None;
    walk_windows(payload, &mut |obj| {
        let secs = local_number(
            obj,
            &[
                "window_seconds",
                "duration_seconds",
                "limit_window_seconds",
            ],
        )?;
        if (secs - target).abs() <= tolerance {
            found = named_window(Some(obj), if hours >= 24.0 { "weekly" } else { "primary" });
        }
        None::<()>
    });
    found
}

fn walk_windows(value: &Value, f: &mut dyn FnMut(&Value) -> Option<()>) {
    match value {
        Value::Object(map) => {
            let _ = f(value);
            for v in map.values() {
                walk_windows(v, f);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_windows(v, f);
            }
        }
        _ => {}
    }
}

fn normalize_percent(raw: Option<f64>, used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    if let Some(p) = raw {
        return Some(if p > 1.0 { clamp01(p / 100.0) } else { clamp01(p) });
    }
    match (used, limit) {
        (Some(u), Some(l)) if l > 0.0 => Some(clamp01(u / l)),
        _ => None,
    }
}

fn local_number(obj: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(v) = obj.get(*key).and_then(coerce_number) {
            return Some(v);
        }
    }
    None
}

fn first_string(obj: &Value, keys: &[&str]) -> Value {
    for key in keys {
        if let Some(s) = obj.get(*key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            return Value::String(s.to_string());
        }
    }
    Value::Null
}

fn meters_from_summary(summary: &Value) -> Vec<UsageMeter> {
    let mut meters = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push = |window: Option<&Value>, id: &str, title: &str| {
        let Some(window) = window.filter(|v| v.is_object()) else {
            return;
        };
        if !seen.insert(id.to_string()) {
            return;
        }
        meters.push(create_meter(
            id,
            title,
            window.get("percent").and_then(|v| v.as_f64()),
            window.get("leftPercent").and_then(|v| v.as_f64()),
            window.get("used").and_then(|v| v.as_f64()),
            window.get("left").and_then(|v| v.as_f64()),
            window.get("limit").and_then(|v| v.as_f64()),
            "percent",
            window.get("resetAt").and_then(|v| v.as_f64()),
            window.get("resetAfterSeconds").and_then(|v| v.as_f64()),
            window.get("windowSeconds").and_then(|v| v.as_f64()),
        ));
    };

    push(summary.get("primaryWindow"), "primary", "5 hour usage limit");
    push(summary.get("weekWindow"), "weekly", "Weekly usage limit");

    if meters.is_empty()
        && (summary.get("percent").and_then(|v| v.as_f64()).is_some()
            || summary.get("used").and_then(|v| v.as_f64()).is_some())
    {
        meters.push(create_meter(
            "summary",
            "Usage",
            summary.get("percent").and_then(|v| v.as_f64()),
            summary.get("leftPercent").and_then(|v| v.as_f64()),
            summary.get("used").and_then(|v| v.as_f64()),
            summary.get("left").and_then(|v| v.as_f64()),
            summary.get("limit").and_then(|v| v.as_f64()),
            "percent",
            summary.get("resetAt").and_then(|v| v.as_f64()),
            summary.get("resetAfterSeconds").and_then(|v| v.as_f64()),
            None,
        ));
    }

    meters
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_weekly_primary_window() {
        let payload = serde_json::json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 100,
                    "window_seconds": 7 * 86400,
                }
            }
        });
        let summary = normalize_summary(&payload);
        let week = summary.get("weekWindow").unwrap();
        assert!((week.get("percent").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-9);
        assert!((week.get("leftPercent").and_then(|v| v.as_f64()).unwrap() - 0.0).abs() < 1e-9);
        let meters = meters_from_summary(&summary);
        assert_eq!(meters[0].id, "weekly");
    }
}
