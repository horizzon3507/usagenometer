//! Claude Code usage — OAuth `/api/oauth/usage`, with Antigravity 3p fallback.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{HttpClient, HttpError};
use crate::jwt::normalize_bearer;
use crate::providers::antigravity;
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, meter_from_used_percent,
};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA_HEADER: &str = "oauth-2025-04-20";
const ID: &str = "claude";
const LABEL: &str = "Claude";

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    match fetch_oauth(client) {
        Ok(snap) => snap,
        Err(oauth_err) => {
            // Custom / proxy Claude setups often have no OAuth file; Claude/GPT
            // pool meters still live on Antigravity when that login exists.
            let ag = antigravity::fetch(client);
            if ag.status == SnapshotStatus::Ok {
                let meters: Vec<_> = ag
                    .meters
                    .into_iter()
                    .filter(|m| m.id.starts_with("3p-"))
                    .collect();
                if !meters.is_empty() {
                    return ProviderSnapshot {
                        id: ID.into(),
                        label: LABEL.into(),
                        status: SnapshotStatus::Ok,
                        error: Some("via Antigravity Claude/GPT pools".into()),
                        account: ag.account,
                        plan: Some("antigravity".into()),
                        meters,
                        stale_age_secs: None,
                    };
                }
            }

            if which("claude") {
                return ProviderSnapshot {
                    id: ID.into(),
                    label: LABEL.into(),
                    status: SnapshotStatus::Auth,
                    error: Some(format!(
                        "{oauth_err} (claude on PATH; run claude login for subscription meters, or use Antigravity)"
                    )),
                    account: None,
                    plan: None,
                    meters: vec![],
                    stale_age_secs: None,
                };
            }

            ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Error, oauth_err)
        }
    }
}

fn fetch_oauth(client: &HttpClient) -> Result<ProviderSnapshot, String> {
    let auth = load_oauth_auth()?;
    let bearer = format!("Bearer {}", auth.access_token);
    let summary: Value = client
        .get_json(
            USAGE_URL,
            &[
                ("Accept", "application/json"),
                ("Authorization", bearer.as_str()),
                ("anthropic-beta", BETA_HEADER),
                ("User-Agent", "usagenometer/0.1"),
            ],
        )
        .map_err(|e: HttpError| {
            if e.is_auth_error() {
                "Claude OAuth token was rejected. Run claude login.".into()
            } else {
                e.to_string()
            }
        })?;

    Ok(snapshot_from_oauth_usage(&summary, &auth))
}

pub struct Auth {
    pub access_token: String,
    pub subscription_type: Option<String>,
}

