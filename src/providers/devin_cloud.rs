//! Devin Cloud — consumption and quota meters.
//!
//! Two credential paths:
//!
//! - **API key** (`DEVIN_API_KEY`, or a `cog_`/`apk_` value inside the Devin
//!   CLI credential file): the REST API at api.devin.ai. ACU consumption from
//!   the org consumption API (`GET /v3/organizations/{org}/consumption/daily`,
//!   org ids + cycle caps from `GET /v2/enterprise/organizations`), with
//!   `GET /v2/enterprise/sessions` (`acus_consumed` per session, bucketed by
//!   creation time) as fallback.
//! - **Devin CLI session** (`devin auth login` — the `devin-…` token inside
//!   `credentials.toml` targets the Connect API at `api_server_url`, normally
//!   server.codeium.com): daily/weekly quota percents, prompt credits and
//!   overage from `SeatManagementService/GetUserStatus`.
//!
//! The credential file lives at `$XDG_DATA_HOME/devin/credentials.toml` /
//! `~/.local/share/devin/` (`%APPDATA%\devin` on Windows).
//!
//! Devin bills in ACUs (Agent Compute Units), not tokens — the ledger stays
//! empty for this provider by design.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, UsageMeter, coerce_number, coerce_unix_seconds, create_meter,
    meter_from_remaining_fraction,
};

const API_BASE: &str = "https://api.devin.ai";
const ID: &str = "devin-cloud";
const LABEL: &str = "Devin Cloud";
const PAGE_LIMIT: i64 = 100;
const MAX_PAGES: usize = 10;
/// Days of daily-consumption history fetched to approximate the billing cycle.
const CYCLE_WINDOW_DAYS: i64 = 30;
const DAY_SECS: f64 = 86_400.0;

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
    let cred = load_credential().map_err(|e| FetchErr {
        status: SnapshotStatus::Auth,
        message: e,
    })?;

    let api_key = match cred {
        Credential::ApiKey(k) => k,
        Credential::CliSession { token, server } => {
            return cli_status_snapshot(client, &token, &server);
        }
    };

    let bearer = format!("Bearer {api_key}");
    let headers = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
        ("User-Agent", "usagenometer/0.1"),
    ];
    let now = now_secs();

    // Preferred path: org list + per-org daily consumption (accurate by
    // billing day, plus the org's cycle cap when one is configured).
    let orgs = match env_org_ids() {
        Some(ids) => Some(
            ids.into_iter()
                .map(|org_id| Org {
                    org_id,
                    name: None,
                    cycle_cap: None,
                })
                .collect(),
        ),
        None => match fetch_orgs(client, &headers) {
            Ok(orgs) => Some(orgs),
            Err(e) => {
                if e.is_auth_error() {
                    return Err(map_http(e));
                }
                None
            }
        },
    };

    if let Some(orgs) = orgs
        && !orgs.is_empty()
    {
        match consumption_snapshot(client, &headers, &orgs, now) {
            Ok(snap) => return Ok(snap),
            Err(e) => {
                if e.is_auth_error() {
                    return Err(map_http(e));
                }
                // fall through to the sessions path
            }
        }
    }

    sessions_snapshot(client, &headers, now)
}

fn map_http(e: HttpError) -> FetchErr {
    let status = if e.is_auth_error() {
        SnapshotStatus::Auth
    } else {
        SnapshotStatus::Error
    };
    let message = if e.is_auth_error() {
        "Devin API key rejected — check DEVIN_API_KEY or run devin auth login.".into()
    } else {
        e.to_string()
    };
    FetchErr { status, message }
}

// ---------- auth ----------

enum Credential {
    /// `cog_`/`apk_` (or `DEVIN_API_KEY`) — REST API at api.devin.ai.
    ApiKey(String),
    /// `devin auth login` session token — Connect API at `server`.
    CliSession { token: String, server: String },
}

