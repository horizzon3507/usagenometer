//! Kimi (Moonshot AI "for coding") quota — read-only reuse of the OAuth
//! credentials the CLIs persist under `~/.kimi/credentials/` /
//! `~/.kimi-code/credentials/` (`<name>.json`, `{access_token, refresh_token,
//! expires_at, expires_in}`; `kimi-code.json` holds the managed-platform
//! token). `GET {base}/usages` is the endpoint kimi-cli's `/usage` command and
//! kimi-code's quota panel both read — no login flow is ever implemented.
//! An expired access token is refreshed exactly like the CLIs do (public
//! OAuth client id) and the refreshed token is written back atomically so
//! refresh-token rotation stays consistent for the CLI itself.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_number, coerce_unix_seconds, create_meter,
    meter_from_used_percent,
};

const ID: &str = "kimi";
const LABEL: &str = "Kimi";
/// Public OAuth client id shared by kimi-cli / kimi-code (source constant,
/// not a secret).
const OAUTH_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
const DEFAULT_API_BASE: &str = "https://api.kimi.com/coding/v1";
/// boosterWallet amounts are fixed-point millionths of a cent.
const FIXED_POINT_CENTS: f64 = 1_000_000.0;

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

fn auth_err(message: impl Into<String>) -> FetchErr {
    FetchErr {
        status: SnapshotStatus::Auth,
        message: message.into(),
    }
}

fn err(message: impl Into<String>) -> FetchErr {
    FetchErr {
        status: SnapshotStatus::Error,
        message: message.into(),
    }
}

struct Creds {
    path: PathBuf,
    access_token: String,
    refresh_token: String,
    expires_at: f64,
    raw: Value,
}

fn fetch_inner(client: &HttpClient) -> Result<ProviderSnapshot, FetchErr> {
    let mut creds = load_creds()?;
    let now = now_secs();
    if creds.expires_at > 0.0 && creds.expires_at <= now + 30.0 {
        refresh(client, &mut creds)?;
    }

    let bearer = format!("Bearer {}", creds.access_token);
    let headers = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
    ];
    let url = format!("{}/usages", api_base());
    let payload: Value = client.get_json(&url, &headers).map_err(map_http)?;
    Ok(snapshot_from_usages(&payload))
}

