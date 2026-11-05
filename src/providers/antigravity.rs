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
// Public OAuth application metadata embedded in the agy client.
const NATIVE_OAUTH_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const NATIVE_OAUTH_CLIENT_SECRET: &str = "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";
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
#[derive(Clone, Copy)]
enum AuthSource {
    AgyKeyring,
    AppStateDb(&'static str),
    GeminiFile,
}

impl AuthSource {
    fn description(self) -> String {
        match self {
            Self::AgyKeyring => "agy keyring (service=gemini, account=antigravity)".into(),
            Self::AppStateDb(path) => format!("Antigravity app state DB ({path})"),
            Self::GeminiFile => "~/.gemini/oauth_creds.json".into(),
        }
    }
}

struct Credentials {
    access_token: String,
    refresh_token: String,
    expires_at: Option<f64>,
    source: AuthSource,
}

fn fetch_inner(client: &HttpClient) -> Result<ProviderSnapshot, FetchErr> {
    try_credentials(
        credential_readers().into_iter().map(|reader| reader()),
        |credentials| fetch_with_credentials(client, credentials),
    )
}

type CredentialReader = fn() -> Result<Option<Credentials>, String>;

fn credential_readers() -> [CredentialReader; 3] {
    [read_agy_keyring, read_app_state_db, read_gemini_file]
}

fn try_credentials<I, F>(candidates: I, mut fetch: F) -> Result<ProviderSnapshot, FetchErr>
where
    I: IntoIterator<Item = Result<Option<Credentials>, String>>,
    F: FnMut(Credentials) -> Result<ProviderSnapshot, FetchErr>,
{
    let mut failures = Vec::new();
    for candidate in candidates {
        let credentials = match candidate {
            Ok(Some(credentials)) => credentials,
            Ok(None) => continue,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        match fetch(credentials) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) if error.status == SnapshotStatus::Auth => failures.push(error.message),
            Err(error) => return Err(error),
        }
    }
    Err(FetchErr {
        status: SnapshotStatus::Auth,
        message: if failures.is_empty() {
            "Antigravity credentials not found. Sign in with agy or the Antigravity app.".into()
        } else {
            failures.join("; ")
        },
    })
}

fn fetch_with_credentials(
    client: &HttpClient,
    credentials: Credentials,
) -> Result<ProviderSnapshot, FetchErr> {
    let source = credentials.source;
    let access_token = resolve_access_token(client, credentials).map_err(|message| FetchErr {
        status: SnapshotStatus::Auth,
        message,
    })?;
    let project = fetch_project(client, &access_token).map_err(|error| FetchErr {
        status: if error.is_auth_error() {
            SnapshotStatus::Auth
        } else {
            SnapshotStatus::Error
        },
        message: error.to_string(),
    })?;
    let summary = fetch_quota(client, &access_token, project.as_deref()).map_err(|error| {
        let status = if error.is_auth_error() {
            SnapshotStatus::Auth
        } else {
            SnapshotStatus::Error
        };
        let message = if error.is_auth_error() {
            "Antigravity token was rejected. Sign in again with Antigravity.".into()
        } else {
            error.to_string()
        };
        FetchErr { status, message }
    })?;
    let account = if matches!(source, AuthSource::GeminiFile) {
        read_active_google_account()
    } else {
        None
    };
    Ok(snapshot_from_quota_summary(&summary, account.as_deref()))
}

pub(crate) fn auth_description() -> Result<String, String> {
    let now = now_secs();
    let mut failures = Vec::new();
    for reader in credential_readers() {
        match reader() {
            Ok(Some(credentials)) => {
                let refresh_available = !credentials.refresh_token.is_empty();
                if has_usable_access_token(&credentials, now) || refresh_available {
                    let expiry = credentials
                        .expires_at
                        .map(|value| format!("expires_at_unix={value:.0}"))
                        .unwrap_or_else(|| "expiry=unknown".into());
                    let refresh = if refresh_available {
                        "refresh=available"
                    } else {
                        "refresh=unavailable"
                    };
                    return Ok(format!(
                        "{}; access_token=present; {expiry}; {refresh}",
                        credentials.source.description()
                    ));
                }
                failures.push(format!(
                    "{} has no usable access or refresh token",
                    credentials.source.description()
                ));
            }
            Ok(None) => {}
            Err(error) => failures.push(error),
        }
    }
    Err(if failures.is_empty() {
        "Antigravity credentials not found in agy keyring, app state DBs, or Gemini OAuth file."
            .into()
    } else {
        failures.join("; ")
    })
}

