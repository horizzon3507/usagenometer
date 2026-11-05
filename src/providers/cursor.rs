//! Cursor usage summary.

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{HttpClient, HttpError};
use crate::jwt::{jwt_exp, jwt_sub, normalize_bearer};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, create_meter, meter_from_used_percent,
};

const USAGE_SUMMARY_URL: &str = "https://cursor.com/api/usage-summary";
const EXPIRY_SKEW: f64 = 30.0;
const ID: &str = "cursor";
const LABEL: &str = "Cursor";

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

    let summary: Value = client
        .get_json(
            USAGE_SUMMARY_URL,
            &[
                ("Accept", "application/json"),
                ("Cookie", &auth.session_cookie),
                ("User-Agent", "Usagenometer/1.0"),
            ],
        )
        .map_err(|e: HttpError| {
            let status = if e.is_auth_error() {
                SnapshotStatus::Auth
            } else {
                SnapshotStatus::Error
            };
            let message = if e.is_auth_error() {
                "Cursor session was rejected. Open Cursor and sign in again.".into()
            } else {
                e.to_string()
            };
            FetchErr { status, message }
        })?;

    Ok(snapshot_from_usage_summary(&summary, &auth))
}

pub struct Auth {
    pub email: Option<String>,
    pub membership_type: Option<String>,
    pub session_cookie: String,
    source: String,
    path: PathBuf,
    expires_at: f64,
    refresh_available: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliCredentials {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

struct AuthCandidate {
    access_token: String,
    refresh_available: bool,
    email: Option<String>,
    membership_type: Option<String>,
    source: &'static str,
    path: PathBuf,
}

fn load_auth() -> Result<Auth, String> {
    resolve_auth(&cli_auth_path(), &state_db_path())
}

fn resolve_auth(cli_path: &Path, app_path: &Path) -> Result<Auth, String> {
    let mut failures = Vec::new();

    if let Some(candidate) = match load_cli_candidate(cli_path) {
        Ok(candidate) => candidate,
        Err(error) => {
            failures.push(error);
            None
        }
    } {
        match auth_from_candidate(candidate) {
            Ok(auth) => return Ok(auth),
            Err(error) => failures.push(error),
        }
    }
    if let Some(candidate) = match load_app_candidate(app_path) {
        Ok(candidate) => candidate,
        Err(error) => {
            failures.push(error);
            None
        }
    } {
        match auth_from_candidate(candidate) {
            Ok(auth) => return Ok(auth),
            Err(error) => failures.push(error),
        }
    }

    if failures.is_empty() {
        Err(format!(
            "Cursor auth not found at {} or {}. Sign in to Cursor.",
            cli_path.display(),
            app_path.display()
        ))
    } else {
        Err(failures.join(" "))
    }
}

fn cli_auth_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("cursor")
        .join("auth.json")
}

fn load_cli_candidate(path: &Path) -> Result<Option<AuthCandidate>, String> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(format!(
                "Failed to read Cursor CLI auth at {}.",
                path.display()
            ));
        }
    };
    let credentials: CliCredentials = serde_json::from_slice(&contents)
        .map_err(|_| format!("Cursor CLI auth at {} is invalid.", path.display()))?;
    let access_token = normalize_bearer(&credentials.access_token);
    if access_token.is_empty() {
        return Err(format!(
            "Cursor CLI auth at {} has no access token.",
            path.display()
        ));
    }
    Ok(Some(AuthCandidate {
        access_token,
        refresh_available: credentials
            .refresh_token
            .is_some_and(|token| !token.trim().is_empty()),
        email: None,
        membership_type: None,
        source: "Cursor CLI",
        path: path.to_path_buf(),
    }))
}

fn load_app_candidate(path: &Path) -> Result<Option<AuthCandidate>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let values = read_state_keys(
        path,
        &[
            "cursorAuth/accessToken",
            "cursorAuth/cachedEmail",
            "cursorAuth/stripeMembershipType",
        ],
    )
    .map_err(|_| {
        format!(
            "Failed to read Cursor app state database at {}.",
            path.display()
        )
    })?;
    let Some(access_token) = values
        .get("cursorAuth/accessToken")
        .and_then(|value| value.as_ref())
        .map(|value| normalize_bearer(value))
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(AuthCandidate {
        access_token,
        refresh_available: false,
        email: values
            .get("cursorAuth/cachedEmail")
            .and_then(|value| value.clone())
            .filter(|value| !value.trim().is_empty()),
        membership_type: values
            .get("cursorAuth/stripeMembershipType")
            .and_then(|value| value.clone())
            .filter(|value| !value.trim().is_empty()),
        source: "Cursor app",
        path: path.to_path_buf(),
    }))
}

