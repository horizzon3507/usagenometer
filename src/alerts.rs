//! Threshold alerts + optional notify-send.
//!
//! Alerts fire on used-% thresholds and/or exhaustion ETA (from history).

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::DisplayMode;
use crate::config::Settings;
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, UsageMeter};
use crate::ui::print_warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    UsedPercent,
    Eta,
    LowBalanceUsd,
}

#[derive(Debug, Clone, serde::Serialize)]
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
    /// Remaining balance in USD when `kind == LowBalanceUsd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_left_usd: Option<f64>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct PersistedAlertState {
    active: BTreeSet<String>,
}

/// Alert state survives one-shot systemd runs, so a quota already above its
/// threshold does not notify again every timer interval.
pub struct AlertStateStore {
    path: PathBuf,
    state: PersistedAlertState,
}

pub struct AlertReconciliation {
    pub new_events: Vec<AlertEvent>,
    pub recovered: Vec<String>,
}

impl AlertStateStore {
    pub fn open() -> Self {
        Self::open_at(crate::paths::cache_dir().join("alerts.json"))
    }

    fn open_at(path: PathBuf) -> Self {
        let state = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self { path, state }
    }

    pub fn reconcile(&mut self, events: Vec<AlertEvent>) -> AlertReconciliation {
        let now: BTreeSet<String> = events.iter().map(alert_key).collect();
        let recovered = self
            .state
            .active
            .difference(&now)
            .cloned()
            .collect::<Vec<_>>();
        let new_events = events
            .into_iter()
            .filter(|event| !self.state.active.contains(&alert_key(event)))
            .collect();
        self.state.active = now;
        let _ = self.save();
        AlertReconciliation {
            new_events,
            recovered,
        }
    }

    fn save(&self) -> std::io::Result<()> {
        crate::paths::ensure_dir(&self.path).map_err(std::io::Error::other)?;
        let raw = serde_json::to_string(&self.state).map_err(std::io::Error::other)?;
        fs::write(&self.path, raw)
    }
}

