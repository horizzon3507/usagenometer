//! Routing hint — suggest providers with headroom when one is low.

use crate::providers::types::{ProviderSnapshot, SnapshotStatus};

const LOW_REMAINING: f64 = 0.15; // 15% left
const HEADROOM: f64 = 0.35; // at least 35% left to suggest

#[derive(Debug, Clone)]
pub struct RoutingHint {
    pub message: String,
}

/// Primary remaining fraction for a snapshot (min across meters = worst).
fn worst_left(snap: &ProviderSnapshot) -> Option<f64> {
    let mut best: Option<f64> = None; // minimum left
    for m in &snap.meters {
        let left = m
            .left_percent
            .or_else(|| m.percent.map(|p| 1.0 - p))?;
        let left = if left > 1.0 { left / 100.0 } else { left };
        best = Some(match best {
            Some(b) => b.min(left),
            None => left,
        });
    }
    best
}

fn pct_label(left: f64) -> String {
    format!("{:.0}%", (left * 100.0).clamp(0.0, 100.0))
}

pub fn compute(snaps: &[ProviderSnapshot]) -> Option<RoutingHint> {
    let mut low: Vec<(&ProviderSnapshot, f64)> = Vec::new();
    let mut headroom: Vec<(&ProviderSnapshot, f64)> = Vec::new();

    for snap in snaps {
        if snap.status != SnapshotStatus::Ok || snap.meters.is_empty() {
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
    use crate::providers::types::{ProviderSnapshot, meter_from_used_percent};

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
}
