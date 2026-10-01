//! GLM Coding Plan (z.ai) usage — read-only quota API.
//!
//! `GET {api.z.ai|open.bigmodel.cn}/api/monitor/usage/quota/limit` with the
//! plan's API key as Bearer. The key lives wherever the user pointed Claude
//! Code at z.ai: `ZAI_API_KEY`, else `env.ANTHROPIC_AUTH_TOKEN` in
//! `~/.claude/settings.json` — used only when `env.ANTHROPIC_BASE_URL` is a
//! z.ai host, so a real Anthropic token is never sent to z.ai.
//!
//! Response: `{data: {level, limits: [{type, unit, number, usage,
//! currentValue, remaining, percentage, nextResetTime(ms)}]}}` where
//! `unit:3,number:5` is the 5-hour window, `unit:6,number:1` weekly and
//! `TIME_LIMIT` the monthly MCP tool budget. Legacy payloads carry flat
//! `fiveHourPercent` / `weeklyPercent` / `monthlyMCPUsage` fields instead.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_number, coerce_unix_seconds, meter_from_used_percent,
};

const QUOTA_URL_INTL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const QUOTA_URL_CN: &str = "https://open.bigmodel.cn/api/monitor/usage/quota/limit";
const ID: &str = "glm";
const LABEL: &str = "GLM";

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    match fetch_inner(client) {
        Ok(snap) => snap,
        Err(e) => ProviderSnapshot::fail(ID, LABEL, e.status, e.message),
    }
}

struct FetchErr {
    status: SnapshotStatus,
    message: String,
}

fn fetch_inner(client: &HttpClient) -> Result<ProviderSnapshot, FetchErr> {
    let auth = load_auth().map_err(|message| FetchErr {
        status: SnapshotStatus::Auth,
        message,
    })?;
    let bearer = format!("Bearer {}", auth.api_key);
    let payload: Value = client
        .get_json(
            auth.quota_url,
            &[
                ("Accept", "application/json"),
                ("Authorization", bearer.as_str()),
                ("User-Agent", "usagenometer/0.1"),
            ],
        )
        .map_err(|e: HttpError| {
            let status = if e.is_auth_error() {
                SnapshotStatus::Auth
            } else {
                SnapshotStatus::Error
            };
            let message = if e.is_auth_error() {
                "z.ai API key was rejected. Check the GLM Coding Plan key.".into()
            } else {
                e.to_string()
            };
            FetchErr { status, message }
        })?;

    Ok(snapshot_from_quota(&payload))
}

struct Auth {
    api_key: String,
    quota_url: &'static str,
}

fn load_auth() -> Result<Auth, String> {
    if let Ok(key) = std::env::var("ZAI_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Ok(Auth {
                api_key: key.into(),
                quota_url: QUOTA_URL_INTL,
            });
        }
    }

    let settings = read_json(&settings_path()).ok_or_else(|| {
        format!(
            "z.ai credentials not found ({}). Point Claude Code at z.ai or set ZAI_API_KEY.",
            settings_path().display()
        )
    })?;
    let env = settings.get("env").cloned().unwrap_or(Value::Null);
    let base = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let quota_url = if base.contains("bigmodel.cn") {
        QUOTA_URL_CN
    } else if base.contains("z.ai") {
        QUOTA_URL_INTL
    } else {
        return Err(
            "ANTHROPIC_BASE_URL is not a z.ai host — not treating ANTHROPIC_AUTH_TOKEN as a GLM key."
                .into(),
        );
    };
    let api_key = env
        .get("ANTHROPIC_AUTH_TOKEN")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "GLM Coding Plan key missing (env.ANTHROPIC_AUTH_TOKEN in ~/.claude/settings.json)."
                .to_string()
        })?;
    Ok(Auth {
        api_key: api_key.into(),
        quota_url,
    })
}

fn settings_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("settings.json");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}

fn read_json(path: &PathBuf) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Map one `limits[]` entry onto the 5h / weekly / monthly surfaces.
/// (id, title) — `None` when the row is not a quota window.
fn limit_window(limit: &Value, index: usize) -> (String, String) {
    let ltype = limit.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let unit = limit.get("unit").and_then(coerce_number);
    let number = limit.get("number").and_then(coerce_number);
    match (ltype, unit, number) {
        ("TIME_LIMIT", _, _) => ("monthly_mcp".into(), "Monthly · MCP".into()),
        (_, Some(3.0), Some(5.0)) => ("five_hour".into(), "5 hour".into()),
        (_, Some(6.0), Some(1.0)) => ("seven_day".into(), "Weekly".into()),
        _ => {
            let kind = if ltype.is_empty() { "limit" } else { ltype };
            (
                format!("{}-{}", kind.to_lowercase().replace('_', "-"), index),
                kind.replace('_', " "),
            )
        }
    }
}

