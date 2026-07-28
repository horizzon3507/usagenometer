//! Grok Build usage — cli-chat-proxy billing (OIDC session from ~/.grok/auth.json).

use serde_json::Value;
use std::fs;
use std::path::PathBuf;

use crate::http::{HttpClient, HttpError};
use crate::jwt::normalize_bearer;
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, create_meter, meter_from_used_percent,
};

const USER_URL: &str = "https://cli-chat-proxy.grok.com/v1/user";
const BILLING_CREDITS_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing";
const ID: &str = "grok";
const LABEL: &str = "Grok";

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    match fetch_inner(client) {
        Ok(snap) => snap,
        Err(err) => {
            if which("grok") && err.status == SnapshotStatus::Auth {
                ProviderSnapshot {
                    id: ID.into(),
                    label: LABEL.into(),
                    status: SnapshotStatus::Auth,
                    error: Some(err.message),
                    account: None,
                    plan: None,
                    meters: vec![],
                }
            } else if which("grok") && err.status == SnapshotStatus::Error {
                ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Error, err.message)
            } else {
                ProviderSnapshot::fail(ID, LABEL, err.status, err.message)
            }
        }
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

    let bearer = format!("Bearer {}", auth.access_token);
    let headers_base = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
        ("User-Agent", "usagenometer/0.1"),
        ("X-XAI-Token-Auth", "xai-grok-cli"),
    ];

    let user: Value = client
        .get_json(USER_URL, &headers_base)
        .map_err(map_http)?;

    let user_id = user
        .get("userId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FetchErr {
            status: SnapshotStatus::Error,
            message: "Grok /v1/user response missing userId.".into(),
        })?;

    let email = user
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or(auth.email);

    let credits_headers = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
        ("User-Agent", "usagenometer/0.1"),
        ("X-XAI-Token-Auth", "xai-grok-cli"),
        ("x-userid", user_id),
    ];

    let credits: Value = client
        .get_json(BILLING_CREDITS_URL, &credits_headers)
        .map_err(map_http)?;

    let monthly = client.get_json(BILLING_URL, &credits_headers).ok();

    Ok(snapshot_from_billing(
        &credits,
        monthly.as_ref(),
        email.as_deref(),
    ))
}

fn map_http(e: HttpError) -> FetchErr {
    let status = if e.is_auth_error() {
        SnapshotStatus::Auth
    } else {
        SnapshotStatus::Error
    };
    let message = if e.is_auth_error() {
        "Grok session was rejected. Run grok login.".into()
    } else {
        e.to_string()
    };
    FetchErr { status, message }
}

struct Auth {
    access_token: String,
    email: Option<String>,
}

