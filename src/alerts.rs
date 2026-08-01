//! Threshold alerts + optional notify-send.
//!
//! Alerts fire on used-% thresholds and/or exhaustion ETA (from history).

use std::collections::HashMap;
use std::process::Command;

use crate::cli::DisplayMode;
use crate::config::Settings;
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, UsageMeter};
use crate::ui::print_warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    UsedPercent,
    Eta,
}

#[derive(Debug, Clone)]
pub struct AlertEvent {
    pub provider_id: String,
    pub provider_label: String,
    pub meter_title: String,
    pub used_pct: f64,
    pub threshold: f64,
    pub display: DisplayMode,
    pub kind: AlertKind,
    /// Seconds until exhaustion when `kind == Eta`.
    pub eta_seconds: Option<f64>,
}

/// Evaluate meters against used-% and optional ETA thresholds.
///
/// `etas` maps `"provider_id/meter_id"` → seconds until exhaustion.
pub fn evaluate(
    snaps: &[ProviderSnapshot],
    settings: &Settings,
    etas: Option<&HashMap<String, f64>>,
) -> Vec<AlertEvent> {
    let mut events = Vec::new();
    for snap in snaps {
        if snap.status != SnapshotStatus::Ok {
            continue;
        }
        if let Some(threshold) = settings.alert_for(&snap.id) {
            let thr = threshold.clamp(0.0, 100.0);
            for meter in &snap.meters {
                if let Some(ev) = check_meter(snap, meter, thr, settings.display) {
                    events.push(ev);
                }
            }
        }
        if let Some(eta_hours) = settings.alert_eta_hours() {
            let limit_secs = eta_hours.max(0.0) * 3600.0;
            if let Some(map) = etas {
                for meter in &snap.meters {
                    let key = format!("{}/{}", snap.id, meter.id);
                    let Some(&secs) = map.get(&key) else {
                        continue;
                    };
                    if secs <= limit_secs + f64::EPSILON {
                        let used = meter
                            .percent
                            .or_else(|| meter.left_percent.map(|lp| 1.0 - lp))
                            .unwrap_or(0.0);
                        let used_pct = if used <= 1.0 { used * 100.0 } else { used };
                        events.push(AlertEvent {
                            provider_id: snap.id.clone(),
                            provider_label: snap.label.clone(),
                            meter_title: meter.title.clone(),
                            used_pct,
                            threshold: eta_hours,
                            display: settings.display,
                            kind: AlertKind::Eta,
                            eta_seconds: Some(secs),
                        });
                    }
                }
            }
        }
    }
    events
}

fn check_meter(
    snap: &ProviderSnapshot,
    meter: &UsageMeter,
    threshold_used: f64,
    display: DisplayMode,
) -> Option<AlertEvent> {
    let used = meter
        .percent
        .or_else(|| meter.left_percent.map(|lp| 1.0 - lp))?;
    let used_pct = if used <= 1.0 { used * 100.0 } else { used };

    let triggered = match display {
        DisplayMode::Used => used_pct + f64::EPSILON >= threshold_used,
        DisplayMode::Left => {
            let left_pct = 100.0 - used_pct;
            let remaining_threshold = 100.0 - threshold_used;
            left_pct <= remaining_threshold + f64::EPSILON
        }
    };
    if !triggered {
        return None;
    }
    Some(AlertEvent {
        provider_id: snap.id.clone(),
        provider_label: snap.label.clone(),
        meter_title: meter.title.clone(),
        used_pct,
        threshold: threshold_used,
        display,
        kind: AlertKind::UsedPercent,
        eta_seconds: None,
    })
}

pub fn print_alerts(events: &[AlertEvent], quiet: bool) {
    if events.is_empty() {
        return;
    }
    for ev in events {
        let msg = match ev.kind {
            AlertKind::Eta => {
                let eta = ev
                    .eta_seconds
                    .map(crate::eta::format_eta)
                    .unwrap_or_else(|| "~?".into());
                format!(
                    "ALERT {} · {} ETA {} (threshold {:.1}h)",
                    ev.provider_label, ev.meter_title, eta, ev.threshold
                )
            }
            AlertKind::UsedPercent => match ev.display {
                DisplayMode::Used => format!(
                    "ALERT {} · {} at {:.0}% used (threshold {:.0}%)",
                    ev.provider_label, ev.meter_title, ev.used_pct, ev.threshold
                ),
                DisplayMode::Left => format!(
                    "ALERT {} · {} at {:.0}% left (threshold {:.0}% used)",
                    ev.provider_label,
                    ev.meter_title,
                    100.0 - ev.used_pct,
                    ev.threshold
                ),
            },
        };
        if quiet {
            eprintln!("! {msg}");
        } else {
            print_warn(&msg);
        }
    }
}