fn alert_key(event: &AlertEvent) -> String {
    let kind = match event.kind {
        AlertKind::UsedPercent => "pct",
        AlertKind::Eta => "eta",
        AlertKind::LowBalanceUsd => "usd",
    };
    format!("{}/{}/{}", event.provider_id, event.meter_title, kind)
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn resets_before(meter: &UsageMeter, eta_secs: f64, now: f64) -> bool {
    let reset_in = meter
        .reset_at
        .map(|r| r - now)
        .or(meter.reset_after_seconds);
    matches!(reset_in, Some(r) if r > 0.0 && r < eta_secs)
}

/// Evaluate meters against used-%, low-balance USD, and optional ETA thresholds.
///
/// `etas` maps `"provider_id/meter_id"` → seconds until exhaustion.
pub fn evaluate(
    snaps: &[ProviderSnapshot],
    settings: &Settings,
    etas: Option<&HashMap<String, f64>>,
) -> Vec<AlertEvent> {
    let mut events = Vec::new();
    let now = now_secs();
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
        if let Some(bal_thr) = settings.alert_balance_for(&snap.id) {
            for meter in &snap.meters {
                if meter.unit.eq_ignore_ascii_case("usd")
                    && let Some(left) = meter.left.filter(|v| v.is_finite())
                    && left <= bal_thr + f64::EPSILON
                {
                    events.push(AlertEvent {
                        provider_id: snap.id.clone(),
                        provider_label: snap.label.clone(),
                        meter_title: meter.title.clone(),
                        used_pct: 0.0,
                        threshold: bal_thr,
                        display: settings.display,
                        kind: AlertKind::LowBalanceUsd,
                        eta_seconds: None,
                        balance_left_usd: Some(left),
                    });
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
                    if secs <= limit_secs + f64::EPSILON && !resets_before(meter, secs, now) {
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
                            balance_left_usd: None,
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
        balance_left_usd: None,
    })
}

pub fn format_alert_message(ev: &AlertEvent) -> String {
    match ev.kind {
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
        AlertKind::LowBalanceUsd => format!(
            "ALERT {} · {} balance ${:.2} (threshold ${:.2})",
            ev.provider_label,
            ev.meter_title,
            ev.balance_left_usd.unwrap_or(0.0),
            ev.threshold
        ),
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
    }
}

pub fn print_alerts(events: &[AlertEvent], quiet: bool) {
    if events.is_empty() {
        return;
    }
    for ev in events {
        let msg = format_alert_message(ev);
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
            AlertKind::LowBalanceUsd => format!(
                "{} {}: ${:.2} remaining",
                ev.provider_label,
                ev.meter_title,
                ev.balance_left_usd.unwrap_or(0.0)
            ),
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

pub fn maybe_notify_recoveries(recovered: &[String], enabled: bool) {
    if !enabled || recovered.is_empty() {
        return;
    }
    let body = recovered
        .iter()
        .map(|key| format!("{key} is back below its alert threshold"))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = Command::new("notify-send")
        .arg("--app-name=usagenometer")
        .arg("--urgency=low")
        .arg("usagenometer recovered")
        .arg(body)
        .output();
}

/// Fire configured webhook URL and/or shell command on alert state transitions.
pub fn dispatch_hooks(transitions: &AlertReconciliation, settings: &Settings) {
    if transitions.new_events.is_empty() && transitions.recovered.is_empty() {
        return;
    }
    if let Some(url) = settings
        .config
        .notify_webhook_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Ok(client) = crate::http::HttpClient::new()
    {
        let payload = serde_json::json!({
            "source": "usagenometer",
            "new_events": transitions.new_events,
            "recovered": transitions.recovered,
        });
        let _ = client.post_json::<serde_json::Value>(url, &payload, &[]);
    }
    if let Some(cmd) = settings
        .config
        .notify_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let mut lines: Vec<String> = transitions
            .new_events
            .iter()
            .map(format_alert_message)
            .collect();
        for r in &transitions.recovered {
            lines.push(format!("RECOVERED {r}"));
        }
        let summary = lines.join("\n");
        let _ = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("USG_ALERT_SUMMARY", summary)
            .output();
    }
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
        let active: std::collections::HashSet<String> = events.iter().map(alert_key).collect();
        self.fired.retain(|k| active.contains(k));
        events
            .into_iter()
            .filter(|ev| self.fired.insert(alert_key(ev)))
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
            selection: crate::providers::AgentSelection::Ready,
            display: DisplayMode::Used,
            quiet: true,
            compact: false,
            compact_cost: false,
            template: None,
            privacy: false,
            json: false,
            pretty: false,
            format: None,
            alert: Some(thr),
            alert_eta: None,
            alert_balance_usd: None,
            notify: false,
            cache_ttl: 300,
            history: true,
            watch_interval: 60,
            profile: None,
            active_project_config: None,
            config: ConfigFile::default(),
        }
    }

    fn settings_eta(hours: f64) -> Settings {
        use crate::config::ConfigFile;
        Settings {
            providers: vec![],
            selection: crate::providers::AgentSelection::Ready,
            display: DisplayMode::Used,
            quiet: true,
            compact: false,
            compact_cost: false,
            template: None,
            privacy: false,
            json: false,
            pretty: false,
            format: None,
            alert: None,
            alert_eta: Some(hours),
            alert_balance_usd: None,
            notify: false,
            cache_ttl: 300,
            history: true,
            watch_interval: 60,
            profile: None,
            active_project_config: None,
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

    #[test]
    fn persisted_state_only_emits_on_threshold_crossing() {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-alert-state-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let event = AlertEvent {
            provider_id: "codex".into(),
            provider_label: "Codex".into(),
            meter_title: "5 hour".into(),
            used_pct: 90.0,
            threshold: 80.0,
            display: DisplayMode::Used,
            kind: AlertKind::UsedPercent,
            eta_seconds: None,
            balance_left_usd: None,
        };
        let mut first = AlertStateStore::open_at(path.clone());
        assert_eq!(first.reconcile(vec![event.clone()]).new_events.len(), 1);
        let mut second = AlertStateStore::open_at(path.clone());
        assert!(second.reconcile(vec![event]).new_events.is_empty());
        let recovered = second.reconcile(vec![]);
        assert_eq!(recovered.recovered.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn low_balance_usd_and_reset_before_eta_suppression() {
        let mut snap = ProviderSnapshot::ok("fx", "fx");
        snap.meters.push(crate::providers::types::create_meter(
            "credits",
            "Gateway credits",
            None,
            None,
            Some(8.0),
            Some(1.5),
            None,
            "usd",
            None,
            None,
            None,
        ));
        let mut s = settings_alert(80.0);
        s.alert_balance_usd = Some(2.0);
        let events = evaluate(&[snap], &s, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AlertKind::LowBalanceUsd);

        // Meter resets in 900s while ETA is 1800s -> suppress false ETA alarm.
        let mut snap2 = ProviderSnapshot::ok("codex", "Codex");
        snap2.meters.push(crate::providers::types::create_meter(
            "5h",
            "5 hour",
            Some(0.6),
            Some(0.4),
            None,
            None,
            None,
            "percent",
            None,
            Some(900.0),
            None,
        ));
        let mut etas = HashMap::new();
        etas.insert("codex/5h".into(), 1800.0);
        let eta_events = evaluate(&[snap2], &settings_eta(1.0), Some(&etas));
        assert!(eta_events.is_empty());
    }
}