fn resolve_access_token(client: &HttpClient, credentials: Credentials) -> Result<String, String> {
    let now = now_secs();
    if has_usable_access_token(&credentials, now) {
        return Ok(credentials.access_token);
    }
    if credentials.refresh_token.is_empty() {
        return Err(format!(
            "{} has no usable access token or refresh token",
            credentials.source.description()
        ));
    }
    let (client_id, client_secret) = resolve_oauth_client();
    let mut form = HashMap::new();
    form.insert("client_id", client_id.as_str());
    form.insert("client_secret", client_secret.as_str());
    form.insert("refresh_token", credentials.refresh_token.as_str());
    form.insert("grant_type", "refresh_token");
    let refreshed: Value = client.post_form(GOOGLE_TOKEN_URL, &form).map_err(|error| {
        if error.is_auth_error() {
            "Antigravity refresh token was rejected. Sign in again with Antigravity.".to_string()
        } else {
            error.to_string()
        }
    })?;
    let access_token = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .map(normalize_bearer)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Antigravity token refresh returned no access token.".to_string())?;
    if refreshed
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty() && token != credentials.refresh_token)
    {
        return Err(
            "Google rotated the Antigravity refresh token, but native credentials are read-only; refusing to discard the replacement."
                .into(),
        );
    }
    Ok(access_token)
}

fn has_usable_access_token(credentials: &Credentials, now: f64) -> bool {
    !credentials.access_token.is_empty()
        && credentials
            .expires_at
            .map(|expiry| expiry > now + EXPIRY_SKEW)
            .unwrap_or(true)
}

fn read_agy_keyring() -> Result<Option<Credentials>, String> {
    let output = match Command::new("secret-tool")
        .args(["lookup", "service", "gemini", "username", "antigravity"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Ok(None),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let payload = parse_secret_text(text)?;
    Ok(credentials_from_json(&payload, AuthSource::AgyKeyring))
}

fn read_app_state_db() -> Result<Option<Credentials>, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = [
        (
            home.join(".config/Antigravity/User/globalStorage/state.vscdb"),
            "~/.config/Antigravity/User/globalStorage/state.vscdb",
        ),
        (
            home.join(".config/Antigravity IDE/User/globalStorage/state.vscdb"),
            "~/.config/Antigravity IDE/User/globalStorage/state.vscdb",
        ),
    ];
    for (path, display_path) in candidates {
        if !path.exists() {
            continue;
        }
        let connection = match rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(connection) => connection,
            Err(_) => continue,
        };
        let value = connection
            .query_row(
                "SELECT value FROM ItemTable WHERE key = ?1",
                ["antigravityUnifiedStateSync.oauthToken"],
                |row| row.get::<_, String>(0),
            )
            .ok();
        if let Some(value) = value
            && let Some(credentials) = parse_app_oauth_token(&value)
        {
            return Ok(Some(Credentials {
                source: AuthSource::AppStateDb(display_path),
                ..credentials
            }));
        }
    }
    Ok(None)
}

fn parse_app_oauth_token(value: &str) -> Option<Credentials> {
    let outer = B64.decode(value.trim()).ok()?;
    for (field, wire, wrapper) in protobuf_fields(&outer)? {
        if field != 1 || wire != 2 {
            continue;
        }
        let wrapper_fields = protobuf_fields(wrapper)?;
        let sentinel = wrapper_fields
            .iter()
            .find(|(field, wire, _)| *field == 1 && *wire == 2)
            .and_then(|(_, _, bytes)| std::str::from_utf8(bytes).ok());
        if sentinel != Some("oauthTokenInfoSentinelKey") {
            continue;
        }
        let payload = wrapper_fields
            .iter()
            .find(|(field, wire, _)| *field == 2 && *wire == 2)
            .map(|(_, _, bytes)| bytes)?;
        let payload_fields = protobuf_fields(payload)?;
        let encoded = payload_fields
            .iter()
            .find(|(field, wire, _)| *field == 1 && *wire == 2)
            .and_then(|(_, _, bytes)| std::str::from_utf8(bytes).ok())?;
        let token_info = B64.decode(encoded).ok()?;
        let fields = protobuf_fields(&token_info)?;
        let field_text = |number| {
            fields
                .iter()
                .find(|(field, wire, _)| *field == number && *wire == 2)
                .and_then(|(_, _, bytes)| std::str::from_utf8(bytes).ok())
        };
        let expiry = fields
            .iter()
            .find(|(field, wire, _)| *field == 4 && *wire == 2)
            .and_then(|(_, _, bytes)| protobuf_fields(bytes))
            .and_then(|timestamp| {
                timestamp
                    .iter()
                    .find(|(field, wire, _)| *field == 1 && *wire == 0)
                    .and_then(|(_, _, bytes)| decode_varint(bytes))
                    .map(|(seconds, _)| seconds as f64)
            });
        let access_token = field_text(1).map(normalize_bearer).unwrap_or_default();
        let refresh_token = field_text(3).map(normalize_bearer).unwrap_or_default();
        if access_token.is_empty() && refresh_token.is_empty() {
            return None;
        }
        return Some(Credentials {
            access_token,
            refresh_token,
            expires_at: expiry,
            source: AuthSource::AgyKeyring,
        });
    }
    None
}