fn map_http(e: HttpError) -> FetchErr {
    if e.is_auth_error() {
        auth_err("Kimi session was rejected. Run kimi login.")
    } else {
        FetchErr {
            status: SnapshotStatus::Error,
            message: e.to_string(),
        }
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn api_base() -> String {
    std::env::var("KIMI_CODE_BASE_URL")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.into())
}

fn oauth_host() -> String {
    for var in ["KIMI_CODE_OAUTH_HOST", "KIMI_OAUTH_HOST"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim_end_matches('/').to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    DEFAULT_OAUTH_HOST.into()
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn credential_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Ok(v) = std::env::var("KIMI_SHARE_DIR") {
        let v = v.trim();
        if !v.is_empty() {
            push(PathBuf::from(v).join("credentials"));
        }
    }
    if let Ok(v) = std::env::var("KIMI_CODE_HOME") {
        let v = v.trim();
        if !v.is_empty() {
            push(PathBuf::from(v).join("credentials"));
        }
    }
    push(home().join(".kimi").join("credentials"));
    push(home().join(".kimi-code").join("credentials"));
    dirs
}

/// Load the best credential file: `kimi-code.json` first (managed platform),
/// then any other `*.json` carrying a non-empty access_token.
fn load_creds() -> Result<Creds, FetchErr> {
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in credential_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        found.sort_by(|a, b| {
            let score = |p: &Path| {
                (p.file_name().and_then(|n| n.to_str()) != Some("kimi-code.json")) as usize
            };
            score(a).cmp(&score(b))
        });
        files.extend(found);
    }
    if files.is_empty() {
        return Err(auth_err(
            "Kimi credentials not found under ~/.kimi*/credentials/. Run kimi login.",
        ));
    }
    for path in files {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(access) = value
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        return Ok(Creds {
            path,
            access_token: access.to_string(),
            refresh_token: value
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            expires_at: value
                .get("expires_at")
                .and_then(coerce_unix_seconds)
                .unwrap_or(0.0),
            raw: value,
        });
    }
    Err(auth_err(
        "Kimi credentials have no access_token. Run kimi login.",
    ))
}

/// Refresh like the CLIs: `POST {oauth_host}/api/oauth/token` with the public
/// client id, then persist the new token pair atomically so the CLI's own
/// next refresh still works (refresh tokens rotate).
fn refresh(client: &HttpClient, creds: &mut Creds) -> Result<(), FetchErr> {
    if creds.refresh_token.is_empty() {
        return Err(auth_err(
            "Kimi access token expired and no refresh token is stored. Run kimi login.",
        ));
    }
    let mut form = std::collections::HashMap::new();
    form.insert("client_id", OAUTH_CLIENT_ID);
    form.insert("grant_type", "refresh_token");
    form.insert("refresh_token", creds.refresh_token.as_str());
    let url = format!("{}/api/oauth/token", oauth_host());
    let resp: Value = client.post_form(&url, &form).map_err(|e| {
        if e.is_auth_error() {
            auth_err("Kimi session expired. Run kimi login.")
        } else {
            FetchErr {
                status: SnapshotStatus::Error,
                message: format!("Kimi token refresh failed: {e}"),
            }
        }
    })?;
    let access = resp
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("Kimi token refresh returned no access token."))?;
    let expires_in = resp
        .get("expires_in")
        .and_then(coerce_unix_seconds)
        .unwrap_or(0.0);
    creds.access_token = access.clone();
    if let Some(rt) = resp.get("refresh_token").and_then(Value::as_str) {
        creds.refresh_token = rt.to_string();
    }
    creds.expires_at = now_secs() + expires_in;

    // Write back the full stored shape (access_token/refresh_token/expires_at
    // + expires_in), preserving unrelated fields.
    creds.raw["access_token"] = Value::String(access);
    creds.raw["refresh_token"] = Value::String(creds.refresh_token.clone());
    creds.raw["expires_at"] = serde_json::json!(creds.expires_at);
    creds.raw["expires_in"] = serde_json::json!(expires_in);
    if let Some(parent) = creds.path.parent() {
        let tmp = parent.join(format!(
            ".{}.usg-tmp",
            creds
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("credentials.json")
        ));
        if let Ok(text) = serde_json::to_string(&creds.raw)
            && fs::write(&tmp, text).is_ok()
        {
            let _ = fs::rename(&tmp, &creds.path);
        }
    }
    Ok(())
}

