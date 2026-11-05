//! Go `/zen/go/v1/usage` reports 0–100 percentages.
//! API contract: https://github.com/anomalyco/opencode/blob/dev/packages/console/app/src/routes/zen/go/v1/usage.ts

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_number, coerce_unix_seconds, create_meter,
};

const GO_ID: &str = "opencode-go";
const GO_LABEL: &str = "OpenCode Go";
const GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

struct Credential {
    key: String,
    source: &'static str,
}

pub fn auth_path() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        });
    data.join("opencode/auth.json")
}

pub fn auth_description_go() -> Result<String, String> {
    auth_description("opencode-go")
}

pub fn auth_description_zen() -> Result<String, String> {
    super::opencode_console::auth_description()
}

fn auth_description(native_id: &str) -> Result<String, String> {
    resolve_credential(native_id).map(|c| format!("{} API key from {}", native_id, c.source))
}

fn resolve_credential(native_id: &str) -> Result<Credential, String> {
    // Native Auth.all falls back to the file only when the override is invalid JSON.
    let override_auth = std::env::var("OPENCODE_AUTH_CONTENT")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let env_key = std::env::var("OPENCODE_API_KEY").ok();
    if override_auth.is_none() {
        use super::opencode_console::{NativeCredential, native_credential};
        match native_credential(&auth_path().with_file_name("opencode.db"), native_id)? {
            NativeCredential::Selected(value) => {
                let key = value
                    .get("key")
                    .and_then(Value::as_str)
                    .filter(|_| {
                        matches!(
                            value.get("type").and_then(Value::as_str),
                            Some("key" | "api")
                        )
                    })
                    .ok_or_else(|| format!("invalid {native_id} API key; reconnect in OpenCode"))?;
                return validated_credential(key, "OpenCode opencode.db", native_id);
            }
            NativeCredential::Missing => {
                return credential_from_sources(
                    None,
                    "OpenCode opencode.db",
                    native_id,
                    env_key.as_deref(),
                );
            }
            NativeCredential::Unavailable => {}
        }
    }
    let (auth, source) = match override_auth {
        Some(auth) => (Some(auth), "OPENCODE_AUTH_CONTENT"),
        None => (
            fs::read(auth_path())
                .ok()
                .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok()),
            "OpenCode auth.json",
        ),
    };
    credential_from_sources(auth.as_ref(), source, native_id, env_key.as_deref())
}

fn credential_from_sources(
    auth: Option<&Value>,
    source: &'static str,
    native_id: &str,
    env_key: Option<&str>,
) -> Result<Credential, String> {
    // Provider loading merges stored API keys after environment keys.
    let entry = auth.and_then(|v| v.get(native_id));
    let native_key = entry
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("api"))
        .and_then(|v| v.get("key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (key, source) = match native_key {
        Some(key) => (key, source),
        None => match env_key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(key) => (key, "OPENCODE_API_KEY"),
            None => {
                return Err(if entry.is_some() {
                    format!(
                        "invalid {native_id} credential; use /connect in OpenCode to store an API key"
                    )
                } else {
                    format!(
                        "missing {native_id} API key; use /connect in OpenCode or set OPENCODE_API_KEY"
                    )
                });
            }
        },
    };
    validated_credential(key, source, native_id)
}

fn validated_credential(
    key: &str,
    source: &'static str,
    native_id: &str,
) -> Result<Credential, String> {
    if key.is_empty()
        || key
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(format!(
            "invalid {native_id} API key; reconnect in OpenCode"
        ));
    }
    Ok(Credential {
        key: key.to_string(),
        source,
    })
}

pub fn fetch_go(client: &HttpClient) -> ProviderSnapshot {
    let credential = match resolve_credential("opencode-go") {
        Ok(credential) => credential,
        Err(error) => return ProviderSnapshot::fail(GO_ID, GO_LABEL, SnapshotStatus::Auth, error),
    };
    let authorization = format!("Bearer {}", credential.key);
    match client.get_json::<Value>(GO_USAGE_URL, &[("Authorization", authorization.as_str())]) {
        Ok(body) => snapshot_from_go_usage(&body),
        Err(error) => go_fetch_error(error),
    }
}

pub fn fetch_zen(client: &HttpClient) -> ProviderSnapshot {
    super::opencode_console::fetch(client)
}

fn go_fetch_error(error: HttpError) -> ProviderSnapshot {
    match error {
        HttpError::Status {
            status: 403,
            body: Some(body),
            ..
        } if body.pointer("/error/type").and_then(Value::as_str) == Some("EntitlementError") => {
            ProviderSnapshot::fail(
                GO_ID,
                GO_LABEL,
                SnapshotStatus::Error,
                "OpenCode accepted the API key but requires a Go subscription; check the selected workspace in the OpenCode console",
            )
        }
        HttpError::Status {
            status: 401 | 403, ..
        } => ProviderSnapshot::fail(
            GO_ID,
            GO_LABEL,
            SnapshotStatus::Auth,
            "OpenCode rejected the Go API key; reconnect with /connect in OpenCode",
        ),
        HttpError::Status { status, .. } => ProviderSnapshot::fail(
            GO_ID,
            GO_LABEL,
            SnapshotStatus::Error,
            format!("OpenCode Go usage HTTP {status}"),
        ),
        HttpError::Other(_) => ProviderSnapshot::fail(
            GO_ID,
            GO_LABEL,
            SnapshotStatus::Error,
            "OpenCode Go usage request failed or returned invalid JSON",
        ),
    }
}

