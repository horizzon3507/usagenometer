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
    writeln!(out, "# HELP usagenometer_up Provider fetch success (1=ok).")?;
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

    // Token ledger: best-effort rescan so an exporter call stays current,
    // then emit whatever the store holds. Scans are idempotent and
    // failure-tolerant; a missing/unopenable store emits nothing.
    let _ = crate::tokens::scan_all(crate::tokens::collect::known_providers());
    emit_token_metrics(&mut out)?;
    Ok(())
}

/// Token-ledger metrics from [`crate::tokens::TokenStore`]:
///
/// - `usagenometer_tokens_total{provider,model,kind}` — cumulative counter
///   (`kind` is `input|output|cache_read|cache_write`)
/// - `usagenometer_token_events_total{provider}` — recorded events
/// - `usagenometer_tokens_last_scan_unixtime` — last successful `scan_all`
///
/// Emits the HELP/TYPE headers always and samples only for data present —
/// a store that never opens still produces a valid (header-only) section.
pub fn emit_token_metrics(out: &mut impl Write) -> io::Result<()> {
    writeln!(
        out,
        "# HELP usagenometer_tokens_total Cumulative agent tokens recorded in the local ledger."
    )?;
    writeln!(out, "# TYPE usagenometer_tokens_total counter")?;
    writeln!(
        out,
        "# HELP usagenometer_token_events_total Token usage events recorded in the local ledger."
    )?;
    writeln!(out, "# TYPE usagenometer_token_events_total counter")?;
    writeln!(
        out,
        "# HELP usagenometer_tokens_last_scan_unixtime Unix time of the last completed ledger scan."
    )?;
    writeln!(out, "# TYPE usagenometer_tokens_last_scan_unixtime gauge")?;

    let Ok(store) = crate::tokens::TokenStore::open() else {
        return Ok(());
    };
    let totals = store.totals().unwrap_or_default();
    let counts = store.provider_event_counts().unwrap_or_default();
    let last = store.last_scan_unix().ok().flatten();
    write_token_metrics(out, &totals, &counts, last)
}

/// Sample emission for token metrics — pure for tests.
fn write_token_metrics(
    out: &mut impl Write,
    totals: &[crate::tokens::TokenTotals],
    counts: &[(String, u64)],
    last_scan: Option<f64>,
) -> io::Result<()> {
    for t in totals {
        let base = format!(
            "provider=\"{}\",model=\"{}\"",
            escape_label(&t.provider),
            escape_label(t.model.as_deref().unwrap_or(""))
        );
        for (kind, value) in [
            ("input", t.input_tokens),
            ("output", t.output_tokens),
            ("cache_read", t.cache_read_tokens),
            ("cache_write", t.cache_write_tokens),
        ] {
            writeln!(
                out,
                "usagenometer_tokens_total{{{base},kind=\"{kind}\"}} {value}"
            )?;
        }
    }
    for (provider, count) in counts {
        writeln!(
            out,
            "usagenometer_token_events_total{{provider=\"{}\"}} {}",
            escape_label(provider),
            count
        )?;
    }
    if let Some(last) = last_scan {
        writeln!(out, "usagenometer_tokens_last_scan_unixtime {last}")?;
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

    #[test]
    fn token_metrics_format() {
        let totals = vec![crate::tokens::TokenTotals {
            provider: "grok".into(),
            model: Some("grok-4".into()),
            input_tokens: 100,
            output_tokens: 40,
            cache_read_tokens: 10,
            cache_write_tokens: 0,
            events: 3,
        }];
        let counts = vec![("grok".to_string(), 3u64)];
        let mut buf = Vec::new();
        emit_token_metrics(&mut buf).unwrap();
        write_token_metrics(&mut buf, &totals, &counts, Some(1_700_000_000.0)).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("# TYPE usagenometer_tokens_total counter"));
        assert!(text.contains(
            "usagenometer_tokens_total{provider=\"grok\",model=\"grok-4\",kind=\"input\"} 100"
        ));
        assert!(text.contains(
            "usagenometer_tokens_total{provider=\"grok\",model=\"grok-4\",kind=\"cache_read\"} 10"
        ));
        assert!(text.contains("usagenometer_token_events_total{provider=\"grok\"} 3"));
        assert!(text.contains("usagenometer_tokens_last_scan_unixtime 1700000000"));
    }
}