fn auth_from_candidate(candidate: AuthCandidate) -> Result<Auth, String> {
    let user_id = user_id_from_token(&candidate.access_token).ok_or_else(|| {
        format!(
            "{} auth at {} has an invalid access token subject.",
            candidate.source,
            candidate.path.display()
        )
    })?;
    let expires_at = jwt_exp(&candidate.access_token).ok_or_else(|| {
        format!(
            "{} auth at {} has an access token without an expiry.",
            candidate.source,
            candidate.path.display()
        )
    })?;
    if expires_at <= now_secs() + EXPIRY_SKEW {
        return Err(format!(
            "{} access token at {} is expired.",
            candidate.source,
            candidate.path.display()
        ));
    }

    Ok(Auth {
        email: candidate.email,
        membership_type: candidate.membership_type,
        session_cookie: format!(
            "WorkosCursorSessionToken={user_id}%3A%3A{}",
            candidate.access_token
        ),
        source: candidate.source.into(),
        path: candidate.path,
        expires_at,
        refresh_available: candidate.refresh_available,
    })
}

pub(crate) fn auth_description() -> Result<String, String> {
    let auth = load_auth()?;
    Ok(format!(
        "{} ({}) expires at unix {}; refresh token available: {}",
        auth.source,
        auth.path.display(),
        auth.expires_at as u64,
        if auth.refresh_available { "yes" } else { "no" }
    ))
}

fn state_db_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

fn read_state_keys(
    path: &Path,
    keys: &[&str],
) -> Result<HashMap<String, Option<String>>, rusqlite::Error> {
    let uri = format!("file:{}?mode=ro", path.display());
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut out = HashMap::new();
    for key in keys {
        let value: Option<Vec<u8>> = match conn.query_row(
            "SELECT CAST(value AS BLOB) FROM ItemTable WHERE CAST(key AS TEXT) = ?1",
            [*key],
            |row| row.get(0),
        ) {
            Ok(bytes) => Some(bytes),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => return Err(error),
        };
        out.insert(
            (*key).to_string(),
            value.and_then(|bytes| String::from_utf8(bytes).ok()),
        );
    }
    Ok(out)
}

fn user_id_from_token(token: &str) -> Option<String> {
    let sub = jwt_sub(token)?;
    let id = sub.strip_prefix("auth0|").unwrap_or(&sub).trim();
    if id.is_empty() || id.contains('|') {
        None
    } else {
        Some(id.to_string())
    }
}

pub fn snapshot_from_usage_summary(summary: &Value, auth: &Auth) -> ProviderSnapshot {
    let individual = summary.get("individualUsage").unwrap_or(&Value::Null);
    let plan = individual.get("plan").unwrap_or(&Value::Null);
    let on_demand = individual.get("onDemand").unwrap_or(&Value::Null);
    let cycle_end = summary.get("billingCycleEnd").and_then(coerce_unix_seconds);

    let mut meters = Vec::new();

    if let Some(auto) = plan.get("autoPercentUsed").and_then(|v| v.as_f64()) {
        meters.push(meter_from_used_percent(
            "auto_composer",
            "Auto + Composer",
            auto,
            cycle_end,
        ));
    }
    if let Some(api) = plan.get("apiPercentUsed").and_then(|v| v.as_f64()) {
        meters.push(meter_from_used_percent("api", "API pool", api, cycle_end));
    }

    if meters.is_empty() {
        if let Some(p) = extract_percent(
            summary
                .get("autoModelSelectedDisplayMessage")
                .and_then(|v| v.as_str()),
        ) {
            meters.push(meter_from_used_percent(
                "auto_composer",
                "Auto + Composer",
                p,
                cycle_end,
            ));
        }
        if let Some(p) = extract_percent(
            summary
                .get("namedModelSelectedDisplayMessage")
                .and_then(|v| v.as_str()),
        ) {
            meters.push(meter_from_used_percent("api", "API pool", p, cycle_end));
        }
    }

    if on_demand
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && let Some(used) = on_demand.get("used").and_then(|v| v.as_f64())
    {
        let limit = on_demand.get("limit").and_then(|v| v.as_f64());
        meters.push(create_meter(
            "on_demand",
            "On-demand",
            limit.filter(|l| *l > 0.0).map(|l| used / l),
            limit
                .filter(|l| *l > 0.0)
                .map(|l| (1.0 - used / l).max(0.0)),
            Some(used),
            limit.map(|l| (l - used).max(0.0)),
            limit,
            "usd",
            cycle_end,
            None,
            None,
        ));
    }

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: None,
        account: auth.email.clone(),
        plan: summary
            .get("membershipType")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| auth.membership_type.clone()),
        meters,
        stale_age_secs: None,
    }
}