fn load_oauth_auth() -> Result<Auth, String> {
    let payload = read_credentials_payload()?;
    let oauth = payload
        .get("claudeAiOauth")
        .cloned()
        .unwrap_or(payload.clone());

    let access = oauth
        .get("accessToken")
        .or_else(|| oauth.get("access_token"))
        .and_then(|v| v.as_str())
        .map(normalize_bearer)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "Claude OAuth credentials found but access token is missing. Run claude login.".to_string()
        })?;

    if let Some(exp_ms) = oauth.get("expiresAt").and_then(|v| v.as_f64()) {
        let exp = if exp_ms > 9_999_999_999.0 {
            exp_ms / 1000.0
        } else {
            exp_ms
        };
        let now = now_secs();
        if exp <= now + 30.0 {
            return Err(
                "Claude OAuth token is expired. Open Claude Code or run claude login.".into(),
            );
        }
    }

    let subscription_type = oauth
        .get("subscriptionType")
        .or_else(|| oauth.get("rateLimitTier"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(Auth {
        access_token: access,
        subscription_type,
    })
}

fn read_credentials_payload() -> Result<Value, String> {
    let path = credentials_path();
    if path.exists() {
        let raw = fs::read_to_string(&path).map_err(|e| {
            format!(
                "Failed to read Claude credentials at {}: {e}",
                path.display()
            )
        })?;
        return serde_json::from_str(&raw).map_err(|_| {
            format!(
                "Claude credentials at {} are not valid JSON.",
                path.display()
            )
        });
    }

    // Linux / secret-service (Claude Code stores JSON under this service name on some installs)
    if let Ok(output) = Command::new("secret-tool")
        .args(["lookup", "service", "Claude Code-credentials"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            return serde_json::from_str(&text).map_err(|_| {
                "Claude keyring credentials are not valid JSON.".to_string()
            });
        }
    }

    Err(
        "Claude OAuth credentials not found (~/.claude/.credentials.json). Run claude login."
            .into(),
    )
}

fn credentials_path() -> PathBuf {
    if let Ok(custom) = std::env::var("CLAUDE_CREDENTIALS_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join(".credentials.json")
}

pub fn snapshot_from_oauth_usage(summary: &Value, auth: &Auth) -> ProviderSnapshot {
    let mut meters = Vec::new();

    // Legacy flat buckets
    push_util_bucket(
        &mut meters,
        summary.get("five_hour"),
        "five_hour",
        "5 hour",
    );
    push_util_bucket(
        &mut meters,
        summary.get("seven_day"),
        "seven_day",
        "Weekly",
    );
    push_util_bucket(
        &mut meters,
        summary.get("seven_day_sonnet"),
        "seven_day_sonnet",
        "Weekly · Sonnet",
    );
    push_util_bucket(
        &mut meters,
        summary.get("seven_day_opus"),
        "seven_day_opus",
        "Weekly · Opus",
    );

    // Newer structured `limits` array
    if let Some(limits) = summary.get("limits").and_then(|v| v.as_array()) {
        for (i, limit) in limits.iter().enumerate() {
            let kind = limit
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("limit");
            let model = limit
                .pointer("/scope/model/display_name")
                .and_then(|v| v.as_str());
            let title = match (kind, model) {
                ("session", _) => "5 hour".to_string(),
                ("weekly_all", _) => "Weekly".to_string(),
                ("weekly_scoped", Some(name)) => format!("Weekly · {name}"),
                (_, Some(name)) => format!("{kind} · {name}"),
                _ => kind.to_string(),
            };
            let id = format!("{kind}-{}", model.unwrap_or(&i.to_string()));
            let percent = limit
                .get("percent")
                .or_else(|| limit.get("utilization"))
                .and_then(|v| v.as_f64());
            let reset_at = limit
                .get("resets_at")
                .or_else(|| limit.get("resetsAt"))
                .and_then(coerce_unix_seconds);
            if let Some(p) = percent {
                meters.push(meter_from_used_percent(&id, &title, p, reset_at));
            }
        }
    }

    if let Some(extra) = summary.get("extra_usage") {
        let enabled = extra
            .get("is_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if enabled {
            let used = extra.get("used_credits").and_then(|v| v.as_f64());
            let limit = extra.get("monthly_limit").and_then(|v| v.as_f64());
            if let (Some(u), Some(l)) = (used, limit) {
                meters.push(crate::providers::types::create_meter(
                    "extra_usage",
                    "Extra usage",
                    if l > 0.0 { Some(u / l) } else { None },
                    if l > 0.0 {
                        Some((1.0 - u / l).max(0.0))
                    } else {
                        None
                    },
                    Some(u),
                    Some((l - u).max(0.0)),
                    Some(l),
                    "credits",
                    None,
                    None,
                    None,
                ));
            }
        }
    }

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if meters.is_empty() {
            Some("Claude OAuth connected, but no quota buckets were returned.".into())
        } else {
            None
        },
        account: None,
        plan: auth.subscription_type.clone(),
        meters,
        stale_age_secs: None,
    }
}

fn push_util_bucket(meters: &mut Vec<crate::providers::types::UsageMeter>, value: Option<&Value>, id: &str, title: &str) {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return;
    };
    let Some(util) = value.get("utilization").and_then(|v| v.as_f64()) else {
        return;
    };
    let reset_at = value.get("resets_at").and_then(coerce_unix_seconds);
    meters.push(meter_from_used_percent(id, title, util, reset_at));
}

fn which(command: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
        })
        .unwrap_or(false)
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
    fn parses_flat_buckets() {
        let summary = serde_json::json!({
            "five_hour": {"utilization": 12.5, "resets_at": "2026-08-05T01:12:18.000Z"},
            "seven_day": {"utilization": 40.0, "resets_at": "2026-08-10T00:00:00Z"},
            "seven_day_opus": null
        });
        let auth = Auth {
            access_token: "x".into(),
            subscription_type: Some("max".into()),
        };
        let snap = snapshot_from_oauth_usage(&summary, &auth);
        assert_eq!(snap.meters.len(), 2);
        assert_eq!(snap.meters[0].id, "five_hour");
        assert!((snap.meters[0].percent.unwrap() - 0.125).abs() < 1e-9);
    }
}
