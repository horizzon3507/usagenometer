//! Antigravity / Cloud Code quota pools.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{HttpClient, HttpError};
use crate::jwt::normalize_bearer;
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, meter_from_remaining_fraction,
};

const CLOUD_CODE_BASES: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const QUOTA_PATH: &str = "/v1internal:retrieveUserQuotaSummary";
const USER_AGENT: &str = "antigravity/usagenometer";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const EXPIRY_SKEW: f64 = 60.0;
const ID: &str = "antigravity";
const LABEL: &str = "Antigravity";

const SUMMARY_BUCKETS: &[(&str, &str, f64)] = &[
    ("gemini-5h", "Gemini 5h", 5.0 * 3600.0),
    ("gemini-weekly", "Gemini weekly", 7.0 * 86400.0),
    ("3p-5h", "Claude/GPT 5h", 5.0 * 3600.0),
    ("3p-weekly", "Claude/GPT weekly", 7.0 * 86400.0),
];

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
    let auth = load_auth(client).map_err(|e| FetchErr {
        status: SnapshotStatus::Auth,
        message: e,
    })?;
    let summary = fetch_quota(client, &auth.access_token).map_err(|e| {
        let status = if e.is_auth_error() {
            SnapshotStatus::Auth
        } else {
            SnapshotStatus::Error
        };
        let message = if e.is_auth_error() {
            "Antigravity token was rejected. Sign in again with Antigravity.".into()
        } else {
            e.to_string()
        };
        FetchErr { status, message }
    })?;
    Ok(snapshot_from_quota_summary(&summary, auth.account.as_deref()))
}

struct Auth {
    access_token: String,
    account: Option<String>,
}

fn load_auth(client: &HttpClient) -> Result<Auth, String> {
    let stored = read_secret_payload()?;
    let token_block = stored
        .get("token")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| stored.clone());

    let mut access = token_block
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(normalize_bearer)
        .unwrap_or_default();
    let refresh = token_block
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(normalize_bearer)
        .unwrap_or_default();
    let mut expires_at = token_block
        .get("expiry")
        .or_else(|| token_block.get("expiry_date"))
        .and_then(coerce_unix_seconds);

    if access.is_empty() && refresh.is_empty() {
        return Err(
            "Antigravity credentials not found. Sign in with the Antigravity app or CLI.".into(),
        );
    }

    let now = now_secs();
    let needs_refresh = access.is_empty()
        || expires_at
            .map(|exp| exp <= now + EXPIRY_SKEW)
            .unwrap_or(false);

    if needs_refresh {
        let client_id = std::env::var("USAGENOMETER_GOOGLE_CLIENT_ID").unwrap_or_default();
        let client_secret = std::env::var("USAGENOMETER_GOOGLE_CLIENT_SECRET").unwrap_or_default();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(
                "Antigravity token needs refresh, but OAuth client credentials are not configured."
                    .into(),
            );
        }
        if refresh.is_empty() {
            return Err(
                "Antigravity access token is expired and no refresh token is available.".into(),
            );
        }

        let mut form = HashMap::new();
        form.insert("client_id", client_id.as_str());
        form.insert("client_secret", client_secret.as_str());
        form.insert("refresh_token", refresh.as_str());
        form.insert("grant_type", "refresh_token");

        let refreshed: Value = client.post_form(GOOGLE_TOKEN_URL, &form).map_err(|e| {
            if e.is_auth_error() {
                "Antigravity refresh token was rejected. Sign in again with Antigravity."
                    .to_string()
            } else {
                e.to_string()
            }
        })?;

        access = refreshed
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(normalize_bearer)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Antigravity token refresh returned no access token.".to_string())?;

        if let Some(expires_in) = refreshed.get("expires_in").and_then(|v| v.as_f64()) {
            expires_at = Some(now + expires_in.floor());
        }
    }

    if access.is_empty() {
        return Err(
            "Antigravity access token is unavailable. Sign in with the Antigravity app or CLI."
                .into(),
        );
    }

    if let Some(exp) = expires_at
        && exp <= now + EXPIRY_SKEW
    {
        return Err(
            "Antigravity access token is expired. Sign in again with Antigravity.".into(),
        );
    }

    Ok(Auth {
        access_token: access,
        account: read_active_google_account(),
    })
}

fn read_secret_payload() -> Result<Value, String> {
    if let Ok(output) = Command::new("secret-tool")
        .args(["lookup", "service", "gemini", "username", "antigravity"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            return parse_secret_text(&text);
        }
    }

    let oauth_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gemini")
        .join("oauth_creds.json");
    if oauth_path.exists()
        && let Ok(raw) = fs::read_to_string(&oauth_path)
        && let Ok(payload) = serde_json::from_str::<Value>(&raw)
    {
        return Ok(serde_json::json!({
            "token": payload,
            "auth_method": "gemini-oauth-file",
        }));
    }

    Err(
        "Antigravity credentials not found in the secret store (service=gemini, username=antigravity)."
            .into(),
    )
}

