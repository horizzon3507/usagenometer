//! Console billing uses account authentication, not Zen model API keys.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use super::types::{ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, create_meter};
use crate::http::{HttpClient, HttpError};

const ID: &str = "opencode-zen";
const LABEL: &str = "OpenCode Zen";
const CONSOLE: &str = "https://opencode.ai/console";
const ORGS_URL: &str = "https://opencode.ai/console/api/orgs";
const BILLING_URL: &str = "https://opencode.ai/console/api/billing/status";
const LOGIN: &str = "opencode auth login opencode --method device";

struct Session {
    authorization: String,
    org_id: Option<String>,
    source: &'static str,
}

pub(super) enum NativeCredential {
    Unavailable,
    Missing,
    Selected(Value),
}

pub(super) fn auth_description() -> Result<String, String> {
    resolve_session().map(|s| format!("OpenCode Console session from {}", s.source))
}

pub(super) fn fetch(client: &HttpClient) -> ProviderSnapshot {
    match resolve_session() {
        Ok(session) => fetch_session(client, &session),
        Err(error) => ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Auth, error),
    }
}

fn resolve_session() -> Result<Session, String> {
    let org = std::env::var("OPENCODE_CONSOLE_ORG_ID").ok();
    if let Some(token) = std::env::var("OPENCODE_CONSOLE_TOKEN")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return bearer_session(&token, org.as_deref(), "OPENCODE_CONSOLE_TOKEN");
    }
    let data = super::opencode::auth_path();
    if let NativeCredential::Selected(value) =
        native_credential(&data.with_file_name("opencode.db"), "opencode")?
    {
        let mut session = oauth_session(&value, "OpenCode opencode.db")?;
        if let Some(org) = org.as_deref() {
            session.org_id = Some(validate_org(org)?.to_owned());
        }
        return Ok(session);
    }
    Err(format!(
        "missing OpenCode Console login; run {LOGIN}. Zen model API keys do not authorize billing"
    ))
}

pub(super) fn native_credential(
    path: &Path,
    integration: &str,
) -> Result<NativeCredential, String> {
    if !path.exists() {
        return Ok(NativeCredential::Unavailable);
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "cannot read OpenCode account database".to_owned())?;
    let has_credentials: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "cannot inspect OpenCode account database".to_owned())?;
    if !has_credentials {
        return Ok(NativeCredential::Unavailable);
    }
    let active: Option<Value> = db.query_row(
        "SELECT value FROM credential WHERE integration_id=?1 AND active=1 ORDER BY time_updated DESC LIMIT 1",
        [integration], native_value,
    ).optional().map_err(|_| "invalid or unreadable active OpenCode credential".to_owned())?;
    let value =
        if active.is_some() {
            active
        } else {
            let mut query = db.prepare(
            "SELECT value FROM credential WHERE integration_id=?1 AND active IS NULL LIMIT 2",
        ).map_err(|_| "cannot read OpenCode credentials".to_owned())?;
            let mut rows = query
                .query([integration])
                .map_err(|_| "cannot read OpenCode credentials".to_owned())?;
            let first = rows
                .next()
                .map_err(|_| "cannot read OpenCode credentials".to_owned())?
                .map(native_value)
                .transpose()
                .map_err(|_| "invalid OpenCode credential".to_owned())?;
            if rows
                .next()
                .map_err(|_| "cannot read OpenCode credentials".to_owned())?
                .is_some()
            {
                return Err(format!(
                    "no selected {integration} account; select one in OpenCode"
                ));
            }
            first
        };
    Ok(match value {
        Some(value) => NativeCredential::Selected(value),
        None => NativeCredential::Missing,
    })
}

fn native_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let raw = row.get_ref(0)?.as_str()?;
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn oauth_session(value: &Value, source: &'static str) -> Result<Session, String> {
    if value.get("type").and_then(Value::as_str) != Some("oauth") {
        return Err(format!(
            "missing OpenCode Console OAuth login; run {LOGIN}. Model API keys do not authorize billing"
        ));
    }
    if let Some(server) = value.pointer("/metadata/server").and_then(Value::as_str) {
        if server.trim_end_matches('/') != CONSOLE {
            return Err("OpenCode account belongs to a different Console server".to_owned());
        }
    }
    let expires = value
        .get("expires")
        .and_then(coerce_unix_seconds)
        .filter(|v| v.is_finite())
        .ok_or_else(|| "invalid OpenCode Console credential expiry".to_owned())?;
    if expires <= time::OffsetDateTime::now_utc().unix_timestamp() as f64 {
        return Err(format!(
            "expired OpenCode Console session; let OpenCode refresh it or run {LOGIN}"
        ));
    }
    let token = value
        .get("access")
        .and_then(Value::as_str)
        .ok_or_else(|| "invalid OpenCode Console access token".to_owned())?;
    let org = value.pointer("/metadata/orgID").and_then(Value::as_str);
    bearer_session(token, org, source)
}