fn load_credential() -> Result<Credential, String> {
    if let Ok(key) = std::env::var("DEVIN_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Ok(Credential::ApiKey(key.to_string()));
        }
    }
    let path = credentials_path();
    let raw = fs::read_to_string(&path).map_err(|_| {
        "Devin credentials not found — set DEVIN_API_KEY or run devin auth login.".to_string()
    })?;
    let parsed: toml::Value = toml::from_str(&raw).map_err(|_| {
        format!(
            "Devin credentials at {} are not valid TOML.",
            path.display()
        )
    })?;
    file_credential(&parsed).ok_or_else(|| {
        format!(
            "Devin credentials at {} carry no API token.",
            path.display()
        )
    })
}

/// `credentials.toml` shape is not publicly documented — collect every
/// non-empty string value whose key mentions a token/api key, at any depth.
/// Real API keys (`cog_`/`apk_`) win over the `devin-…` CLI session token;
/// anything else falls back to the API path (env-var-like behavior).
fn file_credential(parsed: &toml::Value) -> Option<Credential> {
    let mut secrets = Vec::new();
    collect_secrets(parsed, &mut secrets);

    for s in &secrets {
        if s.starts_with("cog_") || s.starts_with("apk_") {
            return Some(Credential::ApiKey(s.clone()));
        }
    }
    for s in &secrets {
        if s.starts_with("devin-") {
            return Some(Credential::CliSession {
                token: s.clone(),
                server: find_string(parsed, "api_server_url")
                    .unwrap_or_else(|| "https://server.codeium.com".into()),
            });
        }
    }
    secrets.into_iter().next().map(Credential::ApiKey)
}

fn collect_secrets(value: &toml::Value, out: &mut Vec<String>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, item) in table {
        let key = key.to_ascii_lowercase();
        if (key.contains("token") || key.contains("api_key"))
            && let Some(s) = item.as_str()
            && !s.trim().is_empty()
        {
            out.push(s.trim().to_string());
        }
    }
    for item in table.values() {
        collect_secrets(item, out);
    }
}

/// First non-empty string value at any depth whose key ends with `suffix`.
fn find_string(value: &toml::Value, suffix: &str) -> Option<String> {
    let table = value.as_table()?;
    for (key, item) in table {
        if key.to_ascii_lowercase().ends_with(suffix)
            && let Some(s) = item.as_str()
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    table.values().find_map(|v| find_string(v, suffix))
}

pub(crate) fn credentials_path() -> PathBuf {
    #[cfg(windows)]
    if let Ok(dir) = std::env::var("APPDATA") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir).join("devin").join("credentials.toml");
        }
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir).join("devin").join("credentials.toml");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("devin")
        .join("credentials.toml")
}

