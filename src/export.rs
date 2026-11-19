//! Export formats + scripting exit codes.

use std::collections::HashMap;
use std::io::{self, Write};

use anyhow::Result;

use crate::cli::DisplayMode;
use crate::pricing::ModelPriceOverride;
use crate::providers::types::{ProviderSnapshot, SnapshotStatus};

/// Prometheus text exposition for meters.
pub fn emit_prometheus(snaps: &[ProviderSnapshot]) -> Result<()> {
    emit_prometheus_with_pricing(snaps, &HashMap::new())
}

pub fn emit_prometheus_with_pricing(
    snaps: &[ProviderSnapshot],
    overrides: &HashMap<String, ModelPriceOverride>,
) -> Result<()> {
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
    writeln!(
        out,
        "# HELP usagenometer_meter_used Raw used value for a meter."
    )?;
    writeln!(out, "# TYPE usagenometer_meter_used gauge")?;
    writeln!(
        out,
        "# HELP usagenometer_meter_left Raw remaining value for a meter."
    )?;
    writeln!(out, "# TYPE usagenometer_meter_left gauge")?;
    writeln!(
        out,
        "# HELP usagenometer_meter_limit Raw limit value for a meter."
    )?;
    writeln!(out, "# TYPE usagenometer_meter_limit gauge")?;
    writeln!(
        out,
        "# HELP usagenometer_reset_unixtime Unix timestamp when quota resets."
    )?;
    writeln!(out, "# TYPE usagenometer_reset_unixtime gauge")?;
    writeln!(
        out,
        "# HELP usagenometer_snapshot_stale_age_seconds Age of stale fallback snapshot (0 when live)."
    )?;
    writeln!(out, "# TYPE usagenometer_snapshot_stale_age_seconds gauge")?;

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
        writeln!(
            out,
            "usagenometer_snapshot_stale_age_seconds{{provider=\"{}\"}} {}",
            escape_label(&snap.id),
            snap.stale_age_secs.unwrap_or(0)
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
            let unit_norm = meter.unit.to_ascii_lowercase();
            let unit_labels = format!("{labels},unit=\"{}\"", escape_label(&unit_norm));
            if let Some(u) = used {
                writeln!(out, "usagenometer_used_ratio{{{labels}}} {u}")?;
            }
            if let Some(l) = left {
                writeln!(out, "usagenometer_left_ratio{{{labels}}} {l}")?;
            }
            if let Some(u) = meter.used {
                writeln!(out, "usagenometer_meter_used{{{unit_labels}}} {u}")?;
            }
            if let Some(l) = meter.left {
                writeln!(out, "usagenometer_meter_left{{{unit_labels}}} {l}")?;
            }
            if let Some(lim) = meter.limit {
                writeln!(out, "usagenometer_meter_limit{{{unit_labels}}} {lim}")?;
            }
            if let Some(reset_at) = meter.reset_at {
                writeln!(out, "usagenometer_reset_unixtime{{{labels}}} {reset_at}")?;
            }
        }
    }

    crate::tokens::scan_all(crate::tokens::collect::known_providers())?;
    emit_token_metrics_with_pricing(&mut out, overrides)?;
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
    emit_token_metrics_with_pricing(out, &HashMap::new())
}

pub fn emit_token_metrics_with_pricing(
    out: &mut impl Write,
    overrides: &HashMap<String, ModelPriceOverride>,
) -> io::Result<()> {
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
        "# HELP usagenometer_cost_usd_total Estimated cumulative USD cost per provider/model."
    )?;
    writeln!(out, "# TYPE usagenometer_cost_usd_total counter")?;
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
    write_token_metrics_with_pricing(out, &totals, &counts, last, overrides)
}

#[cfg_attr(not(test), allow(dead_code))]
fn write_token_metrics(
    out: &mut impl Write,
    totals: &[crate::tokens::TokenTotals],
    counts: &[(String, u64)],
    last_scan: Option<f64>,
) -> io::Result<()> {
    write_token_metrics_with_pricing(out, totals, counts, last_scan, &HashMap::new())
}

