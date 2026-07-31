//! Export formats + scripting exit codes.

use std::io::{self, Write};

use anyhow::Result;

use crate::providers::types::{ProviderSnapshot, SnapshotStatus};

/// Prometheus text exposition for meters.
pub fn emit_prometheus(snaps: &[ProviderSnapshot]) -> Result<()> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "# HELP usagenometer_used_ratio Quota used as a unit interval (0..1)."
    )?;
    writeln!(out, "# TYPE usagenometer_used_ratio gauge")?;
    writeln!(
        out,
        "# HELP usagenometer_left_ratio Quota remaining as a unit interval (0..1)."
    )?;
    writeln!(out, "# TYPE usagenometer_left_ratio gauge")?;
    writeln!(
        out,
        "# HELP usagenometer_up Provider fetch success (1=ok)."
    )?;
    writeln!(out, "# TYPE usagenometer_up gauge")?;

    for snap in snaps {
        let up = if snap.status == SnapshotStatus::Ok {
            1.0
        } else {
            0.0
        };
        writeln!(
            out,
            "usagenometer_up{{provider=\"{}\"}} {}",
            escape_label(&snap.id),
            up
        )?;
        if snap.status != SnapshotStatus::Ok {
            continue;
        }
        for meter in &snap.meters {
            let used = meter
                .percent
                .or_else(|| meter.left_percent.map(|lp| 1.0 - lp));
            let left = meter
                .left_percent
                .or_else(|| meter.percent.map(|p| 1.0 - p));
            let labels = format!(
                "provider=\"{}\",meter=\"{}\",title=\"{}\"",
                escape_label(&snap.id),
                escape_label(&meter.id),
                escape_label(&meter.title)
            );
            if let Some(u) = used {
                writeln!(out, "usagenometer_used_ratio{{{labels}}} {u}")?;
            }
            if let Some(l) = left {
                writeln!(out, "usagenometer_left_ratio{{{labels}}} {l}")?;
            }
        }
    }
    Ok(())
}

fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Exit non-zero if any OK meter has remaining % below `fail_under` (0–100).
/// Semantics: `--fail-under 10` fails when remaining < 10% (i.e. used > 90%).
pub fn check_fail_under(snaps: &[ProviderSnapshot], fail_under: f64) -> (bool, Vec<String>) {
    let thr = fail_under.clamp(0.0, 100.0);
    let mut messages = Vec::new();
    let mut ok = true;
    for snap in snaps {
        if snap.status != SnapshotStatus::Ok {
            continue;
        }
        for meter in &snap.meters {
            let left = meter
                .left_percent
                .or_else(|| meter.percent.map(|p| 1.0 - p));
            let Some(left) = left else { continue };
            let left_pct = if left <= 1.0 { left * 100.0 } else { left };
            if left_pct < thr {
                ok = false;
                messages.push(format!(
                    "{} · {} at {:.0}% remaining (fail-under {:.0}%)",
                    snap.label, meter.title, left_pct, thr
                ));
            }
        }
    }
    (ok, messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderSnapshot, meter_from_used_percent};

    #[test]
    fn fail_under_triggers() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 95.0, None));
        let (ok, msgs) = check_fail_under(&[snap], 10.0);
        assert!(!ok);
        assert!(!msgs.is_empty());
    }

    #[test]
    fn fail_under_passes() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 50.0, None));
        let (ok, _) = check_fail_under(&[snap], 10.0);
        assert!(ok);
    }
}