/// `DEVIN_ORG_ID` (comma-separated) skips org discovery — the org-scoped
/// consumption endpoint works on non-Enterprise plans where the enterprise
/// organizations list does not.
fn env_org_ids() -> Option<Vec<String>> {
    let raw = std::env::var("DEVIN_ORG_ID").ok()?;
    let ids: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

// ---------- org path ----------

struct Org {
    org_id: String,
    name: Option<String>,
    /// Org-level monthly ACU cap (`max_cycle_acu_limit`), when configured.
    cycle_cap: Option<f64>,
}

fn fetch_orgs(client: &HttpClient, headers: &[(&str, &str)]) -> Result<Vec<Org>, HttpError> {
    let mut out = Vec::new();
    let mut skip: i64 = 0;
    for _ in 0..MAX_PAGES {
        let url = format!("{API_BASE}/v2/enterprise/organizations?limit={PAGE_LIMIT}&skip={skip}");
        let page: Value = client.get_json(&url, headers)?;
        let items = page
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for item in &items {
            let Some(org_id) = item.get("org_id").and_then(|v| v.as_str()) else {
                continue;
            };
            out.push(Org {
                org_id: org_id.to_string(),
                name: item
                    .get("org_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                cycle_cap: item.get("max_cycle_acu_limit").and_then(coerce_number),
            });
        }
        if !page
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            break;
        }
        skip = page
            .get("next_cursor")
            .and_then(|v| v.as_i64())
            .unwrap_or(skip + items.len() as i64);
    }
    Ok(out)
}

fn consumption_snapshot(
    client: &HttpClient,
    headers: &[(&str, &str)],
    orgs: &[Org],
    now: f64,
) -> Result<ProviderSnapshot, HttpError> {
    let time_after = (now - (CYCLE_WINDOW_DAYS + 1) as f64 * DAY_SECS) as i64;
    let time_before = now as i64;

    let mut days: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    let mut names = Vec::new();
    let mut cycle_cap = 0.0;
    let mut first_err: Option<HttpError> = None;
    for org in orgs {
        let url = format!(
            "{API_BASE}/v3/organizations/{}/consumption/daily?time_after={time_after}&time_before={time_before}",
            org.org_id
        );
        match client.get_json::<Value>(&url, headers) {
            Ok(v) => {
                for (day, acus) in consumption_days(&v) {
                    *days.entry(day).or_insert(0.0) += acus;
                }
            }
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
        if let Some(name) = &org.name {
            names.push(name.clone());
        }
        cycle_cap += org.cycle_cap.unwrap_or(0.0);
    }

    if days.is_empty()
        && let Some(e) = first_err
    {
        return Err(e);
    }

    Ok(ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if days.is_empty() {
            Some("No ACU consumption recorded in the last 30 days.".into())
        } else if first_err.is_some() {
            Some("Partial data — consumption failed for at least one org.".into())
        } else {
            None
        },
        account: if names.is_empty() {
            None
        } else {
            Some(names.join(", "))
        },
        plan: None,
        meters: meters_from_days(&days, now, (cycle_cap > 0.0).then_some(cycle_cap)),
        stale_age_secs: None,
    })
}

/// `consumption_by_date[]` → (bucket-start unix secs, acus). Buckets start at
/// midnight PST per the API reference.
fn consumption_days(v: &Value) -> Vec<(i64, f64)> {
    v.get("consumption_by_date")
        .and_then(|a| a.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|d| {
                    let date = d.get("date").and_then(coerce_unix_seconds)? as i64;
                    let acus = d.get("acus").and_then(coerce_number).unwrap_or(0.0);
                    Some((date, acus))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn meters_from_days(
    days: &std::collections::BTreeMap<i64, f64>,
    now: f64,
    cycle_cap: Option<f64>,
) -> Vec<UsageMeter> {
    // The newest bucket at or before `now` is the current billing day.
    let Some(&today_start) = days.keys().rfind(|d| (**d) as f64 <= now) else {
        return Vec::new();
    };
    let mut meters = Vec::new();
    let today = days.get(&today_start).copied().unwrap_or(0.0);
    meters.push(acu_meter("acu_today", "ACU today", today, DAY_SECS));

    let week_start = today_start - 6 * DAY_SECS as i64;
    let week: f64 = days
        .iter()
        .filter(|(d, _)| **d >= week_start)
        .map(|(_, a)| *a)
        .sum();
    meters.push(acu_meter("acu_7d", "ACU 7d", week, 7.0 * DAY_SECS));

    if let Some(cap) = cycle_cap {
        // Cycles follow the contract's monthly billing window; ~30d of daily
        // buckets approximates it when the exact start is not exposed.
        let used: f64 = days.values().sum();
        meters.push(create_meter(
            "acu_cycle",
            "Cycle ACU (~30d)",
            None,
            None,
            Some(used),
            None,
            Some(cap),
            "acu",
            None,
            None,
            Some(CYCLE_WINDOW_DAYS as f64 * DAY_SECS),
        ));
    }
    meters
}

fn acu_meter(id: &str, title: &str, used: f64, window: f64) -> UsageMeter {
    create_meter(
        id,
        title,
        None,
        None,
        Some(used),
        None,
        None,
        "acu",
        None,
        None,
        Some(window),
    )
}

// ---------- sessions fallback ----------

/// ACU consumed by sessions created in the window (`created_at`, not by
/// billing day — a session's lifetime `acus_consumed` lands on creation).
fn sessions_snapshot(
    client: &HttpClient,
    headers: &[(&str, &str)],
    now: f64,
) -> Result<ProviderSnapshot, FetchErr> {
    let from = time::OffsetDateTime::from_unix_timestamp((now - 7.0 * DAY_SECS) as i64)
        .ok()
        .and_then(|dt| {
            dt.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default();

    let mut skip: i64 = 0;
    let mut acu_24h = 0.0;
    let mut acu_7d = 0.0;
    let mut seen = 0usize;
    for _ in 0..MAX_PAGES {
        let url = format!(
            "{API_BASE}/v2/enterprise/sessions?created_date_from={from}&limit={PAGE_LIMIT}&skip={skip}"
        );
        let page: Value = client.get_json(&url, headers).map_err(map_http)?;
        let items = page
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for item in &items {
            let acus = item
                .get("acus_consumed")
                .and_then(coerce_number)
                .unwrap_or(0.0);
            let created = item
                .get("created_at")
                .and_then(coerce_unix_seconds)
                .unwrap_or(0.0);
            acu_7d += acus;
            if created >= now - DAY_SECS {
                acu_24h += acus;
            }
        }
        seen += items.len();
        if !page
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            break;
        }
        skip = page
            .get("next_cursor")
            .and_then(|v| v.as_i64())
            .unwrap_or(skip + items.len() as i64);
    }

    let meters = vec![
        acu_meter("acu_today", "ACU 24h", acu_24h, DAY_SECS),
        acu_meter("acu_7d", "ACU 7d", acu_7d, 7.0 * DAY_SECS),
    ];
    Ok(ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if seen == 0 {
            Some("No sessions created in the last 7 days.".into())
        } else {
            None
        },
        account: None,
        plan: None,
        meters,
        stale_age_secs: None,
    })
}

// ---------- CLI session path (Connect / codeium) ----------

/// Quota meters for `devin auth login` users: the session token can't reach
/// api.devin.ai, but `SeatManagementService/GetUserStatus` on the Connect
/// server returns plan info plus daily/weekly quota percents.
fn cli_status_snapshot(
    client: &HttpClient,
    token: &str,
    server: &str,
) -> Result<ProviderSnapshot, FetchErr> {
    let url = format!(
        "{}/exa.seat_management_pb.SeatManagementService/GetUserStatus",
        server.trim_end_matches('/')
    );
    let bearer = format!("Bearer {token}");
    let headers = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
        ("Connect-Protocol-Version", "1"),
    ];
    let body = serde_json::json!({
        "metadata": {
            "apiKey": token,
            "ideName": "devin-cli",
            "ideVersion": "3000.11.3",
            "extensionName": "devin",
            "extensionVersion": "3000.11.3",
            "locale": "en-US",
        }
    });
    let payload: Value = client.post_json(&url, &body, &headers).map_err(|e| {
        if e.is_auth_error() {
            map_http(e)
        } else {
            FetchErr {
                status: SnapshotStatus::Error,
                message: e.to_string(),
            }
        }
    })?;
    Ok(cli_snapshot_from_status(&payload))
}

fn cli_snapshot_from_status(payload: &Value) -> ProviderSnapshot {
    let status = payload.get("userStatus").unwrap_or(payload);
    let plan_status = status.get("planStatus").cloned().unwrap_or(Value::Null);

    let mut meters = Vec::new();

    // `hideDailyQuota` marks plans without a daily window (e.g. Max).
    let hide_daily = plan_status
        .pointer("/planInfo/hideDailyQuota")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !hide_daily
        && let Some(left) = plan_status
            .get("dailyQuotaRemainingPercent")
            .and_then(coerce_number)
    {
        meters.push(meter_from_remaining_fraction(
            "daily_quota",
            "Daily quota",
            left / 100.0,
            plan_status
                .get("dailyQuotaResetAtUnix")
                .and_then(coerce_unix_seconds),
            Some(DAY_SECS),
        ));
    }
    if let Some(left) = plan_status
        .get("weeklyQuotaRemainingPercent")
        .and_then(coerce_number)
    {
        meters.push(meter_from_remaining_fraction(
            "weekly_quota",
            "Weekly quota",
            left / 100.0,
            plan_status
                .get("weeklyQuotaResetAtUnix")
                .and_then(coerce_unix_seconds),
            Some(7.0 * DAY_SECS),
        ));
    }
    // -1 marks an unlimited credit pool; negative = exhausted.
    if let Some(credits) = plan_status
        .get("availablePromptCredits")
        .and_then(coerce_number)
        && credits > 0.0
    {
        meters.push(create_meter(
            "prompt_credits",
            "Prompt credits",
            None,
            None,
            None,
            Some(credits),
            None,
            "credits",
            None,
            None,
            Some(CYCLE_WINDOW_DAYS as f64 * DAY_SECS),
        ));
    }
    // Micros of overage balance; negative = spent beyond the plan quota.
    if let Some(micros) = plan_status
        .get("overageBalanceMicros")
        .and_then(coerce_number)
        && micros < 0.0
    {
        meters.push(create_meter(
            "overage",
            "Overage spend",
            None,
            None,
            Some(micros.abs() / 1_000_000.0),
            None,
            None,
            "usd",
            None,
            None,
            None,
        ));
    }

    let plan = plan_status
        .pointer("/planInfo/planName")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let account = status
        .pointer("/planStatus/planInfo/devinInfo/accountDisplayName")
        .and_then(|v| v.as_str())
        .or_else(|| status.get("name").and_then(|v| v.as_str()))
        .map(str::to_string);

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if meters.is_empty() {
            Some("Devin session connected, but no quota fields were returned.".into())
        } else {
            None
        },
        account,
        plan,
        meters,
        stale_age_secs: None,
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

    fn day(ts: i64, acus: f64) -> (i64, f64) {
        (ts, acus)
    }

    #[test]
    fn consumption_days_parse() {
        let v = serde_json::json!({
            "total_acus": 9.5,
            "consumption_by_date": [
                {"date": 1_759_737_600, "acus": 4.0, "acus_by_product": {"cascade": 0.0, "devin": 4.0, "terminal": 0.0}},
                {"date": 1_759_824_000, "acus": 5.5}
            ]
        });
        let days = consumption_days(&v);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0], (1_759_737_600, 4.0));
        assert_eq!(days[1], (1_759_824_000, 5.5));
    }

    #[test]
    fn meters_from_days_windows() {
        // Buckets are midnight-PST (08:00 UTC) unix seconds.
        let t0 = 1_760_889_600i64; // a bucket start
        let now = (t0 + 12 * 3600) as f64;
        let days: std::collections::BTreeMap<i64, f64> = [
            day(t0 - 31 * 86400, 1.0), // outside cycle sum only when fetched — included here
            day(t0 - 8 * 86400, 2.0),  // outside 7d window
            day(t0 - 86400, 4.0),      // yesterday
            day(t0, 3.0),              // today
        ]
        .into_iter()
        .collect();

        let meters = meters_from_days(&days, now, None);
        assert_eq!(meters.len(), 2);
        assert_eq!(meters[0].id, "acu_today");
        assert_eq!(meters[0].used, Some(3.0));
        assert_eq!(meters[1].id, "acu_7d");
        assert_eq!(meters[1].used, Some(7.0));
        assert!(meters.iter().all(|m| m.percent.is_none()));
        assert_eq!(meters[0].unit, "acu");
        assert_eq!(meters[0].window_seconds, Some(86400.0));
    }

    #[test]
    fn cycle_meter_uses_org_cap() {
        let t0 = 1_760_889_600i64;
        let now = (t0 + 3_600) as f64;
        let days: std::collections::BTreeMap<i64, f64> =
            [day(t0 - 86400, 40.0), day(t0, 60.0)].into_iter().collect();
        let meters = meters_from_days(&days, now, Some(200.0));
        let cycle = meters.iter().find(|m| m.id == "acu_cycle").unwrap();
        assert_eq!(cycle.used, Some(100.0));
        assert_eq!(cycle.limit, Some(200.0));
        assert!((cycle.percent.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_days_yield_no_meters() {
        let days: std::collections::BTreeMap<i64, f64> = Default::default();
        assert!(meters_from_days(&days, 0.0, Some(10.0)).is_empty());
    }

    #[test]
    fn file_credential_prefers_api_key() {
        let v: toml::Value = toml::from_str("api_token = \"cog_abc123\"").unwrap();
        assert!(matches!(
            file_credential(&v),
            Some(Credential::ApiKey(k)) if k == "cog_abc123"
        ));
        let nested: toml::Value = toml::from_str("[auth]\ntoken = \"apk_xyz\"").unwrap();
        assert!(matches!(
            file_credential(&nested),
            Some(Credential::ApiKey(k)) if k == "apk_xyz"
        ));
        let empty: toml::Value = toml::from_str("foo = \"bar\"").unwrap();
        assert!(file_credential(&empty).is_none());
    }

    #[test]
    fn file_credential_maps_devin_session_to_connect() {
        let v: toml::Value = toml::from_str(
            "windsurf_api_key = \"devin-session-x\"\napi_server_url = \"https://server.codeium.com\"",
        )
        .unwrap();
        let Some(Credential::CliSession { token, server }) = file_credential(&v) else {
            panic!("expected CLI session credential");
        };
        assert_eq!(token, "devin-session-x");
        assert_eq!(server, "https://server.codeium.com");

        // A `cog_` key anywhere in the file beats the session token.
        let both: toml::Value =
            toml::from_str("windsurf_api_key = \"devin-session-x\"\napi_key = \"cog_pref\"")
                .unwrap();
        assert!(matches!(
            file_credential(&both),
            Some(Credential::ApiKey(k)) if k == "cog_pref"
        ));
    }

    #[test]
    fn cli_status_maps_quota_meters() {
        let payload = serde_json::json!({
            "userStatus": {
                "name": "Gabriel Rodrigues",
                "planStatus": {
                    "planInfo": {
                        "planName": "Max",
                        "hideDailyQuota": true,
                        "devinInfo": {"accountDisplayName": "Firefly Labs"}
                    },
                    "dailyQuotaRemainingPercent": 100,
                    "weeklyQuotaRemainingPercent": 51,
                    "availablePromptCredits": -1,
                    "overageBalanceMicros": "-239976",
                    "weeklyQuotaResetAtUnix": "1791705600"
                }
            }
        });
        let snap = cli_snapshot_from_status(&payload);
        assert_eq!(snap.plan.as_deref(), Some("Max"));
        assert_eq!(snap.account.as_deref(), Some("Firefly Labs"));
        assert_eq!(snap.status, SnapshotStatus::Ok);

        // Daily meter suppressed by hideDailyQuota; unlimited credits skipped.
        let weekly = snap
            .meters
            .iter()
            .find(|m| m.id == "weekly_quota")
            .expect("weekly meter");
        assert!((weekly.left_percent.unwrap() - 0.51).abs() < 1e-9);
        assert_eq!(weekly.reset_at, Some(1_791_705_600.0));
        assert!(snap.meters.iter().all(|m| m.id != "daily_quota"));
        assert!(snap.meters.iter().all(|m| m.id != "prompt_credits"));

        let overage = snap
            .meters
            .iter()
            .find(|m| m.id == "overage")
            .expect("overage meter");
        assert!((overage.used.unwrap() - 0.239976).abs() < 1e-6);
    }

    #[test]
    fn cli_status_shows_daily_quota_when_visible() {
        let payload = serde_json::json!({
            "userStatus": {
                "planStatus": {
                    "planInfo": {"planName": "Pro"},
                    "dailyQuotaRemainingPercent": 25.0,
                    "availablePromptCredits": 30
                }
            }
        });
        let snap = cli_snapshot_from_status(&payload);
        let daily = snap
            .meters
            .iter()
            .find(|m| m.id == "daily_quota")
            .expect("daily meter");
        assert!((daily.left_percent.unwrap() - 0.25).abs() < 1e-9);
        let credits = snap
            .meters
            .iter()
            .find(|m| m.id == "prompt_credits")
            .expect("credits meter");
        assert_eq!(credits.left, Some(30.0));
    }

    #[test]
    fn sessions_items_sum_windows() {
        // Pure parse-check on the sessions response shape the fallback reads.
        let page = serde_json::json!({
            "has_more": false,
            "items": [
                {"session_id": "s1", "acus_consumed": 2.5, "created_at": "2026-10-01T10:00:00Z"},
                {"session_id": "s2", "acus_consumed": 1.0, "created_at": "2026-09-28T10:00:00Z"}
            ],
            "limit": 100, "skip": 0, "total": 2, "next_cursor": null
        });
        let items = page.get("items").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            items[0].get("acus_consumed").and_then(coerce_number),
            Some(2.5)
        );
        assert!(
            items[0]
                .get("created_at")
                .and_then(coerce_unix_seconds)
                .unwrap()
                > 1_700_000_000.0
        );
    }
}