fn protobuf_fields(mut bytes: &[u8]) -> Option<Vec<(u32, u8, &[u8])>> {
    let mut fields = Vec::new();
    while !bytes.is_empty() {
        let (key, consumed) = decode_varint(bytes)?;
        bytes = &bytes[consumed..];
        let field = (key >> 3) as u32;
        let wire = (key & 7) as u8;
        if field == 0 {
            return None;
        }
        match wire {
            0 => {
                let (_, consumed) = decode_varint(bytes)?;
                fields.push((field, wire, &bytes[..consumed]));
                bytes = &bytes[consumed..];
            }
            1 => {
                if bytes.len() < 8 {
                    return None;
                }
                fields.push((field, wire, &bytes[..8]));
                bytes = &bytes[8..];
            }
            2 => {
                let (length, consumed) = decode_varint(bytes)?;
                bytes = &bytes[consumed..];
                let length = usize::try_from(length).ok()?;
                if bytes.len() < length {
                    return None;
                }
                fields.push((field, wire, &bytes[..length]));
                bytes = &bytes[length..];
            }
            5 => {
                if bytes.len() < 4 {
                    return None;
                }
                fields.push((field, wire, &bytes[..4]));
                bytes = &bytes[4..];
            }
            _ => return None,
        }
    }
    Some(fields)
}

fn decode_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut result = 0u64;
    for (index, byte) in bytes.iter().copied().enumerate().take(10) {
        result |= u64::from(byte & 0x7f).checked_shl((index * 7) as u32)?;
        if byte & 0x80 == 0 {
            return Some((result, index + 1));
        }
    }
    None
}

fn read_gemini_file() -> Result<Option<Credentials>, String> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gemini/oauth_creds.json");
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|_| "Gemini OAuth credential file is not valid JSON.".to_string())?;
    Ok(credentials_from_json(&payload, AuthSource::GeminiFile))
}

fn credentials_from_json(payload: &Value, source: AuthSource) -> Option<Credentials> {
    let token = payload
        .get("token")
        .filter(|value| value.is_object())
        .unwrap_or(payload);
    Some(Credentials {
        access_token: token
            .get("access_token")
            .and_then(Value::as_str)
            .map(normalize_bearer)
            .unwrap_or_default(),
        refresh_token: token
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(normalize_bearer)
            .unwrap_or_default(),
        expires_at: token
            .get("expiry")
            .or_else(|| token.get("expiry_date"))
            .and_then(coerce_unix_seconds),
        source,
    })
    .filter(|credentials| {
        !credentials.access_token.is_empty() || !credentials.refresh_token.is_empty()
    })
}

fn parse_secret_text(text: &str) -> Result<Value, String> {
    let raw = if let Some(encoded) = text.trim().strip_prefix("go-keyring-base64:") {
        String::from_utf8(
            B64.decode(encoded.trim())
                .map_err(|_| "agy keyring payload is not valid base64.".to_string())?,
        )
        .map_err(|_| "agy keyring payload is not valid UTF-8.".to_string())?
    } else {
        text.trim().to_string()
    };
    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        return Ok(value);
    }
    if let Some(token) = raw.strip_prefix("Bearer ") {
        return Ok(serde_json::json!({ "access_token": token.trim() }));
    }
    Err("agy keyring payload is not valid Antigravity JSON.".into())
}

fn resolve_oauth_client() -> (String, String) {
    let client_id = std::env::var("USAGENOMETER_GOOGLE_CLIENT_ID")
        .or_else(|_| std::env::var("AGY_CLIENT_ID"))
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| NATIVE_OAUTH_CLIENT_ID.to_string());
    let client_secret = std::env::var("USAGENOMETER_GOOGLE_CLIENT_SECRET")
        .or_else(|_| std::env::var("AGY_CLIENT_SECRET"))
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| NATIVE_OAUTH_CLIENT_SECRET.to_string());
    (client_id, client_secret)
}

fn read_active_google_account() -> Option<String> {
    let path = dirs::home_dir()?.join(".gemini/google_accounts.json");
    let raw = fs::read_to_string(path).ok()?;
    let payload: Value = serde_json::from_str(&raw).ok()?;
    payload
        .get("active")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn fetch_project(client: &HttpClient, token: &str) -> Result<Option<String>, HttpError> {
    let body = serde_json::json!({ "metadata": { "ideType": "ANTIGRAVITY" } });
    let response = post_cloud_code(client, token, "/v1internal:loadCodeAssist", &body)?;
    Ok(response
        .get("cloudaicompanionProject")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|project| !project.is_empty()))
}

