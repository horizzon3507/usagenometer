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

    crate::tokens::scan_all(crate::tokens::collect::known_providers())?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed,
    Breached,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub outcome: CheckOutcome,
    pub messages: Vec<String>,
}

/// Check whether verifiable quota meters remain at least `fail_under` percent.
/// A meter exactly at the threshold passes; unavailable providers cannot prove a pass.
pub fn check_fail_under(snaps: &[ProviderSnapshot], fail_under: f64) -> CheckReport {
    let thr = fail_under.clamp(0.0, 100.0);
    let mut messages = Vec::new();
    let mut breached = false;
    let mut unavailable = false;
    let mut verified_quota = false;
    for snap in snaps {
        let provider = if snap.label.is_empty() {
            snap.id.as_str()
        } else {
            snap.label.as_str()
        };
        if snap.status != SnapshotStatus::Ok {
            unavailable = true;
            messages.push(format!(
                "{provider} ({}) quota unavailable: status {}{}",
                snap.id,
                snap.status.as_str(),
                snap.error
                    .as_deref()
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default()
            ));
            continue;
        }
        if let Some(age) = snap.stale_age_secs {
            unavailable = true;
            messages.push(format!(
                "{provider} ({}) quota unavailable: stale snapshot ({age}s old)",
                snap.id
            ));
            continue;
        }
        let mut has_quota = false;
        for meter in &snap.meters {
            let used_pct = meter
                .percent
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                .map(|used| if used <= 1.0 { used * 100.0 } else { used });
            let left_pct = used_pct.map(|used| 100.0 - used).or_else(|| {
                meter
                    .left_percent
                    .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                    .map(|left| if left <= 1.0 { left * 100.0 } else { left })
            });
            let Some(left_pct) = left_pct else {
                continue;
            };
            has_quota = true;
            let breached_threshold = used_pct
                .map(|used| used > 100.0 - thr)
                .unwrap_or(left_pct < thr);
            if breached_threshold {
                breached = true;
                messages.push(format!(
                    "{provider} · {} at {:.0}% remaining (fail-under {:.0}%)",
                    meter.title, left_pct, thr
                ));
            }
        }
        if has_quota {
            verified_quota = true;
        } else {
            unavailable = true;
            messages.push(format!(
                "{provider} ({}) quota unavailable: no usable percentage quota meters",
                snap.id
            ));
        }
    }
    if !verified_quota && messages.is_empty() {
        messages.push(
            "quota verification unavailable: no provider supplied a usable percentage quota".into(),
        );
    }
    let outcome = if breached {
        CheckOutcome::Breached
    } else if unavailable || !verified_quota {
        CheckOutcome::Unavailable
    } else {
        CheckOutcome::Passed
    };
    CheckReport { outcome, messages }
}

/// Exit non-zero when the period's total token cost reaches `budget_usd`
/// (`total >= budget`, so a `0` budget fails on any recorded spend).
pub fn check_budget_usd(total_usd: f64, budget_usd: f64, window: &str) -> (bool, Vec<String>) {
    if total_usd >= budget_usd {
        return (
            false,
            vec![format!(
                "token spend this {window} ${total_usd:.4} reached budget ${budget_usd:.2}"
            )],
        );
    }
    (true, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderSnapshot, meter_from_used_percent};

    #[test]
    fn budget_triggers_at_threshold() {
        let (ok, msgs) = check_budget_usd(10.0, 10.0, "week");
        assert!(!ok);
        assert!(!msgs.is_empty());
        let (ok, _) = check_budget_usd(9.99, 10.0, "week");
        assert!(ok);
        let (ok, _) = check_budget_usd(0.0, 0.0, "day");
        assert!(!ok); // 0 >= 0: spend met the budget
        let (ok, _) = check_budget_usd(0.0, 5.0, "month");
        assert!(ok);
    }

    #[test]
    fn fail_under_breaches_below_threshold() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 95.0, None));
        let report = check_fail_under(&[snap], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Breached);
        assert!(
            report
                .messages
                .iter()
                .any(|message| message.contains("Codex"))
        );
    }

    #[test]
    fn fail_under_accepts_exact_threshold() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 90.0, None));
        let report = check_fail_under(&[snap], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Passed);
        assert!(report.messages.is_empty());
    }

    #[test]
    fn fail_under_still_breaches_near_but_below_threshold() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 90.01, None));
        let report = check_fail_under(&[snap], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Breached);
    }

    #[test]
    fn fail_under_reports_auth_failure_as_unavailable() {
        let snap = ProviderSnapshot::fail("codex", "Codex", SnapshotStatus::Auth, "not logged in");
        let report = check_fail_under(&[snap], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Unavailable);
        assert!(report.messages[0].contains("Codex"));
        assert!(report.messages[0].contains("not logged in"));
    }

    #[test]
    fn fail_under_rejects_stale_snapshot() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.stale_age_secs = Some(600);
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 50.0, None));
        let report = check_fail_under(&[snap], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Unavailable);
        assert!(report.messages[0].contains("stale snapshot"));
    }

    #[test]
    fn fail_under_reports_missing_quotas() {
        let snap = ProviderSnapshot::ok("ledger-only", "Ledger only");
        let report = check_fail_under(&[snap], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Unavailable);
        assert!(report.messages[0].contains("no usable percentage quota meters"));
        let empty_report = check_fail_under(&[], 10.0);
        assert_eq!(empty_report.outcome, CheckOutcome::Unavailable);
        assert!(!empty_report.messages.is_empty());
    }

    #[test]
    fn fail_under_breach_precedes_unavailable_provider() {
        let mut breached = ProviderSnapshot::ok("codex", "Codex");
        breached
            .meters
            .push(meter_from_used_percent("w", "Weekly", 95.0, None));
        let unavailable =
            ProviderSnapshot::fail("claude", "Claude", SnapshotStatus::Error, "request failed");
        let report = check_fail_under(&[breached, unavailable], 10.0);
        assert_eq!(report.outcome, CheckOutcome::Breached);
        assert_eq!(report.messages.len(), 2);
        assert!(
            report
                .messages
                .iter()
                .any(|message| message.contains("Claude"))
        );
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