/// `GET /usages` response → meters. New shape (kimi-code managed):
/// `{usages: {limit_5h, limit_7d, limit_month_total, limit_month_code:
///   {used_ratio, reset_time?}}, boosterWallet: {...}}`.
/// Older kimi-cli shape is also parsed: `{usage:{limit,used|remaining},
/// limits:[{detail:{...}, window:{duration,timeUnit}, name|title|scope}]}`.
fn snapshot_from_usages(payload: &Value) -> ProviderSnapshot {
    let mut meters = Vec::new();

    if let Some(usages) = payload.get("usages").and_then(Value::as_object) {
        for (key, title, window_s) in [
            ("limit_5h", "5h limit", Some(5.0 * 3600.0)),
            ("limit_7d", "Weekly limit", Some(7.0 * 86400.0)),
            ("limit_month_total", "Monthly limit", None),
            ("limit_month_code", "Monthly code limit", None),
        ] {
            let Some(entry) = usages.get(key).and_then(Value::as_object) else {
                continue;
            };
            let Some(ratio) = entry.get("used_ratio").and_then(coerce_number) else {
                continue;
            };
            let reset_at = entry
                .get("reset_time")
                .and_then(coerce_unix_seconds)
                .or_else(|| {
                    entry
                        .get("reset_time")
                        .and_then(Value::as_str)
                        .and_then(parse_rfc3339)
                });
            let mut m = meter_from_used_percent(key, title, ratio, reset_at);
            m.window_seconds = window_s;
            meters.push(m);
        }
    }

    if let Some(wallet) = payload.get("boosterWallet").and_then(Value::as_object)
        && let Some(balance) = wallet.get("balance").and_then(Value::as_object)
        && balance.get("type").and_then(Value::as_str) == Some("BOOSTER")
    {
        let total_cents = balance
            .get("amount")
            .and_then(coerce_number)
            .map(|v| v / FIXED_POINT_CENTS)
            .unwrap_or(0.0);
        let left_cents = balance
            .get("amountLeft")
            .and_then(coerce_number)
            .map(|v| v / FIXED_POINT_CENTS)
            .unwrap_or(0.0);
        if total_cents > 0.0 {
            meters.push(create_meter(
                "booster_wallet",
                "Booster wallet",
                Some(((total_cents - left_cents) / total_cents).max(0.0).min(1.0)),
                Some((left_cents / total_cents).clamp(0.0, 1.0)),
                Some((total_cents - left_cents) / 100.0),
                Some(left_cents / 100.0),
                Some(total_cents / 100.0),
                "USD",
                None,
                None,
                None,
            ));
        }
        let cents_field = |key: &str| {
            wallet
                .get(key)
                .and_then(|v| v.get("priceInCents"))
                .and_then(coerce_number)
        };
        if wallet.get("monthlyChargeLimitEnabled") == Some(&Value::Bool(true))
            && let Some(limit) = cents_field("monthlyChargeLimit")
            && limit > 0.0
        {
            let used = cents_field("monthlyUsed").unwrap_or(0.0);
            meters.push(create_meter(
                "booster_monthly",
                "Booster monthly spend",
                Some((used / limit).clamp(0.0, 1.0)),
                Some((1.0 - used / limit).clamp(0.0, 1.0)),
                Some(used / 100.0),
                Some((limit - used).max(0.0) / 100.0),
                Some(limit / 100.0),
                "USD",
                None,
                None,
                None,
            ));
        }
    }

    // Older kimi-cli `/usage` shape: {usage:{limit,used|remaining}, limits:[...]}
    if meters.is_empty() {
        if let Some(u) = payload.get("usage")
            && let Some(m) = limit_meter("weekly", "Weekly limit", u, None)
        {
            meters.push(m);
        }
        if let Some(list) = payload.get("limits").and_then(Value::as_array) {
            for (i, item) in list.iter().enumerate() {
                let detail = item
                    .get("detail")
                    .and_then(Value::as_object)
                    .map(|d| Value::Object(d.clone()))
                    .unwrap_or_else(|| item.clone());
                let label = item
                    .get("name")
                    .or_else(|| item.get("title"))
                    .or_else(|| item.get("scope"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Limit #{}", i + 1));
                if let Some(m) = limit_meter(&format!("limit_{i}"), &label, &detail, None) {
                    meters.push(m);
                }
            }
        }
    }

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if meters.is_empty() {
            Some("Logged in, but this Kimi account did not expose quota.".into())
        } else {
            None
        },
        account: None,
        plan: None,
        meters,
        stale_age_secs: None,
    }
}

fn limit_meter(
    id: &str,
    title: &str,
    data: &Value,
    reset: Option<f64>,
) -> Option<crate::providers::types::UsageMeter> {
    let limit = data.get("limit").and_then(coerce_number);
    let used = data.get("used").and_then(coerce_number).or_else(|| {
        match (limit, data.get("remaining").and_then(coerce_number)) {
            (Some(l), Some(r)) => Some((l - r).max(0.0)),
            _ => None,
        }
    });
    if used.is_none() && limit.is_none() {
        return None;
    }
    let reset_at = reset.or_else(|| {
        ["reset_at", "resetAt", "reset_time", "resetTime"]
            .iter()
            .find_map(|k| data.get(*k).and_then(coerce_unix_seconds))
            .or_else(|| {
                ["reset_at", "resetAt", "reset_time", "resetTime"]
                    .iter()
                    .find_map(|k| data.get(*k).and_then(Value::as_str))
                    .and_then(parse_rfc3339)
            })
    });
    Some(create_meter(
        id,
        title,
        used.zip(limit)
            .and_then(|(u, l)| (l > 0.0).then(|| (u / l).clamp(0.0, 1.0))),
        used.zip(limit)
            .and_then(|(u, l)| (l > 0.0).then(|| (1.0 - u / l).clamp(0.0, 1.0))),
        used,
        used.zip(limit).map(|(u, l)| (l - u).max(0.0)),
        limit,
        "units",
        reset_at,
        None,
        None,
    ))
}

fn parse_rfc3339(s: &str) -> Option<f64> {
    time::OffsetDateTime::parse(s.trim(), &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|dt| dt.unix_timestamp() as f64 + f64::from(dt.nanosecond()) / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_managed_usages() {
        let payload = serde_json::json!({
            "usages": {
                "limit_5h": {"used_ratio": 0.42, "reset_time": "2026-10-01T20:00:00Z"},
                "limit_7d": {"used_ratio": 0.10},
                "limit_month_total": {"used_ratio": 0.75},
                "limit_month_code": {"used_ratio": 0.5}
            },
            "boosterWallet": {
                "balance": {"type": "BOOSTER", "amount": 500_000_000, "amountLeft": 200_000_000},
                "monthlyChargeLimitEnabled": true,
                "monthlyChargeLimit": {"priceInCents": 1000, "currency": "USD"},
                "monthlyUsed": {"priceInCents": 250, "currency": "USD"}
            }
        });
        let snap = snapshot_from_usages(&payload);
        assert_eq!(snap.status, SnapshotStatus::Ok);
        assert_eq!(snap.meters.len(), 6);
        assert_eq!(snap.meters[0].id, "limit_5h");
        assert!((snap.meters[0].percent.unwrap() - 0.42).abs() < 1e-9);
        assert_eq!(snap.meters[0].window_seconds, Some(18000.0));
        assert!(snap.meters[0].reset_at.unwrap() > 0.0);
        let wallet = &snap.meters[4];
        assert_eq!(wallet.id, "booster_wallet");
        assert_eq!(wallet.unit, "USD");
        assert!((wallet.limit.unwrap() - 5.0).abs() < 1e-9);
        assert!((wallet.left.unwrap() - 2.0).abs() < 1e-9);
        let monthly = &snap.meters[5];
        assert!((monthly.percent.unwrap() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn parses_legacy_usage_shape() {
        let payload = serde_json::json!({
            "usage": {"limit": 100, "remaining": 40},
            "limits": [
                {"detail": {"limit": 50, "used": 10}, "window": {"duration": 300, "timeUnit": "MINUTES"}},
                {"name": "Weekly", "detail": {"limit": 200, "used": 100}}
            ]
        });
        let snap = snapshot_from_usages(&payload);
        assert_eq!(snap.meters.len(), 3);
        assert!((snap.meters[0].percent.unwrap() - 0.60).abs() < 1e-9);
        assert_eq!(snap.meters[2].title, "Weekly");
        assert!((snap.meters[2].percent.unwrap() - 0.50).abs() < 1e-9);
    }

    #[test]
    fn empty_payload_is_ok_but_unmetered() {
        let snap = snapshot_from_usages(&serde_json::json!({}));
        assert_eq!(snap.status, SnapshotStatus::Ok);
        assert!(snap.meters.is_empty());
        assert!(snap.error.is_some());
    }
}
