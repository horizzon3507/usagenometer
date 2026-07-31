//! Exhaustion ETA from history / watch samples.

use std::collections::HashMap;
use std::time::Duration;

use crate::providers::types::{ProviderSnapshot, UsageMeter};

#[derive(Debug, Clone)]
pub struct MeterEta {
    pub provider_id: String,
    pub meter_id: String,
    pub meter_title: String,
    /// Estimated seconds until 100% used, if burn rate > 0.
    pub seconds: Option<f64>,
    pub samples: usize,
}

/// Linear ETA to 100% used from (time, used_fraction) points. Needs ≥2 samples
/// with positive average burn rate.
pub fn eta_from_points(points: &[(f64, f64)]) -> Option<(f64, usize)> {
    if points.len() < 2 {
        return None;
    }
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let first = sorted.first()?;
    let last = sorted.last()?;
    let dt = last.0 - first.0;
    if dt <= 0.0 {
        return None;
    }
    let du = last.1 - first.1;
    if du <= 1e-6 {
        return None; // not burning (or recovering)
    }
    let rate = du / dt; // used fraction per second
    let remaining = (1.0 - last.1).clamp(0.0, 1.0);
    if remaining <= 0.0 {
        return Some((0.0, sorted.len()));
    }
    Some((remaining / rate, sorted.len()))
}

/// Estimate ETAs for each meter of a provider from history points.
pub fn etas_for_provider(
    provider_id: &str,
    points: &[crate::history::MeterPoint],
) -> Vec<MeterEta> {
    let mut by_meter: HashMap<String, (String, Vec<(f64, f64)>)> = HashMap::new();
    for p in points {
        let entry = by_meter
            .entry(p.meter_id.clone())
            .or_insert_with(|| (p.meter_title.clone(), Vec::new()));
        entry.1.push((p.recorded_at, p.used_percent));
    }
    let mut out = Vec::new();
    for (meter_id, (title, pts)) in by_meter {
        let (seconds, samples) = match eta_from_points(&pts) {
            Some((s, n)) => (Some(s), n),
            None => (None, pts.len()),
        };
        out.push(MeterEta {
            provider_id: provider_id.into(),
            meter_id,
            meter_title: title,
            seconds,
            samples,
        });
    }
    out
}

/// Format ETA for display, e.g. `~2h10m`.
pub fn format_eta(seconds: f64) -> String {
    let secs = seconds.max(0.0) as u64;
    if secs == 0 {
        return "~0m".into();
    }
    let d = Duration::from_secs(secs);
    format!("~{}", crate::ui::fmt_duration(d))
}

/// Attach ETA strings keyed by `"provider_id/meter_id"`.
pub fn eta_map_from_history(
    history: &crate::history::HistoryStore,
    snaps: &[ProviderSnapshot],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for snap in snaps {
        if let Ok(points) = history.recent_meter_points(&snap.id, 24) {
            for eta in etas_for_provider(&snap.id, &points) {
                if let Some(secs) = eta.seconds {
                    if eta.samples >= 2 {
                        map.insert(
                            format!("{}/{}", eta.provider_id, eta.meter_id),
                            format_eta(secs),
                        );
                    }
                }
            }
        }
    }
    map
}

/// From in-memory watch samples: list of (unix_secs, snapshot meters used%).
pub fn eta_from_watch_ring(
    provider_id: &str,
    meter: &UsageMeter,
    ring: &[(f64, f64)], // (time, used_fraction) for this meter
) -> Option<String> {
    let _ = (provider_id, meter);
    eta_from_points(ring).map(|(s, _)| format_eta(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_linear() {
        // 0% at t=0, 50% at t=3600 → 100% in another 3600s
        let pts = vec![(0.0, 0.0), (3600.0, 0.5)];
        let (secs, n) = eta_from_points(&pts).unwrap();
        assert_eq!(n, 2);
        assert!((secs - 3600.0).abs() < 1.0);
    }

    #[test]
    fn no_eta_when_flat() {
        let pts = vec![(0.0, 0.4), (100.0, 0.4)];
        assert!(eta_from_points(&pts).is_none());
    }
}
