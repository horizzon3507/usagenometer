//! Smart routing — suggest or select providers with headroom / reset urgency / runway.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use serde::Serialize;

use crate::providers::availability;
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, UsageMeter};

const LOW_REMAINING: f64 = 0.15; // 15% left
const HEADROOM: f64 = 0.35; // at least 35% left to suggest

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingStrategy {
    /// Highest remaining % across all meters (default).
    #[default]
    Headroom,
    /// Prioritize providers whose quota resets soonest (use-it-or-lose-it).
    Reset,
    /// Factor in local burn-rate ETA vs reset window.
    Runway,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteCandidate {
    pub provider_id: String,
    pub label: String,
    pub worst_left: f64,
    pub bottleneck_meter: String,
    pub reset_after_secs: Option<f64>,
    pub eta_secs: Option<f64>,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoutingHint {
    pub message: String,
}

fn meter_left_fraction(m: &UsageMeter) -> Option<f64> {
    let left = m.left_percent.or_else(|| m.percent.map(|p| 1.0 - p))?;
    let left = if left > 1.0 { left / 100.0 } else { left };
    Some(left.clamp(0.0, 1.0))
}

fn meter_reset_after(m: &UsageMeter, now: f64) -> Option<f64> {
    m.reset_at
        .map(|at| (at - now).max(0.0))
        .or_else(|| m.reset_after_seconds.filter(|s| *s > 0.0))
}

/// Primary remaining fraction for a snapshot (min across meters = worst).
fn worst_left(snap: &ProviderSnapshot) -> Option<f64> {
    let mut best: Option<f64> = None;
    for m in &snap.meters {
        let Some(left) = meter_left_fraction(m) else {
            continue;
        };
        best = Some(match best {
            Some(b) => b.min(left),
            None => left,
        });
    }
    best
}

fn bottleneck_meter<'a>(snap: &'a ProviderSnapshot) -> Option<(&'a UsageMeter, f64)> {
    let mut best: Option<(&'a UsageMeter, f64)> = None;
    for m in &snap.meters {
        let Some(left) = meter_left_fraction(m) else {
            continue;
        };
        if best.is_none_or(|(_, b)| left < b) {
            best = Some((m, left));
        }
    }
    best
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Rank verified providers meeting `min_left_pct` (0–100) using `strategy`.
pub fn rank_candidates(
    snaps: &[ProviderSnapshot],
    strategy: RoutingStrategy,
    min_left_pct: f64,
    etas: Option<&HashMap<String, f64>>,
    allow_stale: bool,
) -> Vec<RouteCandidate> {
    let now = now_secs();
    let min_left = (min_left_pct / 100.0).clamp(0.0, 1.0);
    let mut out = Vec::new();
    for snap in snaps {
        if snap.status != SnapshotStatus::Ok || snap.meters.is_empty() {
            continue;
        }
        if !allow_stale && snap.stale_age_secs.is_some() {
            continue;
        }
        let Some((meter, left)) = bottleneck_meter(snap) else {
            continue;
        };
        if left + f64::EPSILON < min_left {
            continue;
        }
        let reset_after = meter_reset_after(meter, now);
        let eta_secs = etas.and_then(|map| map.get(&format!("{}/{}", snap.id, meter.id)).copied());
        let score = match strategy {
            RoutingStrategy::Headroom => left,
            RoutingStrategy::Reset => match reset_after {
                Some(r) if r > 0.0 => left * (1.0 + (86_400.0 / r.max(1800.0)).min(3.0)),
                _ => left,
            },
            RoutingStrategy::Runway => match (eta_secs, reset_after) {
                (Some(eta), Some(reset)) if eta < reset => left * 0.3,
                (Some(eta), None) if eta < 3600.0 => left * 0.5,
                _ => left,
            },
        };
        out.push(RouteCandidate {
            provider_id: snap.id.clone(),
            label: snap.label.clone(),
            worst_left: left,
            bottleneck_meter: meter.title.clone(),
            reset_after_secs: reset_after,
            eta_secs,
            score,
            binary: availability::resolve_agent_binary(&snap.id),
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.worst_left
                    .partial_cmp(&a.worst_left)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    out
}

fn pct_label(left: f64) -> String {
    format!("{:.0}%", (left * 100.0).clamp(0.0, 100.0))
}

pub fn compute(snaps: &[ProviderSnapshot]) -> Option<RoutingHint> {
    let mut low: Vec<(&ProviderSnapshot, f64)> = Vec::new();
    let mut headroom: Vec<(&ProviderSnapshot, f64)> = Vec::new();

    for snap in snaps {
        if snap.status != SnapshotStatus::Ok
            || snap.meters.is_empty()
            || snap.stale_age_secs.is_some()
        {
            continue;
        }
        let Some(left) = worst_left(snap) else {
            continue;
        };
        if left < LOW_REMAINING {
            low.push((snap, left));
        } else if left >= HEADROOM {
            headroom.push((snap, left));
        }
    }

    if low.is_empty() || headroom.is_empty() {
        return None;
    }

    let low_names: Vec<String> = low
        .iter()
        .map(|(s, left)| format!("{} ({})", s.label, pct_label(*left)))
        .collect();
    let alt_names: Vec<String> = headroom
        .iter()
        .map(|(s, left)| format!("{} ({})", s.label, pct_label(*left)))
        .collect();
    let message = format!(
        "{} low → try {}",
        low_names.join(" / "),
        alt_names.join(" / ")
    );
    Some(RoutingHint { message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderSnapshot, create_meter, meter_from_used_percent};

    #[test]
    fn suggests_when_low() {
        let mut low = ProviderSnapshot::ok("codex", "Codex");
        low.meters
            .push(meter_from_used_percent("w", "Weekly", 92.0, None));
        let mut ok = ProviderSnapshot::ok("cursor", "Cursor");
        ok.meters
            .push(meter_from_used_percent("a", "Auto", 20.0, None));
        let hint = compute(&[low, ok]).unwrap();
        assert!(hint.message.contains("Codex"));
        assert!(hint.message.contains("Cursor"));
        assert!(hint.message.contains("8%"));
        assert!(hint.message.contains("80%"));
    }

    #[test]
    fn reset_strategy_prefers_expiring_headroom_and_skips_stale() {
        let mut soon = ProviderSnapshot::ok("claude", "Claude");
        soon.meters.push(create_meter(
            "5h",
            "5 hour",
            Some(0.6),
            Some(0.4),
            None,
            None,
            None,
            "percent",
            None,
            Some(1800.0),
            None,
        ));
        let mut weekly = ProviderSnapshot::ok("codex", "Codex");
        weekly.meters.push(create_meter(
            "w",
            "Weekly",
            Some(0.4),
            Some(0.6),
            None,
            None,
            None,
            "percent",
            None,
            Some(500_000.0),
            None,
        ));
        let mut stale = ProviderSnapshot::ok("cursor", "Cursor");
        stale.stale_age_secs = Some(600);
        stale
            .meters
            .push(meter_from_used_percent("a", "Auto", 5.0, None));

        let snaps = vec![soon, weekly, stale];
        let head = rank_candidates(&snaps, RoutingStrategy::Headroom, 10.0, None, false);
        assert_eq!(head.len(), 2);
        assert_eq!(head[0].provider_id, "codex");

        let reset = rank_candidates(&snaps, RoutingStrategy::Reset, 10.0, None, false);
        assert_eq!(reset[0].provider_id, "claude");
    }
}