/// Best-effort desktop notification; silent if notify-send missing.
pub fn maybe_notify(events: &[AlertEvent], enabled: bool) {
    if !enabled || events.is_empty() {
        return;
    }
    let body: String = events
        .iter()
        .map(|ev| match ev.kind {
            AlertKind::Eta => {
                let eta = ev
                    .eta_seconds
                    .map(crate::eta::format_eta)
                    .unwrap_or_else(|| "~?".into());
                format!("{} {}: ETA {}", ev.provider_label, ev.meter_title, eta)
            }
            AlertKind::UsedPercent => format!(
                "{} {}: {:.0}% used",
                ev.provider_label, ev.meter_title, ev.used_pct
            ),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = Command::new("notify-send")
        .arg("--app-name=usagenometer")
        .arg("--urgency=normal")
        .arg("usagenometer alert")
        .arg(&body)
        .output();
}

/// Track which alerts already fired this process (provider/meter/kind) to avoid spam in watch.
pub struct AlertTracker {
    fired: std::collections::HashSet<String>,
}

impl AlertTracker {
    pub fn new() -> Self {
        Self {
            fired: std::collections::HashSet::new(),
        }
    }

    pub fn filter_new(&mut self, events: Vec<AlertEvent>) -> Vec<AlertEvent> {
        events
            .into_iter()
            .filter(|ev| {
                let kind = match ev.kind {
                    AlertKind::UsedPercent => "pct",
                    AlertKind::Eta => "eta",
                };
                let key = format!("{}/{}/{}", ev.provider_id, ev.meter_title, kind);
                self.fired.insert(key)
            })
            .collect()
    }
}

impl Default for AlertTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderSnapshot, meter_from_used_percent};

    fn settings_alert(thr: f64) -> Settings {
        use crate::config::ConfigFile;
        Settings {
            providers: vec![],
            display: DisplayMode::Used,
            quiet: true,
            compact: false,
            privacy: false,
            json: false,
            pretty: false,
            format: None,
            alert: Some(thr),
            alert_eta: None,
            notify: false,
            cache_ttl: 300,
            history: true,
            watch_interval: 60,
            config: ConfigFile::default(),
        }
    }

    fn settings_eta(hours: f64) -> Settings {
        use crate::config::ConfigFile;
        Settings {
            providers: vec![],
            display: DisplayMode::Used,
            quiet: true,
            compact: false,
            privacy: false,
            json: false,
            pretty: false,
            format: None,
            alert: None,
            alert_eta: Some(hours),
            notify: false,
            cache_ttl: 300,
            history: true,
            watch_interval: 60,
            config: ConfigFile::default(),
        }
    }

    #[test]
    fn triggers_above_threshold() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 85.0, None));
        let events = evaluate(&[snap], &settings_alert(80.0), None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AlertKind::UsedPercent);
    }

    #[test]
    fn no_trigger_below() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 50.0, None));
        let events = evaluate(&[snap], &settings_alert(80.0), None);
        assert!(events.is_empty());
    }

    #[test]
    fn triggers_eta_within_threshold() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 60.0, None));
        let mut etas = HashMap::new();
        etas.insert("codex/w".into(), 1800.0); // 30 minutes
        let events = evaluate(&[snap], &settings_eta(1.0), Some(&etas));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AlertKind::Eta);
    }

    #[test]
    fn no_eta_trigger_when_far() {
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("w", "Weekly", 60.0, None));
        let mut etas = HashMap::new();
        etas.insert("codex/w".into(), 10_800.0); // 3 hours
        let events = evaluate(&[snap], &settings_eta(1.0), Some(&etas));
        assert!(events.is_empty());
    }
}
