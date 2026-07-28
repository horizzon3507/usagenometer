//! Shared snapshot / meter types.

use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotStatus {
    Ok,
    Auth,
    Error,
    Disabled,
}

impl SnapshotStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Auth => "auth",
            Self::Error => "error",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageMeter {
    pub id: String,
    pub title: String,
    pub used: Option<f64>,
    pub left: Option<f64>,
    pub limit: Option<f64>,
    pub percent: Option<f64>,
    pub left_percent: Option<f64>,
    pub unit: String,
    pub reset_at: Option<f64>,
    pub reset_after_seconds: Option<f64>,
    pub window_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderSnapshot {
    pub id: String,
    pub label: String,
    pub status: SnapshotStatus,
    pub error: Option<String>,
    pub account: Option<String>,
    pub plan: Option<String>,
    pub meters: Vec<UsageMeter>,
}

impl ProviderSnapshot {
    pub fn ok(id: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: SnapshotStatus::Ok,
            error: None,
            account: None,
            plan: None,
            meters: vec![],
        }
    }

    pub fn fail(id: &str, label: &str, status: SnapshotStatus, error: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status,
            error: Some(error.into()),
            account: None,
            plan: None,
            meters: vec![],
        }
    }
}

pub fn create_meter(
    id: impl Into<String>,
    title: impl Into<String>,
    mut percent: Option<f64>,
    mut left_percent: Option<f64>,
    mut used: Option<f64>,
    mut left: Option<f64>,
    limit: Option<f64>,
    unit: &str,
    reset_at: Option<f64>,
    reset_after_seconds: Option<f64>,
    window_seconds: Option<f64>,
) -> UsageMeter {
    percent = percent.and_then(unit_interval);
    left_percent = left_percent.and_then(unit_interval);

    if percent.is_none()
        && let Some(lp) = left_percent
    {
        percent = Some(clamp01(1.0 - lp));
    }
    if left_percent.is_none()
        && let Some(p) = percent
    {
        left_percent = Some(clamp01(1.0 - p));
    }
    if used.is_none()
        && let (Some(l), Some(lim)) = (left, limit)
    {
        used = Some((lim - l).max(0.0));
    }
    if left.is_none()
        && let (Some(u), Some(lim)) = (used, limit)
    {
        left = Some((lim - u).max(0.0));
    }
    if percent.is_none()
        && let (Some(u), Some(lim)) = (used, limit)
        && lim > 0.0
    {
        percent = Some(clamp01(u / lim));
    }
    if left_percent.is_none()
        && let Some(p) = percent
    {
        left_percent = Some(clamp01(1.0 - p));
    }

    UsageMeter {
        id: id.into(),
        title: title.into(),
        used,
        left,
        limit,
        percent,
        left_percent,
        unit: unit.into(),
        reset_at,
        reset_after_seconds,
        window_seconds,
    }
}

pub fn meter_from_used_percent(
    id: &str,
    title: &str,
    used_percent: f64,
    reset_at: Option<f64>,
) -> UsageMeter {
    let mut fraction = used_percent;
    if fraction > 1.0 {
        fraction /= 100.0;
    }
    fraction = clamp01(fraction);
    create_meter(
        id,
        title,
        Some(fraction),
        Some(1.0 - fraction),
        Some(fraction * 100.0),
        Some((1.0 - fraction) * 100.0),
        Some(100.0),
        "percent",
        reset_at,
        None,
        None,
    )
}

pub fn meter_from_remaining_fraction(
    id: &str,
    title: &str,
    remaining: f64,
    reset_at: Option<f64>,
    window_seconds: Option<f64>,
) -> UsageMeter {
    let left = clamp01(remaining);
    create_meter(
        id,
        title,
        Some(1.0 - left),
        Some(left),
        Some((1.0 - left) * 100.0),
        Some(left * 100.0),
        Some(100.0),
        "percent",
        reset_at,
        None,
        window_seconds,
    )
}

pub fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn unit_interval(v: f64) -> Option<f64> {
    if !v.is_finite() {
        return None;
    }
    if v > 1.0 && v <= 100.0 {
        Some(clamp01(v / 100.0))
    } else {
        Some(clamp01(v))
    }
}

pub fn coerce_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn coerce_unix_seconds(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => {
            let v = n.as_f64()?;
            Some(if v > 9_999_999_999.0 { v / 1000.0 } else { v })
        }
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(n) = trimmed.parse::<f64>() {
                return Some(if n > 9_999_999_999.0 { n / 1000.0 } else { n });
            }
            OffsetDateTime::parse(trimmed, &Rfc3339)
                .ok()
                .map(|dt| dt.unix_timestamp() as f64 + f64::from(dt.nanosecond()) / 1e9)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_percent_meter() {
        let m = meter_from_used_percent("t", "T", 25.0, None);
        assert!((m.left_percent.unwrap() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn remaining_fraction_meter() {
        let m = meter_from_remaining_fraction("r", "R", 0.2, None, None);
        assert!((m.percent.unwrap() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn parses_rfc3339_z() {
        let v = coerce_unix_seconds(&serde_json::json!("2026-08-05T01:12:18.000Z"));
        assert!(v.unwrap() > 1_700_000_000.0);
    }
}