fn fetch_quota(
    client: &HttpClient,
    token: &str,
    project: Option<&str>,
) -> Result<Value, HttpError> {
    let body = project
        .map(|project| serde_json::json!({ "project": project }))
        .unwrap_or_else(|| serde_json::json!({}));
    post_cloud_code(client, token, QUOTA_PATH, &body)
}

fn post_cloud_code(
    client: &HttpClient,
    token: &str,
    path: &str,
    body: &Value,
) -> Result<Value, HttpError> {
    let mut last = None;
    for base in CLOUD_CODE_BASES {
        let url = format!("{base}{path}");
        match client.post_json(
            &url,
            body,
            &[
                ("Accept", "application/json"),
                ("Authorization", &format!("Bearer {token}")),
                ("User-Agent", USER_AGENT),
            ],
        ) {
            Ok(value) => return Ok(value),
            Err(error) if error.is_auth_error() => return Err(error),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| HttpError::Other(anyhow::anyhow!("Cloud Code unavailable"))))
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
        error: if meters.is_empty() {
            Some("Antigravity account connected, but no quota buckets were returned.".into())
        } else {
            None
        },
        account: account.map(str::to_string),
        plan: None,
        meters,
        stale_age_secs: None,
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

    #[test]
    fn native_app_oauth_payload_decodes_tokens_and_expiry() {
        fn varint(mut value: u64) -> Vec<u8> {
            let mut bytes = Vec::new();
            while value >= 0x80 {
                bytes.push((value as u8) | 0x80);
                value >>= 7;
            }
            bytes.push(value as u8);
            bytes
        }

        fn bytes_field(field: u8, value: &[u8]) -> Vec<u8> {
            let mut encoded = vec![field << 3 | 2];
            encoded.extend(varint(value.len() as u64));
            encoded.extend(value);
            encoded
        }

        fn number_field(field: u8, value: u64) -> Vec<u8> {
            let mut encoded = vec![field << 3];
            encoded.extend(varint(value));
            encoded
        }

        let expiry = number_field(1, 2_000_000_000);
        let mut token_info = bytes_field(1, b"native-access-token");
        token_info.extend(bytes_field(2, b"Bearer"));
        token_info.extend(bytes_field(3, b"native-refresh-token"));
        token_info.extend(bytes_field(4, &expiry));
        let encoded_token_info = B64.encode(token_info);
        let payload = bytes_field(1, encoded_token_info.as_bytes());
        let mut wrapper = bytes_field(1, b"oauthTokenInfoSentinelKey");
        wrapper.extend(bytes_field(2, &payload));
        let native_value = B64.encode(bytes_field(1, &wrapper));

        let parsed = parse_app_oauth_token(&native_value).unwrap();
        assert_eq!(parsed.access_token, "native-access-token");
        assert_eq!(parsed.refresh_token, "native-refresh-token");
        assert_eq!(parsed.expires_at, Some(2_000_000_000.0));
    }

    #[test]
    fn agy_keyring_json_payload_is_parsed() {
        let payload = serde_json::json!({
            "access_token": "agy-access-token",
            "refresh_token": "agy-refresh-token",
            "expiry": 2_000_000_000.0,
            "token_type": "Bearer"
        });
        let credentials = credentials_from_json(&payload, AuthSource::AgyKeyring).unwrap();
        assert_eq!(credentials.access_token, "agy-access-token");
        assert_eq!(credentials.refresh_token, "agy-refresh-token");
        assert_eq!(credentials.expires_at, Some(2_000_000_000.0));
    }

    #[test]
    fn access_expiry_accounts_for_refresh_skew() {
        let expired = Credentials {
            access_token: "expired-token".into(),
            refresh_token: String::new(),
            expires_at: Some(100.0),
            source: AuthSource::AgyKeyring,
        };
        assert!(!has_usable_access_token(&expired, 100.0));
        let mut fresh = Credentials {
            expires_at: Some(160.0),
            ..expired
        };
        assert!(!has_usable_access_token(&fresh, 100.0));
        fresh.expires_at = Some(161.0);
        assert!(has_usable_access_token(&fresh, 100.0));
    }

    #[test]
    fn empty_quota_does_not_fabricate_meters() {
        let snapshot = snapshot_from_quota_summary(&serde_json::json!({}), None);
        assert!(snapshot.meters.is_empty());
        assert!(snapshot.error.is_some());
    }
}