fn snapshot_from_go_usage(body: &Value) -> ProviderSnapshot {
    let mut snapshot = ProviderSnapshot::ok(GO_ID, GO_LABEL);
    for (id, title, window_seconds) in [
        ("rolling", "5 hour usage", Some(5.0 * 3600.0)),
        ("weekly", "Weekly usage", Some(7.0 * 86400.0)),
        ("monthly", "Monthly usage", None),
    ] {
        let Some(usage) = body.get("usage").and_then(|v| v.get(id)) else {
            return invalid_go_usage();
        };
        let Some(percent) = usage
            .get("percent")
            .and_then(coerce_number)
            .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
        else {
            return invalid_go_usage();
        };
        let Some(reset_at) = usage
            .get("resetsAt")
            .and_then(coerce_unix_seconds)
            .filter(|v| v.is_finite())
        else {
            return invalid_go_usage();
        };
        snapshot.meters.push(create_meter(
            id,
            title,
            Some(percent / 100.0),
            None,
            Some(percent),
            None,
            Some(100.0),
            "percent",
            Some(reset_at),
            None,
            window_seconds,
        ));
    }
    snapshot
}

fn invalid_go_usage() -> ProviderSnapshot {
    ProviderSnapshot::fail(
        GO_ID,
        GO_LABEL,
        SnapshotStatus::Error,
        "OpenCode Go returned incomplete or invalid usage windows",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn go_usage() -> Value {
        json!({"usage": {
            "rolling": {"status": "ok", "percent": 0.5, "resetsAt": "2026-10-09T05:00:00Z"},
            "weekly": {"status": "ok", "percent": 1, "resetsAt": "2026-10-12T00:00:00Z"},
            "monthly": {"status": "rate-limited", "percent": 100, "resetsAt": "2026-11-01T00:00:00Z"}
        }})
    }

    #[test]
    fn go_percentages_always_use_zero_to_one_hundred_scale() {
        let snapshot = snapshot_from_go_usage(&go_usage());
        assert_eq!(snapshot.status, SnapshotStatus::Ok);
        assert_eq!(snapshot.id, GO_ID);
        assert_eq!(snapshot.meters.len(), 3);
        let rolling = &snapshot.meters[0];
        assert_eq!(rolling.percent, Some(0.005));
        assert_eq!(rolling.used, Some(0.5));
        assert_eq!(rolling.left, Some(99.5));
        assert_eq!(rolling.unit, "percent");
        assert_eq!(rolling.window_seconds, Some(18000.0));
        assert_eq!(rolling.reset_at, Some(1791522000.0));
        assert_eq!(snapshot.meters[1].percent, Some(0.01));
        assert_eq!(snapshot.meters[2].percent, Some(1.0));
        assert_eq!(snapshot.meters[2].left, Some(0.0));
        assert_eq!(snapshot.meters[2].window_seconds, None);
    }

    #[test]
    fn invalid_go_response_never_becomes_empty_success() {
        for bad in [json!({}), json!({"usage": {}}), json!({"balance": 12})] {
            let snapshot = snapshot_from_go_usage(&bad);
            assert_eq!(snapshot.status, SnapshotStatus::Error);
            assert!(snapshot.meters.is_empty());
        }
        for bad_percent in [json!(-1), json!(101), json!(null), json!("NaN")] {
            let mut body = go_usage();
            body["usage"]["weekly"]["percent"] = bad_percent;
            assert_eq!(snapshot_from_go_usage(&body).status, SnapshotStatus::Error);
        }
        let mut body = go_usage();
        body["usage"]["rolling"]["resetsAt"] = json!("not a date");
        assert_eq!(snapshot_from_go_usage(&body).status, SnapshotStatus::Error);
    }

    #[test]
    fn go_key_selection_never_uses_the_zen_model_key() {
        let auth = json!({
            "opencode-go": {"type": "api", "key": "go-secret"},
            "opencode": {"type": "api", "key": "zen-secret"}
        });
        let go = credential_from_sources(Some(&auth), "native", "opencode-go", Some("env-secret"))
            .unwrap();
        assert_eq!(go.key, "go-secret");
        let model_only = json!({"opencode": {"type": "api", "key": "zen-secret"}});
        assert!(credential_from_sources(Some(&model_only), "native", "opencode-go", None).is_err());
        assert_eq!(
            credential_from_sources(
                Some(&model_only),
                "native",
                "opencode-go",
                Some("env-secret")
            )
            .unwrap()
            .key,
            "env-secret"
        );
    }

    #[test]
    fn invalid_auth_errors_do_not_expose_keys() {
        let auth = json!({"opencode-go": {"type": "api", "key": "private\nsecret"}});
        let error = credential_from_sources(Some(&auth), "native", "opencode-go", None)
            .err()
            .unwrap();
        assert!(!error.contains("private"));
        assert!(!error.contains("secret"));
        let oauth = json!({"opencode-go": {"type": "oauth", "access": "private-secret"}});
        assert!(credential_from_sources(Some(&oauth), "native", "opencode-go", None).is_err());
    }

    #[test]
    fn entitlement_failure_is_not_reported_as_rejected_auth() {
        let snapshot = go_fetch_error(HttpError::Status {
            status: 403,
            message: "private-secret".into(),
            body: Some(json!({"error": {"type": "EntitlementError", "message": "private-secret"}})),
        });
        assert_eq!(snapshot.status, SnapshotStatus::Error);
        assert!(
            snapshot
                .error
                .as_deref()
                .unwrap()
                .contains("Go subscription")
        );
        assert!(
            !snapshot
                .error
                .as_deref()
                .unwrap()
                .contains("private-secret")
        );
        let rejected = go_fetch_error(HttpError::Status {
            status: 401,
            message: "private-secret".into(),
            body: None,
        });
        assert_eq!(rejected.status, SnapshotStatus::Auth);
        assert!(
            !rejected
                .error
                .as_deref()
                .unwrap()
                .contains("private-secret")
        );
    }
}
