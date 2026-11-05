//! fx (vercel-labs/fx) — AI Gateway credit balance, the same number
//! `fx credits` prints. Read-only reuse of fx's own credential sources in
//! fx's precedence order (`src/core/auth/credentials.zig`):
//! `VERCEL_OIDC_TOKEN` → `AI_GATEWAY_API_KEY` → `~/.fx/auth.json` (fx login)
//! → `~/.fx/api-key` (stored key, Linux file backend).
//!
//! `GET https://ai-gateway.vercel.sh/coding-agent/v1/credits` returns decimal
//! USD strings `{balance, used|total_used, plan?}`. An fx login can reach
//! several teams, so — like fx — its team rides in `?teamId=`; API keys carry
//! their own team. Expired logins are never refreshed here: fx rotates the
//! refresh token under its own `auth.lock`, and a second refresher would race
//! it. Codex / Grok subscriptions and custom endpoints have no Gateway
//! balance — their usage still lands in the `fx` token ledger.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::http::{HttpClient, HttpError};
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, coerce_number, create_meter};

const ID: &str = "fx";
const LABEL: &str = "fx";
const CREDITS_URL: &str = "https://ai-gateway.vercel.sh/coding-agent/v1/credits";
/// fx refreshes a login 60 s before expiry; treat it as expired from then on.
const EXPIRY_SKEW_MS: i64 = 60_000;

#[derive(Debug, PartialEq, Eq)]
struct Credential {
    token: String,
    /// Team for `?teamId=` (fx login only).
    team: Option<String>,
    /// Team slug shown as the account line (fx login only).
    account: Option<String>,
    source: &'static str,
}

pub fn fx_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fx")
}

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    let cred = match resolve_credential(now_ms()) {
        Ok(c) => c,
        Err(msg) => return ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Auth, msg),
    };
    let url = match cred.team.as_deref() {
        Some(team) if valid_team(team) => format!("{CREDITS_URL}?teamId={team}"),
        _ => CREDITS_URL.to_string(),
    };
    let bearer = format!("Bearer {}", cred.token);
    match client.get_json::<Value>(&url, &[("Authorization", bearer.as_str())]) {
        Ok(body) => snapshot_from_credits(&body, cred.account),
        Err(e) if e.is_auth_error() => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Auth,
            format!(
                "AI Gateway rejected the {} credential — run fx login",
                cred.source
            ),
        ),
        Err(HttpError::Status { status, .. }) => ProviderSnapshot::fail(
            ID,
            LABEL,
            SnapshotStatus::Error,
            format!("AI Gateway credits HTTP {status}"),
        ),
        Err(e) => ProviderSnapshot::fail(ID, LABEL, SnapshotStatus::Error, e.to_string()),
    }
}

fn resolve_credential(now_ms: i64) -> Result<Credential, String> {
    for (var, source) in [
        ("VERCEL_OIDC_TOKEN", "VERCEL_OIDC_TOKEN"),
        ("AI_GATEWAY_API_KEY", "AI_GATEWAY_API_KEY"),
    ] {
        if let Some(token) = std::env::var(var).ok().filter(|v| !v.trim().is_empty()) {
            return Ok(Credential {
                token: token.trim().to_string(),
                team: None,
                account: None,
                source,
            });
        }
    }
    let home = fx_home();
    let login = fs::read_to_string(home.join("auth.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let mut login_expired = false;
    if let Some(v) = login.as_ref() {
        match login_credential(v, now_ms) {
            Some(Ok(cred)) => return Ok(cred),
            Some(Err(())) => login_expired = true,
            None => {}
        }
    }
    if let Some(key) = fs::read_to_string(home.join("api-key"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Ok(Credential {
            token: key,
            team: None,
            account: None,
            source: "stored API key",
        });
    }
    Err(if login_expired {
        "fx login expired — run any fx command (or fx login) to refresh".into()
    } else {
        "no AI Gateway credential — run fx login or fx setup, or set AI_GATEWAY_API_KEY".into()
    })
}

/// `Some(Ok)` usable login, `Some(Err)` expired login, `None` not an fx login.
fn login_credential(v: &Value, now_ms: i64) -> Option<Result<Credential, ()>> {
    let token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let expires_at_ms = v.get("expires_at_ms").and_then(Value::as_i64)?;
    if expires_at_ms - EXPIRY_SKEW_MS <= now_ms {
        return Some(Err(()));
    }
    let str_field = |key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let team_slug = str_field("team_slug");
    Some(Ok(Credential {
        token: token.to_string(),
        team: str_field("team_id").or_else(|| team_slug.clone()),
        account: team_slug,
        source: "fx login",
    }))
}

/// fx only forwards URL-safe team ids (`validGatewayTeam`).
fn valid_team(team: &str) -> bool {
    !team.is_empty()
        && team.len() <= 128
        && team
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn snapshot_from_credits(body: &Value, account: Option<String>) -> ProviderSnapshot {
    let balance = body.get("balance").and_then(coerce_number);
    let used = body
        .get("used")
        .or_else(|| body.get("total_used"))
        .and_then(coerce_number);
    let mut snap = ProviderSnapshot::ok(ID, LABEL);
    snap.account = account;
    snap.plan = body
        .get("plan")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if balance.is_none() && used.is_none() {
        snap.error = Some("AI Gateway returned no credit balance".into());
        return snap;
    }
    // Lifetime spend vs. remaining balance is not a capped window (top-ups
    // grow both), so no percentage is manufactured.
    snap.meters.push(create_meter(
        "credits",
        "Gateway credits",
        None,
        None,
        used,
        balance,
        None,
        "usd",
        None,
        None,
        None,
    ));
    snap
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expired_login_is_not_used_and_team_id_wins_over_slug() {
        let login = json!({
            "version": 1, "access_token": "tok", "refresh_token": "r",
            "expires_at_ms": 10_000_000, "team_id": "team_1", "team_slug": "acme",
        });
        let fresh = login_credential(&login, 1_000).unwrap().unwrap();
        assert_eq!(fresh.team.as_deref(), Some("team_1"));
        assert_eq!(fresh.account.as_deref(), Some("acme"));
        // Inside the 60 s refresh skew counts as expired.
        assert!(
            login_credential(&login, 10_000_000 - 30_000)
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn credits_strings_map_to_balance_meter_without_percent() {
        let snap = snapshot_from_credits(
            &json!({"balance": "4.99999992", "total_used": "0.00000008", "plan": "pro"}),
            None,
        );
        assert_eq!(snap.plan.as_deref(), Some("pro"));
        let m = &snap.meters[0];
        assert_eq!(m.unit, "usd");
        assert!((m.left.unwrap() - 4.99999992).abs() < 1e-9);
        assert!(m.percent.is_none() && m.limit.is_none());

        let empty = snapshot_from_credits(&json!({}), None);
        assert!(empty.meters.is_empty() && empty.error.is_some());
    }

    #[test]
    fn unsafe_team_is_not_forwarded() {
        assert!(valid_team("team_000000000000000000000000"));
        assert!(!valid_team("team a/../b"));
        assert!(!valid_team(""));
    }
}