fn load_auth() -> Result<Auth, String> {
    let path = auth_path();
    if !path.exists() {
        return Err(format!(
            "Grok auth not found at {}. Run grok login.",
            path.display()
        ));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|_| format!("Failed to read Grok auth at {}.", path.display()))?;
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|_| format!("Grok auth at {} is not valid JSON.", path.display()))?;

    let entry = payload
        .as_object()
        .and_then(|map| {
            map.values()
                .find(|v| {
                    v.get("key")
                        .and_then(|k| k.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                })
                .cloned()
        })
        .ok_or_else(|| {
            format!(
                "Grok auth at {} has no session token. Run grok login.",
                path.display()
            )
        })?;

    let access = entry
        .get("key")
        .and_then(|v| v.as_str())
        .map(normalize_bearer)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Grok session token is empty. Run grok login.".to_string())?;

    if let Some(exp) = entry.get("expires_at").and_then(coerce_unix_seconds) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        if exp <= now + 30.0 {
            return Err("Grok session token is expired. Run grok login.".into());
        }
    }

    Ok(Auth {
        access_token: access,
        email: entry
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

fn auth_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
        .join("auth.json")
}

pub fn snapshot_from_billing(
    credits: &Value,
    monthly: Option<&Value>,
    email: Option<&str>,
) -> ProviderSnapshot {
    let config = credits.get("config").unwrap_or(credits);
    let period_end = config
        .pointer("/currentPeriod/end")
        .or_else(|| config.get("billingPeriodEnd"))
        .and_then(coerce_unix_seconds);

    let mut meters = Vec::new();

    if let Some(percent) = config.get("creditUsagePercent").and_then(|v| v.as_f64()) {
        meters.push(meter_from_used_percent(
            "weekly_credits",
            "Weekly credits",
            percent,
            period_end,
        ));
    }

    if let Some(products) = config.get("productUsage").and_then(|v| v.as_array()) {
        for product in products {
            let name = product
                .get("product")
                .and_then(|v| v.as_str())
                .unwrap_or("Product");
            let Some(percent) = product.get("usagePercent").and_then(|v| v.as_f64()) else {
                continue;
            };
            meters.push(meter_from_used_percent(
                &format!("product_{}", name.to_lowercase()),
                &humanize_product(name),
                percent,
                period_end,
            ));
        }
    }

    let on_cap = config
        .pointer("/onDemandCap/val")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let on_used = config
        .pointer("/onDemandUsed/val")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if on_cap > 0.0 {
        meters.push(create_meter(
            "on_demand",
            "On-demand",
            Some(on_used / on_cap),
            Some((1.0 - on_used / on_cap).max(0.0)),
            Some(on_used),
            Some((on_cap - on_used).max(0.0)),
            Some(on_cap),
            "credits",
            period_end,
            None,
            None,
        ));
    }

    // Unified-billing fallback: monthly included budget
    if meters.is_empty()
        && let Some(monthly) = monthly
    {
        let mcfg = monthly.get("config").unwrap_or(monthly);
        let used = mcfg.pointer("/used/val").and_then(|v| v.as_f64());
        let limit = mcfg.pointer("/monthlyLimit/val").and_then(|v| v.as_f64());
        let reset_at = mcfg
            .get("billingPeriodEnd")
            .and_then(coerce_unix_seconds);
        if let (Some(u), Some(l)) = (used, limit) {
            if l > 0.0 {
                meters.push(create_meter(
                    "monthly",
                    "Monthly",
                    Some(u / l),
                    Some((1.0 - u / l).max(0.0)),
                    Some(u),
                    Some((l - u).max(0.0)),
                    Some(l),
                    "credits",
                    reset_at,
                    None,
                    None,
                ));
            } else if u > 0.0 {
                // Limit omitted/zero on some unified plans — still surface used credits.
                meters.push(create_meter(
                    "monthly_used",
                    "Monthly used",
                    None,
                    None,
                    Some(u),
                    None,
                    None,
                    "credits",
                    reset_at,
                    None,
                    None,
                ));
            }
        }
    }

    let plan = credits
        .get("subscriptionTier")
        .or_else(|| credits.get("planName"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if meters.is_empty() {
            Some("Logged in, but this Grok plan did not expose percentage quotas.".into())
        } else {
            None
        },
        account: email.map(str::to_string),
        plan,
        meters,
    }
}

fn humanize_product(name: &str) -> String {
    match name {
        "GrokBuild" => "Grok Build".into(),
        "GrokChat" => "Grok Chat".into(),
        "Api" => "API".into(),
        other => other.to_string(),
    }
}

fn which(command: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_credits_meter() {
        let credits = serde_json::json!({
            "subscriptionTier": "SuperGrok",
            "config": {
                "creditUsagePercent": 34.0,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-05T01:12:18.000Z"
                },
                "productUsage": [
                    {"product": "GrokBuild", "usagePercent": 45.0},
                    {"product": "GrokChat"}
                ],
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0}
            }
        });
        let snap = snapshot_from_billing(&credits, None, Some("g@example.com"));
        assert_eq!(snap.meters.len(), 2);
        assert_eq!(snap.meters[0].id, "weekly_credits");
        assert!((snap.meters[0].percent.unwrap() - 0.34).abs() < 1e-9);
        assert_eq!(snap.meters[1].id, "product_grokbuild");
    }

    #[test]
    fn monthly_fallback() {
        let credits = serde_json::json!({
            "config": {
                "isUnifiedBillingUser": true,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-08-05T00:00:00Z"},
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0}
            }
        });
        let monthly = serde_json::json!({
            "config": {
                "monthlyLimit": {"val": 150000},
                "used": {"val": 75000},
                "billingPeriodEnd": "2026-08-01T00:00:00Z"
            }
        });
        let snap = snapshot_from_billing(&credits, Some(&monthly), None);
        assert_eq!(snap.meters.len(), 1);
        assert_eq!(snap.meters[0].id, "monthly");
        assert!((snap.meters[0].percent.unwrap() - 0.5).abs() < 1e-9);
    }
}