fn bearer_session(token: &str, org: Option<&str>, source: &'static str) -> Result<Session, String> {
    if token.is_empty()
        || token
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err("invalid OpenCode Console access token".to_owned());
    }
    if crate::jwt::jwt_exp(token)
        .is_some_and(|expiry| expiry <= time::OffsetDateTime::now_utc().unix_timestamp() as f64)
    {
        return Err(format!(
            "expired OpenCode Console access token; run {LOGIN}"
        ));
    }
    Ok(Session {
        authorization: format!("Bearer {token}"),
        org_id: org.map(validate_org).transpose()?.map(str::to_owned),
        source,
    })
}

fn validate_org(org: &str) -> Result<&str, String> {
    if (org.starts_with("org_") || org.starts_with("wrk_"))
        && org
            .bytes()
            .all(|b| !b.is_ascii_whitespace() && !b.is_ascii_control())
        && org.len() > 4
    {
        Ok(org)
    } else {
        Err("invalid OpenCode Console organization ID".to_owned())
    }
}

fn fetch_session(client: &HttpClient, session: &Session) -> ProviderSnapshot {
    let discovered_org;
    let org = match &session.org_id {
        Some(org) => org.as_str(),
        None => {
            let organizations = match client
                .get_json::<Value>(ORGS_URL, &[("Authorization", &session.authorization)])
            {
                Ok(body) => body,
                Err(error) => return fetch_error(error),
            };
            discovered_org = match select_org(&organizations) {
                Ok(org) => org.to_owned(),
                Err((status, error)) => return ProviderSnapshot::fail(ID, LABEL, status, error),
            };
            discovered_org.as_str()
        }
    };
    match client.get_json::<Value>(
        BILLING_URL,
        &[("Authorization", &session.authorization), ("x-org-id", org)],
    ) {
        Ok(body) => snapshot_from_billing(&body),
        Err(error) => fetch_error(error),
    }
}

fn select_org(body: &Value) -> Result<&str, (SnapshotStatus, String)> {
    let organizations = body.as_array().ok_or_else(|| {
        (
            SnapshotStatus::Error,
            "invalid OpenCode Console organization response".to_owned(),
        )
    })?;
    match organizations.as_slice() {
        [org] => validate_org(org.get("id").and_then(Value::as_str).unwrap_or(""))
            .map_err(|error| (SnapshotStatus::Error, error)),
        [] => Err((SnapshotStatus::Auth, "OpenCode Console account has no organizations".to_owned())),
        _ => Err((SnapshotStatus::Auth, "multiple OpenCode Console organizations; select one in OpenCode or set OPENCODE_CONSOLE_ORG_ID".to_owned())),
    }
}

fn fetch_error(error: HttpError) -> ProviderSnapshot {
    match error {
        HttpError::Status { status: 401, .. } => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            format!("OpenCode Console rejected the session; run {LOGIN}"),
        ),
        HttpError::Status { status: 403, .. } => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            "OpenCode Console denied billing access; select an organization where the account can read billing",
        ),
        HttpError::Status { status, .. } => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            format!("OpenCode Console billing HTTP {status}"),
        ),
        HttpError::Other(_) => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            "OpenCode Console billing request failed or returned invalid JSON",
        ),
    }
}

fn microcents(value: &Value) -> Option<f64> {
    let amount = match value {
        Value::String(value) => value.parse::<i128>().ok()? as f64,
        Value::Number(value) => value
            .as_i64()
            .map(|v| v as f64)
            .or_else(|| value.as_u64().map(|v| v as f64))?,
        _ => return None,
    };
    Some(amount / 100_000_000.0)
}

