//! Persistent XDG config (`~/.config/usagenometer/config.toml`).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::{DisplayMode, ProviderArg};
use crate::paths;

/// On-disk config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    /// Default providers (empty = all). Names: codex, cursor, …
    pub providers: Vec<String>,
    /// Preferred display order of providers.
    pub provider_order: Vec<String>,
    /// Watch refresh interval in seconds.
    pub watch_interval: Option<u64>,
    /// `left` or `used`.
    pub display: Option<String>,
    /// Global alert threshold as used percent 0–100 (e.g. 80 = warn at ≥80% used).
    pub alert: Option<f64>,
    /// Alert when exhaustion ETA is at or below this many hours (needs history).
    pub alert_eta: Option<f64>,
    /// Per-provider alert thresholds (used %).
    pub alerts: HashMap<String, f64>,
    /// Hide/redact account emails.
    pub privacy: bool,
    /// Default to compact one-liner status.
    pub compact: bool,
    /// Desktop notify-send on threshold alerts.
    pub notify: bool,
    /// Short cache TTL for successful snapshots (seconds).
    pub cache_ttl: Option<u64>,
    /// Persist history snapshots on fetch (default true).
    #[serde(default = "default_true")]
    pub history: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            provider_order: Vec::new(),
            watch_interval: None,
            display: None,
            alert: None,
            alert_eta: None,
            alerts: HashMap::new(),
            privacy: false,
            compact: false,
            notify: false,
            cache_ttl: None,
            history: true,
        }
    }
}

impl ConfigFile {
    pub fn load() -> Self {
        let path = paths::config_file();
        Self::load_from(&path).unwrap_or_default()
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let cfg: ConfigFile = toml::from_str(&raw)
            .with_context(|| format!("parse config {}", path.display()))?;
        Ok(cfg)
    }

    pub fn path() -> PathBuf {
        paths::config_file()
    }

    pub fn display_mode(&self) -> DisplayMode {
        match self.display.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
            Some(s) if s == "used" => DisplayMode::Used,
            _ => DisplayMode::Left,
        }
    }

    pub fn parse_providers(&self) -> Vec<ProviderArg> {
        self.providers
            .iter()
            .filter_map(|name| parse_provider_name(name))
            .collect()
    }

    pub fn cache_ttl_secs(&self) -> u64 {
        self.cache_ttl.unwrap_or(300)
    }

    pub fn watch_interval_secs(&self) -> u64 {
        self.watch_interval.unwrap_or(60).max(5)
    }

    /// Threshold for a provider (used % 0–100), if any.
    pub fn alert_for(&self, provider_id: &str) -> Option<f64> {
        self.alerts
            .get(provider_id)
            .copied()
            .or(self.alert)
            .filter(|v| v.is_finite() && *v >= 0.0)
    }
}

fn parse_provider_name(name: &str) -> Option<ProviderArg> {
    match name.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(ProviderArg::Codex),
        "cursor" => Some(ProviderArg::Cursor),
        "antigravity" => Some(ProviderArg::Antigravity),
        "claude" => Some(ProviderArg::Claude),
        "grok" => Some(ProviderArg::Grok),
        _ => None,
    }
}

/// Runtime settings after merging config + CLI flags.
#[derive(Debug, Clone)]
pub struct Settings {
    pub providers: Vec<ProviderArg>,
    pub display: DisplayMode,
    pub quiet: bool,
    pub compact: bool,
    pub privacy: bool,
    pub json: bool,
    pub pretty: bool,
    pub format: Option<crate::cli::OutputFormat>,
    /// Global CLI alert override (used %).
    pub alert: Option<f64>,
    /// Alert when ETA ≤ this many hours (CLI overrides config).
    pub alert_eta: Option<f64>,
    pub notify: bool,
    pub cache_ttl: u64,
    pub history: bool,
    pub watch_interval: u64,
    pub config: ConfigFile,
}

impl Settings {
    pub fn alert_for(&self, provider_id: &str) -> Option<f64> {
        self.alert
            .or_else(|| self.config.alert_for(provider_id))
    }

    /// Hours until exhaustion; CLI flag overrides config.
    pub fn alert_eta_hours(&self) -> Option<f64> {
        self.alert_eta
            .or(self.config.alert_eta)
            .filter(|v| v.is_finite() && *v >= 0.0)
    }

    pub fn dump_toml(&self) -> String {
        let mut effective = self.config.clone();
        if !self.providers.is_empty() {
            effective.providers = self.providers.iter().map(|p| p.id().to_string()).collect();
        }
        effective.display = Some(match self.display {
            DisplayMode::Left => "left".into(),
            DisplayMode::Used => "used".into(),
        });
        effective.privacy = self.privacy;
        effective.compact = self.compact;
        effective.notify = self.notify;
        effective.cache_ttl = Some(self.cache_ttl);
        effective.history = self.history;
        effective.watch_interval = Some(self.watch_interval);
        if let Some(a) = self.alert {
            effective.alert = Some(a);
        }
        if let Some(h) = self.alert_eta {
            effective.alert_eta = Some(h);
        }
        toml::to_string_pretty(&effective).unwrap_or_else(|_| String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_toml() {
        let raw = r#"
privacy = true
alert = 80
compact = true
[alerts]
codex = 90
"#;
        let cfg: ConfigFile = toml::from_str(raw).unwrap();
        assert!(cfg.privacy);
        assert_eq!(cfg.alert, Some(80.0));
        assert_eq!(cfg.alert_for("codex"), Some(90.0));
        assert_eq!(cfg.alert_for("cursor"), Some(80.0));
    }
}
