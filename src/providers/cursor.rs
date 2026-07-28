//! Cursor usage summary.

use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{HttpClient, HttpError};
use crate::jwt::{jwt_exp, jwt_sub, normalize_bearer};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, create_meter, meter_from_used_percent,
};

const USAGE_SUMMARY_URL: &str = "https://www.cursor.com/api/usage-summary";
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
}

fn load_auth() -> Result<Auth, String> {
    let path = state_db_path();
    if !path.exists() {
        return Err(format!(
            "Cursor state database not found at {}. Sign in to the Cursor app.",
            path.display()
        ));
    }

    let values = read_state_keys(
        &path,
        &[
            "cursorAuth/accessToken",
            "cursorAuth/cachedEmail",
            "cursorAuth/stripeMembershipType",
        ],
    )
    .map_err(|_| format!("Failed to read Cursor state database at {}.", path.display()))?;

    let access = values
        .get("cursorAuth/accessToken")
        .and_then(|v| v.as_ref())
        .map(|s| normalize_bearer(s))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "Cursor auth not found at {}. Sign in to the Cursor app.",
                path.display()
            )
        })?;

    let user_id = user_id_from_token(&access)
        .ok_or_else(|| "Cursor access token is missing a user id subject.".to_string())?;

    if let Some(exp) = jwt_exp(&access) {
        let now = now_secs();
        if exp <= now + EXPIRY_SKEW {
            return Err("Cursor session token is expired. Open Cursor and sign in again.".into());
        }
    }

    let email = values
        .get("cursorAuth/cachedEmail")
        .and_then(|v| v.clone())
        .filter(|s| !s.trim().is_empty());
    let membership_type = values
        .get("cursorAuth/stripeMembershipType")
        .and_then(|v| v.clone())
        .filter(|s| !s.trim().is_empty());

    Ok(Auth {
        email,
        membership_type,
        session_cookie: format!("WorkosCursorSessionToken={user_id}%3A%3A{access}"),
    })
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
    path: &PathBuf,
    keys: &[&str],
) -> Result<HashMap<String, Option<String>>, rusqlite::Error> {
    let uri = format!("file:{}?mode=ro", path.display());
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut out = HashMap::new();
    for key in keys {
        let value: Option<String> = match conn.query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            [*key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(s) => Some(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(rusqlite::Error::InvalidColumnType(_, _, _)) => {
                // BLOB fallback
                conn.query_row(
                    "SELECT value FROM ItemTable WHERE key = ?1",
                    [*key],
                    |row| {
                        let bytes: Vec<u8> = row.get(0)?;
                        Ok(String::from_utf8_lossy(&bytes).into_owned())
                    },
                )
                .optional()?
            }
            Err(e) => return Err(e),
        };
        out.insert((*key).to_string(), value);
    }
    Ok(out)
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn user_id_from_token(token: &str) -> Option<String> {
    let sub = jwt_sub(token)?;
    let parts: Vec<&str> = sub.split('|').collect();
    let id = if parts.len() > 1 { parts[1] } else { parts[0] }.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

pub fn snapshot_from_usage_summary(summary: &Value, auth: &Auth) -> ProviderSnapshot {
    let individual = summary.get("individualUsage").unwrap_or(&Value::Null);
    let plan = individual.get("plan").unwrap_or(&Value::Null);
    let on_demand = individual.get("onDemand").unwrap_or(&Value::Null);
    let cycle_end = summary
        .get("billingCycleEnd")
        .and_then(coerce_unix_seconds);

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
        };
        let snap = snapshot_from_usage_summary(&summary, &auth);
        assert_eq!(snap.meters.len(), 2);
        assert_eq!(snap.meters[0].id, "auto_composer");
        assert!((snap.meters[0].percent.unwrap() - 0.7367).abs() < 0.001);
        assert!((snap.meters[1].percent.unwrap() - 0.3664).abs() < 0.001);
    }
}
