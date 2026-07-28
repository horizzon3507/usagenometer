//! Tiny JWT helpers (decode claims only — no verification).

use anyhow::{Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;

pub fn jwt_claims(token: &str) -> Result<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!("not a JWT"));
    }
    let raw = URL_SAFE_NO_PAD
        .decode(parts[1].as_bytes())
        .or_else(|_| {
            // Some tokens include padding
            base64::engine::general_purpose::URL_SAFE.decode(parts[1].as_bytes())
        })
        .map_err(|e| anyhow!("jwt payload b64: {e}"))?;
    serde_json::from_slice(&raw).map_err(|e| anyhow!("jwt claims json: {e}"))
}

pub fn jwt_exp(token: &str) -> Option<f64> {
    jwt_claims(token)
        .ok()
        .and_then(|c| c.get("exp").and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))))
}

pub fn jwt_sub(token: &str) -> Option<String> {
    jwt_claims(token)
        .ok()
        .and_then(|c| c.get("sub").and_then(|v| v.as_str().map(str::to_string)))
}

pub fn normalize_bearer(token: &str) -> String {
    let t = token.trim();
    let stripped = t
        .strip_prefix("Bearer ")
        .or_else(|| t.strip_prefix("bearer "))
        .unwrap_or(t);
    stripped.trim().to_string()
}