fn extract_percent(message: Option<&str>) -> Option<f64> {
    let message = message?;
    let bytes = message.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let num: f64 = message[start..i].parse().ok()?;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'%' {
                return Some(num);
            }
        } else {
            i += 1;
        }
    }
    None
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
    fn dual_pool_snapshot() {
        let summary = serde_json::json!({
            "membershipType": "pro",
            "billingCycleEnd": "2026-08-05T01:12:18.000Z",
            "individualUsage": {
                "plan": {
                    "autoPercentUsed": 73.67,
                    "apiPercentUsed": 36.644444444444446,
                }
            }
        });
        let auth = Auth {
            email: Some("user@example.com".into()),
            membership_type: Some("pro".into()),
            session_cookie: String::new(),
            source: "Cursor CLI".into(),
            path: PathBuf::new(),
            expires_at: now_secs() + 3600.0,
            refresh_available: true,
        };
        let snap = snapshot_from_usage_summary(&summary, &auth);
        assert_eq!(snap.meters.len(), 2);
        assert_eq!(snap.meters[0].id, "auto_composer");
        assert!((snap.meters[0].percent.unwrap() - 0.7367).abs() < 0.001);
        assert!((snap.meters[1].percent.unwrap() - 0.3664).abs() < 0.001);
    }

    fn test_token(sub: &str, exp: i64) -> String {
        use base64::Engine;
        let claims = serde_json::json!({"sub": sub, "exp": exp});
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("header.{encoded}.signature")
    }

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "usagenometer-cursor-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn write_app_db(path: &Path, token: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute("CREATE TABLE ItemTable (key BLOB, value BLOB)", [])
            .unwrap();
        for (key, value) in [
            ("cursorAuth/accessToken", token),
            ("cursorAuth/cachedEmail", "app-cache@example.test"),
            ("cursorAuth/stripeMembershipType", "pro"),
        ] {
            connection
                .execute(
                    "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                    rusqlite::params![key.as_bytes(), value.as_bytes()],
                )
                .unwrap();
        }
    }

    #[test]
    fn resolves_cli_auth_and_builds_workos_cookie_from_auth0_subject() {
        let cli_path = temp_path("cli");
        let app_path = temp_path("app-db");
        let token = test_token("auth0|cursor-user", (now_secs() + 3600.0) as i64);
        let app_token = test_token("auth0|app-user", (now_secs() + 3600.0) as i64);
        fs::write(
            &cli_path,
            serde_json::json!({"accessToken": token, "refreshToken": "present"}).to_string(),
        )
        .unwrap();
        write_app_db(&app_path, &app_token);

        let auth = resolve_auth(&cli_path, &app_path).unwrap();
        assert!(
            auth.session_cookie
                .starts_with("WorkosCursorSessionToken=cursor-user%3A%3A")
        );
        assert_eq!(auth.source, "Cursor CLI");
        assert!(auth.refresh_available);
        fs::remove_file(cli_path).unwrap();
        fs::remove_file(app_path).unwrap();
    }

    #[test]
    fn expired_cli_auth_falls_back_to_blob_backed_app_state() {
        let cli_path = temp_path("expired-cli");
        let app_path = temp_path("app-db");
        let expired = test_token("auth0|old-user", 1);
        let app_token = test_token("auth0|app-user", (now_secs() + 3600.0) as i64);
        fs::write(
            &cli_path,
            serde_json::json!({"accessToken": expired, "refreshToken": "present"}).to_string(),
        )
        .unwrap();
        write_app_db(&app_path, &app_token);

        let auth = resolve_auth(&cli_path, &app_path).unwrap();
        assert_eq!(auth.source, "Cursor app");
        assert_eq!(auth.email.as_deref(), Some("app-cache@example.test"));
        assert_eq!(auth.membership_type.as_deref(), Some("pro"));
        assert!(
            auth.session_cookie
                .starts_with("WorkosCursorSessionToken=app-user%3A%3A")
        );
        assert!(!auth.refresh_available);
        fs::remove_file(cli_path).unwrap();
        fs::remove_file(app_path).unwrap();
    }

    #[test]
    fn invalid_cli_json_falls_back_to_app_and_reports_both_expired_sources() {
        let cli_path = temp_path("invalid-cli");
        let app_path = temp_path("expired-app");
        fs::write(&cli_path, b"{").unwrap();
        write_app_db(&app_path, &test_token("auth0|old-user", 1));
        let error = resolve_auth(&cli_path, &app_path)
            .err()
            .expect("no valid Cursor source should resolve");
        assert!(error.contains("Cursor CLI auth"));
        assert!(error.contains("Cursor app access token"));
        fs::remove_file(cli_path).unwrap();
        fs::remove_file(app_path).unwrap();
    }
}