fn write_token_metrics_with_pricing(
    out: &mut impl Write,
    totals: &[crate::tokens::TokenTotals],
    counts: &[(String, u64)],
    last_scan: Option<f64>,
    overrides: &HashMap<String, ModelPriceOverride>,
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
        if let Some(model) = t.model.as_deref()
            && let Some(p) = crate::pricing::price_for_merged(model, overrides)
        {
            let usd = (t.input_tokens as f64 * p.input
                + t.output_tokens as f64 * p.output
                + t.cache_read_tokens as f64 * p.cache_read
                + t.cache_write_tokens as f64 * p.cache_write)
                / 1_000_000.0;
            writeln!(out, "usagenometer_cost_usd_total{{{base}}} {usd:.6}")?;
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

/// Emit single-line Waybar JSON: `{"text","tooltip","class","percentage"}`.
pub fn emit_waybar(
    snaps: &[ProviderSnapshot],
    display: DisplayMode,
    alert_for: impl Fn(&str) -> Option<f64>,
    etas: Option<&HashMap<String, String>>,
    template: Option<&str>,
    cost_suffix: Option<&str>,
) -> Result<()> {
    let text = crate::ui::format_compact_line(snaps, display, template, cost_suffix)
        .unwrap_or_else(|| "—".into());
    let mut tooltip_lines = Vec::new();
    let mut worst_used_pct: f64 = 0.0;
    let mut has_pct = false;
    let mut class = "normal";

    for snap in snaps {
        if snap.status != SnapshotStatus::Ok {
            continue;
        }
        let mut header = snap.label.clone();
        if let Some(plan) = snap.plan.as_deref().filter(|s| !s.is_empty()) {
            header.push_str(&format!(" ({plan})"));
        }
        if let Some(age) = snap.stale_age_secs {
            header.push_str(&format!(" [stale {}m]", age / 60));
        }
        tooltip_lines.push(header);
        let warn_thr = alert_for(&snap.id);
        for m in &snap.meters {
            let used = m.percent.or_else(|| m.left_percent.map(|lp| 1.0 - lp));
            let left = m.left_percent.or_else(|| m.percent.map(|p| 1.0 - p));
            let shown_f = match display {
                DisplayMode::Left => left,
                DisplayMode::Used => used,
            };
            let val = shown_f
                .map(|f| format!("{:.0}%", f * 100.0))
                .or_else(|| crate::ui::money_value(m, display).map(|v| format!("${v:.2}")))
                .unwrap_or_else(|| "—".into());
            let reset = crate::ui::format_reset(m);
            let eta = etas
                .and_then(|map| map.get(&format!("{}/{}", snap.id, m.id)))
                .map(|e| format!("  ·  eta {e}"))
                .unwrap_or_default();
            tooltip_lines.push(format!("  {}: {val}{reset}{eta}", m.title));

            if let Some(u) = used {
                let u_pct = if u <= 1.0 { u * 100.0 } else { u };
                has_pct = true;
                if u_pct > worst_used_pct {
                    worst_used_pct = u_pct;
                }
                if u_pct >= 95.0 {
                    class = "critical";
                } else if class != "critical" && warn_thr.is_some_and(|t| u_pct + f64::EPSILON >= t)
                {
                    class = "warning";
                }
            }
        }
    }

    let percentage = if has_pct {
        match display {
            DisplayMode::Used => worst_used_pct.round().clamp(0.0, 100.0) as u64,
            DisplayMode::Left => (100.0 - worst_used_pct).round().clamp(0.0, 100.0) as u64,
        }
    } else {
        0
    };

    let payload = serde_json::json!({
        "text": text,
        "tooltip": tooltip_lines.join("\n"),
        "class": class,
        "percentage": percentage,
    });
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}

/// `(provider, group keys) -> (cost_usd, unpriced_events)`
pub type GroupCostMap = HashMap<(String, Vec<String>), (f64, usize)>;

/// Emit grouped token rows as RFC 4180 CSV.
pub fn emit_tokens_csv(
    rows: &[crate::tokens::GroupRow],
    dim_names: &[String],
    costs: Option<&GroupCostMap>,
) -> Result<()> {
    let mut out = io::stdout().lock();
    let mut headers: Vec<&str> = vec!["provider"];
    for d in dim_names {
        headers.push(d.as_str());
    }
    headers.extend([
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "total_tokens",
    ]);
    if costs.is_some() {
        headers.extend(["cost_usd", "unpriced_events"]);
    }
    writeln!(out, "{}", headers.join(","))?;
    for row in rows {
        let mut fields = vec![csv_escape(&row.provider)];
        for k in &row.keys {
            fields.push(csv_escape(k));
        }
        fields.extend([
            row.totals.input.to_string(),
            row.totals.output.to_string(),
            row.totals.cache_read.to_string(),
            row.totals.cache_write.to_string(),
            row.totals.total().to_string(),
        ]);
        if let Some(cost_map) = costs {
            let (usd, unpriced) = cost_map
                .get(&(row.provider.clone(), row.keys.clone()))
                .copied()
                .unwrap_or_default();
            fields.push(format!("{usd:.6}"));
            fields.push(unpriced.to_string());
        }
        writeln!(out, "{}", fields.join(","))?;
    }
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
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
    check_fail_under_ext(snaps, fail_under, None, None, None)
}

/// Extended quota + USD balance + ETA gate.
pub fn check_fail_under_ext(
    snaps: &[ProviderSnapshot],
    fail_under: f64,
    min_balance_usd: Option<f64>,
    etas: Option<&HashMap<String, f64>>,
    fail_eta_hours: Option<f64>,
) -> CheckReport {
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
            if let Some(left_pct) = left_pct {
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
            } else if let Some(min_usd) = min_balance_usd
                && meter.unit.eq_ignore_ascii_case("usd")
                && let Some(left_usd) = meter.left.filter(|v| v.is_finite())
            {
                has_quota = true;
                if left_usd < min_usd {
                    breached = true;
                    messages.push(format!(
                        "{provider} · {} at ${left_usd:.2} remaining (min-balance-usd ${min_usd:.2})",
                        meter.title
                    ));
                }
            }
            if let Some(eta_limit_h) = fail_eta_hours
                && let Some(eta_map) = etas
                && let Some(&eta_secs) = eta_map.get(&format!("{}/{}", snap.id, meter.id))
                && eta_secs <= eta_limit_h.max(0.0) * 3600.0 + f64::EPSILON
            {
                breached = true;
                messages.push(format!(
                    "{provider} · {} exhaustion ETA {} (fail-eta-hours {:.1}h)",
                    meter.title,
                    crate::eta::format_eta(eta_secs),
                    eta_limit_h
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