fn parse_secret_text(text: &str) -> Result<Value, String> {
    let mut raw = text.trim().to_string();
    if let Some(b64) = raw.strip_prefix("go-keyring-base64:") {
        let bytes = B64
            .decode(b64.trim())
            .map_err(|_| "Antigravity secret store payload is not valid base64.".to_string())?;
        raw = String::from_utf8(bytes)
            .map_err(|_| "Antigravity secret store payload is not valid UTF-8.".to_string())?;
    }

    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
        return Ok(v);
    }
    if let Some(token) = raw.strip_prefix("Bearer ") {
        return Ok(serde_json::json!({ "token": { "access_token": token.trim() } }));
    }
    if raw.len() > 20 {
        return Ok(serde_json::json!({ "token": { "access_token": raw } }));
    }
    Err("Antigravity secret store payload is not valid JSON.".into())
}

fn read_active_google_account() -> Option<String> {
    let path = dirs::home_dir()?.join(".gemini").join("google_accounts.json");
    let raw = fs::read_to_string(path).ok()?;
    let payload: Value = serde_json::from_str(&raw).ok()?;
    payload
        .get("active")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn fetch_quota(client: &HttpClient, token: &str) -> Result<Value, HttpError> {
    let mut last = None;
    for base in CLOUD_CODE_BASES {
        let url = format!("{base}{QUOTA_PATH}");
        match client.post_json(
            &url,
            &serde_json::json!({}),
            &[
                ("Accept", "application/json"),
                ("Authorization", &format!("Bearer {token}")),
                ("User-Agent", USER_AGENT),
            ],
        ) {
            Ok(v) => return Ok(v),
            Err(e) if e.is_auth_error() => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| HttpError::Other(anyhow::anyhow!("quota unavailable"))))
}

pub fn snapshot_from_quota_summary(summary: &Value, account: Option<&str>) -> ProviderSnapshot {
    let groups = summary
        .pointer("/response/groups")
        .or_else(|| summary.get("groups"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut by_id: HashMap<String, Value> = HashMap::new();
    for group in &groups {
        if let Some(buckets) = group.get("buckets").and_then(|v| v.as_array()) {
            for bucket in buckets {
                if let Some(id) = bucket.get("bucketId").and_then(|v| v.as_str())
                    && !by_id.contains_key(id)
                {
                    by_id.insert(id.to_string(), bucket.clone());
                }
            }
        }
    }

    let mut meters = Vec::new();
    for (bucket_id, title, window_seconds) in SUMMARY_BUCKETS {
        let Some(bucket) = by_id.get(*bucket_id) else {
            continue;
        };
        let Some(remaining) = bucket.get("remainingFraction").and_then(|v| v.as_f64()) else {
            continue;
        };
        let display = bucket
            .get("displayName")
            .and_then(|v| v.as_str())
            .map(|name| format!("{}{name}", group_prefix(bucket_id)))
            .unwrap_or_else(|| (*title).to_string());
        let reset_at = bucket.get("resetTime").and_then(coerce_unix_seconds);
        meters.push(meter_from_remaining_fraction(
            bucket_id,
            &display,
            remaining,
            reset_at,
            Some(*window_seconds),
        ));
    }

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: None,
        account: account.map(str::to_string),
        plan: None,
        meters,
    }
}

fn group_prefix(bucket_id: &str) -> &'static str {
    if bucket_id.starts_with("gemini-") {
        "Gemini · "
    } else if bucket_id.starts_with("3p-") {
        "Claude/GPT · "
    } else {
        ""
    }
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
    fn four_buckets() {
        let summary = serde_json::json!({
            "groups": [
                {
                    "buckets": [
                        {
                            "bucketId": "gemini-weekly",
                            "displayName": "Weekly Limit",
                            "remainingFraction": 0.98,
                            "resetTime": "2026-07-31T14:47:51Z"
                        },
                        {
                            "bucketId": "gemini-5h",
                            "displayName": "Five Hour Limit",
                            "remainingFraction": 1,
                            "resetTime": "2026-07-27T04:32:50Z"
                        }
                    ]
                },
                {
                    "buckets": [
                        { "bucketId": "3p-5h", "displayName": "Five Hour Limit", "remainingFraction": 0.5 },
                        { "bucketId": "3p-weekly", "displayName": "Weekly Limit", "remainingFraction": 1 }
                    ]
                }
            ]
        });
        let snap = snapshot_from_quota_summary(&summary, Some("ag@example.com"));
        assert_eq!(snap.meters.len(), 4);
        assert_eq!(snap.meters[0].id, "gemini-5h");
        assert_eq!(snap.meters[0].percent.unwrap(), 0.0);
        assert_eq!(snap.meters[2].id, "3p-5h");
        assert!((snap.meters[2].percent.unwrap() - 0.5).abs() < 1e-9);
    }
}