fn snapshot_from_billing(body: &Value) -> ProviderSnapshot {
    let invalid = || {
        ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            "OpenCode Console returned incomplete or invalid billing amounts",
        )
    };
    let Some(balance) = body.get("balanceMicroCents").and_then(microcents) else {
        return invalid();
    };
    let mut snapshot = ProviderSnapshot::ok(ID, LABEL);
    snapshot.meters.push(create_meter(
        "balance",
        "Credit balance",
        None,
        None,
        None,
        Some(balance),
        None,
        "USD",
        None,
        None,
        None,
    ));
    if body
        .get("creditLimitMicroCents")
        .is_some_and(|v| !v.is_null())
    {
        let Some(_) = body
            .get("creditLimitMicroCents")
            .and_then(microcents)
            .filter(|v| *v >= 0.0)
        else {
            return invalid();
        };
        let Some(available) = body.get("availableMicroCents").and_then(microcents) else {
            return invalid();
        };
        snapshot.meters.push(create_meter(
            "available_credit",
            "Available credit",
            None,
            None,
            None,
            Some(available),
            None,
            "USD",
            None,
            None,
            None,
        ));
    }
    snapshot.plan = body.get("mode").and_then(Value::as_str).map(str::to_owned);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn billing_keeps_signed_microcent_balances_without_inventing_quota() {
        for (raw, dollars) in [
            (json!("1250000000"), 12.5),
            (json!(0), 0.0),
            (json!("-12500000"), -0.125),
        ] {
            let snapshot = snapshot_from_billing(&json!({
                "mode": "pay-as-you-go", "balanceMicroCents": raw,
                "creditLimitMicroCents": null, "availableMicroCents": raw,
            }));
            assert_eq!(snapshot.status, SnapshotStatus::Ok);
            let balance = &snapshot.meters[0];
            assert_eq!(balance.left, Some(dollars));
            assert_eq!(balance.percent, None);
            assert_eq!(balance.limit, None);
            assert_eq!(balance.reset_at, None);
        }
        let invoice = snapshot_from_billing(&json!({
            "mode":"invoiceable", "balanceMicroCents":"-100000000",
            "creditLimitMicroCents":"10000000000", "availableMicroCents":"9900000000",
        }));
        assert_eq!(invoice.meters[0].left, Some(-1.0));
        assert_eq!(invoice.meters[1].left, Some(99.0));
        assert_eq!(invoice.meters[1].percent, None);
        assert_eq!(invoice.meters[1].limit, None);
    }

    #[test]
    fn billing_rejects_missing_and_malformed_amounts_instead_of_showing_zero() {
        for balance in [
            Value::Null,
            json!("NaN"),
            json!("1e8"),
            json!(1.5),
            json!({}),
        ] {
            assert_eq!(
                snapshot_from_billing(&json!({"balanceMicroCents":balance})).status,
                SnapshotStatus::Error
            );
        }
        assert_eq!(
            snapshot_from_billing(&json!({})).status,
            SnapshotStatus::Error
        );
        assert_eq!(
            snapshot_from_billing(&json!({
                "balanceMicroCents":"0", "creditLimitMicroCents":"100",
            }))
            .status,
            SnapshotStatus::Error
        );
    }

    #[test]
    fn org_selection_never_guesses_across_multiple_workspaces() {
        assert_eq!(
            select_org(&json!([{"id":"wrk_legacy"}])).unwrap(),
            "wrk_legacy"
        );
        assert!(select_org(&json!([{"id":"org_one"},{"id":"org_two"}])).is_err());
        assert!(select_org(&json!([])).is_err());
        assert!(select_org(&json!([{"id":"org_bad\r\nCookie: secret"}])).is_err());
        assert_eq!(select_org(&json!({})).unwrap_err().0, SnapshotStatus::Error);
    }

    #[test]
    fn only_console_oauth_with_live_expiry_is_usable() {
        let credential = json!({"type":"oauth","access":"account-access", "expires":4102444800000_i64,
            "metadata":{"server":CONSOLE,"orgID":"org_test"}});
        assert_eq!(
            oauth_session(&credential, "test")
                .unwrap()
                .org_id
                .as_deref(),
            Some("org_test")
        );
        let mut expired = credential.clone();
        expired["expires"] = json!(1);
        assert!(
            oauth_session(&expired, "test")
                .err()
                .unwrap()
                .contains("expired")
        );
        let mut stale_access = credential.clone();
        stale_access["access"] = json!("e30.eyJleHAiOjF9.signature");
        assert!(
            oauth_session(&stale_access, "test")
                .err()
                .unwrap()
                .contains("expired")
        );
        let mut foreign = credential.clone();
        foreign["metadata"]["server"] = json!("https://example.com/console");
        assert!(oauth_session(&foreign, "test").is_err());
        assert!(oauth_session(&json!({"type":"key","key":"model-key"}), "test").is_err());
    }

    #[test]
    fn console_auth_and_permission_failures_never_echo_server_secrets() {
        for (code, expected) in [
            (401, SnapshotStatus::Auth),
            (403, SnapshotStatus::Auth),
            (503, SnapshotStatus::Error),
        ] {
            let snapshot = fetch_error(HttpError::Status {
                status: code,
                message: "private-session".to_owned(),
                body: Some(json!({"error":"private-session"})),
            });
            assert_eq!(snapshot.status, expected);
            assert!(!snapshot.error.unwrap().contains("private-session"));
        }
    }
}