fn snapshot_from_quota(payload: &Value) -> ProviderSnapshot {
    let data = payload.get("data").unwrap_or(payload);
    let mut meters = Vec::new();

    if let Some(limits) = data.get("limits").and_then(|v| v.as_array()) {
        for (i, limit) in limits.iter().enumerate() {
            let (id, title) = limit_window(limit, i);
            let percent = limit.get("percentage").and_then(coerce_number).or_else(|| {
                let used = limit.get("currentValue").and_then(coerce_number)?;
                let total = limit.get("usage").and_then(coerce_number)?;
                (total > 0.0).then_some(used / total * 100.0)
            });
            let reset_at = limit
                .get("nextResetTime")
                .or_else(|| limit.get("resetTime"))
                .and_then(coerce_unix_seconds);
            if let Some(p) = percent {
                meters.push(meter_from_used_percent(&id, &title, p, reset_at));
            }
        }
    }

    // Legacy flat-percent payload.
    if meters.is_empty() {
        for (key, id, title) in [
            ("fiveHourPercent", "five_hour", "5 hour"),
            ("weeklyPercent", "seven_day", "Weekly"),
            ("monthlyMCPUsage", "monthly_mcp", "Monthly · MCP"),
        ] {
            if let Some(p) = data.get(key).and_then(coerce_number) {
                let reset_at = data.get("nextResetTime").and_then(coerce_unix_seconds);
                meters.push(meter_from_used_percent(id, title, p, reset_at));
            }
        }
    }

    let plan = data
        .get("level")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if meters.is_empty() {
            Some("z.ai quota connected, but no usage limits were returned.".into())
        } else {
            None
        },
        account: None,
        plan,
        meters,
        stale_age_secs: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_limits_array() {
        let payload = serde_json::json!({
            "code": 0,
            "data": {
                "level": "pro",
                "limits": [
                    {"type": "CREDIT_LIMIT", "unit": 3, "number": 5,
                     "usage": 12000, "currentValue": 6000, "remaining": 6000,
                     "percentage": 50, "nextResetTime": 1787649214999_f64},
                    {"type": "CREDIT_LIMIT", "unit": 6, "number": 1,
                     "usage": 60000, "currentValue": 0, "remaining": 60000,
                     "percentage": 0},
                    {"type": "TIME_LIMIT", "usage": 100, "currentValue": 10,
                     "remaining": 90, "percentage": 10}
                ]
            }
        });
        let snap = snapshot_from_quota(&payload);
        assert_eq!(snap.status, SnapshotStatus::Ok);
        assert_eq!(snap.plan.as_deref(), Some("pro"));
        assert_eq!(snap.meters.len(), 3);
        assert_eq!(snap.meters[0].id, "five_hour");
        assert_eq!(snap.meters[0].title, "5 hour");
        assert!((snap.meters[0].percent.unwrap() - 0.5).abs() < 1e-9);
        assert!(snap.meters[0].reset_at.unwrap() > 1_700_000_000.0);
        assert_eq!(snap.meters[1].id, "seven_day");
        assert_eq!(snap.meters[2].id, "monthly_mcp");
    }

    #[test]
    fn percentage_falls_back_to_value_over_usage() {
        let payload = serde_json::json!({
            "limits": [{"type": "TOKENS_LIMIT", "unit": 3, "number": 5,
                        "usage": 8000, "currentValue": 2000}]
        });
        let snap = snapshot_from_quota(&payload);
        assert!((snap.meters[0].percent.unwrap() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn parses_legacy_flat_fields() {
        let payload = serde_json::json!({
            "data": {"fiveHourPercent": 30, "weeklyPercent": 60}
        });
        let snap = snapshot_from_quota(&payload);
        assert_eq!(snap.meters.len(), 2);
        assert_eq!(snap.meters[0].id, "five_hour");
        assert!((snap.meters[1].percent.unwrap() - 0.60).abs() < 1e-9);
    }

    #[test]
    fn empty_limits_is_ok_with_note() {
        let snap = snapshot_from_quota(&serde_json::json!({"data": {"limits": []}}));
        assert_eq!(snap.status, SnapshotStatus::Ok);
        assert!(snap.meters.is_empty());
        assert!(snap.error.is_some());
    }
}
