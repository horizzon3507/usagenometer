//! Persistent XDG config (`~/.config/usagenometer/config.toml`), profiles, and `.usg.toml`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::{DisplayMode, ProviderArg};
use crate::paths;

/// Named multi-account profile override (`[profiles.<name>]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileConfig {
    pub providers: Option<Vec<String>>,
    pub budget_usd: Option<f64>,
    pub budget_period: Option<String>,
    pub alert: Option<f64>,
    pub alert_eta: Option<f64>,
    pub privacy: Option<bool>,
    /// Optional env overrides applied when this profile is active (e.g. CODEX_HOME).
    pub env: HashMap<String, String>,
}

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
    /// Global low-balance USD alert threshold for prepaid credit providers.
    pub alert_balance_usd: Option<f64>,
    /// Per-provider alert thresholds (used %).
    pub alerts: HashMap<String, f64>,
    /// Per-provider low-balance USD alert thresholds.
    pub alerts_balance_usd: HashMap<String, f64>,
    /// Hide/redact account emails.
    pub privacy: bool,
    /// Default to compact one-liner status.
    pub compact: bool,
    /// Include today's token USD spend in compact output.
    pub compact_cost: bool,
    /// Custom template string for compact output (`{label} {pct}{stale}`).
    pub compact_template: Option<String>,
    /// Desktop notify-send on threshold alerts.
    pub notify: bool,
    /// Optional webhook URL receiving JSON alert payloads on state transitions.
    pub notify_webhook_url: Option<String>,
    /// Optional shell command invoked on alert state transitions (`USG_ALERT_SUMMARY`).
    pub notify_command: Option<String>,
    /// Short cache TTL for successful snapshots (seconds).
    pub cache_ttl: Option<u64>,
    /// Persist history snapshots on fetch (default true).
    #[serde(default = "default_true")]
    pub history: bool,
    /// Default USD budget for `usg check` (CLI `--budget-usd` wins).
    pub budget_usd: Option<f64>,
    /// Default budget window: "day" | "week" | "month" (CLI `--period` wins).
    pub budget_period: Option<String>,
    /// Per-model USD-per-1M-token pricing overrides: `[pricing."<model>"]`.
    pub pricing: HashMap<String, crate::pricing::ModelPriceOverride>,
    /// Named account profiles (`[profiles.<name>]`).
    pub profiles: HashMap<String, ProfileConfig>,
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
            alert_balance_usd: None,
            alerts: HashMap::new(),
            alerts_balance_usd: HashMap::new(),
            privacy: false,
            compact: false,
            compact_cost: false,
            compact_template: None,
            notify: false,
            notify_webhook_url: None,
            notify_command: None,
            cache_ttl: None,
            history: true,
            budget_usd: None,
            budget_period: None,
            pricing: HashMap::new(),
            profiles: HashMap::new(),
        }
    }
}

impl ConfigFile {
    pub fn load() -> Self {
        let path = paths::config_file();
        Self::load_from(&path).unwrap_or_default()
    }

    pub fn load_with_options(
        explicit_path: Option<&Path>,
        profile: Option<&str>,
        allow_project: bool,
    ) -> Result<(Self, Option<PathBuf>, Vec<String>)> {
        let mut warnings = Vec::new();
        let path = explicit_path
            .map(PathBuf::from)
            .unwrap_or_else(paths::config_file);
        let mut cfg = if explicit_path.is_some() {
            Self::load_from(&path)?
        } else {
            match Self::load_from(&path) {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(format!("{e}"));
                    Self::default()
                }
            }
        };

        let mut active_project = None;
        if allow_project && let Some(proj_path) = paths::project_config_file() {
            match Self::load_from(&proj_path) {
                Ok(overlay) => {
                    cfg.merge_project(&overlay);
                    active_project = Some(proj_path);
                }
                Err(e) => warnings.push(format!("project config ignored ({e})")),
            }
        }

        if let Some(name) = profile.map(str::trim).filter(|s| !s.is_empty())
            && !cfg.apply_profile(name)
        {
            warnings.push(format!("unknown profile '{name}'"));
        }

        Ok((cfg, active_project, warnings))
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        let cfg: ConfigFile =
            toml::from_str(&raw).with_context(|| format!("parse config {}", path.display()))?;
        Ok(cfg)
    }

    pub fn merge_project(&mut self, overlay: &ConfigFile) {
        if !overlay.providers.is_empty() {
            self.providers = overlay.providers.clone();
        }
        if !overlay.provider_order.is_empty() {
            self.provider_order = overlay.provider_order.clone();
        }
        if overlay.watch_interval.is_some() {
            self.watch_interval = overlay.watch_interval;
        }
        if overlay.display.is_some() {
            self.display = overlay.display.clone();
        }
        if overlay.alert.is_some() {
            self.alert = overlay.alert;
        }
        if overlay.alert_eta.is_some() {
            self.alert_eta = overlay.alert_eta;
        }
        if overlay.alert_balance_usd.is_some() {
            self.alert_balance_usd = overlay.alert_balance_usd;
        }
        for (k, v) in &overlay.alerts {
            self.alerts.insert(k.clone(), *v);
        }
        for (k, v) in &overlay.alerts_balance_usd {
            self.alerts_balance_usd.insert(k.clone(), *v);
        }
        if overlay.budget_usd.is_some() {
            self.budget_usd = overlay.budget_usd;
        }
        if overlay.budget_period.is_some() {
            self.budget_period = overlay.budget_period.clone();
        }
        for (k, v) in &overlay.pricing {
            self.pricing.insert(k.clone(), *v);
        }
    }

    pub fn apply_profile(&mut self, name: &str) -> bool {
        let Some(profile) = self.profiles.get(name).cloned() else {
            return false;
        };
        if let Some(p) = profile.providers {
            self.providers = p;
        }
        if let Some(b) = profile.budget_usd {
            self.budget_usd = Some(b);
        }
        if let Some(bp) = profile.budget_period {
            self.budget_period = Some(bp);
        }
        if let Some(a) = profile.alert {
            self.alert = Some(a);
        }
        if let Some(ae) = profile.alert_eta {
            self.alert_eta = Some(ae);
        }
        if let Some(priv_mode) = profile.privacy {
            self.privacy = priv_mode;
        }
        for (k, v) in profile.env {
            let trimmed = k.trim();
            if !trimmed.is_empty() {
                // Profile explicitly overrides provider home/credential env vars for this process.
                unsafe {
                    std::env::set_var(trimmed, v);
                }
            }
        }
        true
    }

    pub fn path() -> PathBuf {
        paths::config_file()
    }

    pub fn display_mode(&self) -> DisplayMode {
        match self
            .display
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
        {
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

    /// Low-balance USD threshold for a provider, if any.
    pub fn alert_balance_for(&self, provider_id: &str) -> Option<f64> {
        self.alerts_balance_usd
            .get(provider_id)
            .copied()
            .or(self.alert_balance_usd)
            .filter(|v| v.is_finite() && *v >= 0.0)
    }

    /// Default USD budget, if configured.
    pub fn budget(&self) -> Option<f64> {
        self.budget_usd.filter(|v| v.is_finite() && *v >= 0.0)
    }

    /// Default budget window ("day" | "week" | "month"), if configured.
    pub fn budget_window(&self) -> Option<String> {
        self.budget_period
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| matches!(s.as_str(), "day" | "week" | "month"))
    }

    /// Validate fields and return human-readable error strings (empty = valid).
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        for name in &self.providers {
            if parse_provider_name(name).is_none() {
                errs.push(format!("unknown provider in `providers`: '{name}'"));
            }
        }
        for name in &self.provider_order {
            if parse_provider_name(name).is_none() {
                errs.push(format!("unknown provider in `provider_order`: '{name}'"));
            }
        }
        for (name, val) in &self.alerts {
            if parse_provider_name(name).is_none() {
                errs.push(format!("unknown provider in `[alerts]`: '{name}'"));
            }
            if !val.is_finite() || !(0.0..=100.0).contains(val) {
                errs.push(format!(
                    "invalid `[alerts].{name}` value {val} (want 0..=100)"
                ));
            }
        }
        for (name, val) in &self.alerts_balance_usd {
            if parse_provider_name(name).is_none() {
                errs.push(format!(
                    "unknown provider in `[alerts_balance_usd]`: '{name}'"
                ));
            }
            if !val.is_finite() || *val < 0.0 {
                errs.push(format!(
                    "invalid `[alerts_balance_usd].{name}` value {val} (want >= 0)"
                ));
            }
        }
        if let Some(d) = self.display.as_deref() {
            let norm = d.trim().to_ascii_lowercase();
            if !matches!(norm.as_str(), "left" | "used") {
                errs.push(format!("invalid `display`: '{d}' (want 'left' or 'used')"));
            }
        }
        if let Some(bp) = self.budget_period.as_deref() {
            let norm = bp.trim().to_ascii_lowercase();
            if !matches!(norm.as_str(), "day" | "week" | "month") {
                errs.push(format!(
                    "invalid `budget_period`: '{bp}' (want 'day', 'week', or 'month')"
                ));
            }
        }
        if let Some(a) = self.alert
            && (!a.is_finite() || !(0.0..=100.0).contains(&a))
        {
            errs.push(format!("invalid `alert`: {a} (want 0..=100)"));
        }
        for (pname, p) in &self.profiles {
            if let Some(list) = &p.providers {
                for name in list {
                    if parse_provider_name(name).is_none() {
                        errs.push(format!(
                            "unknown provider in `[profiles.{pname}].providers`: '{name}'"
                        ));
                    }
                }
            }
            if let Some(bp) = p.budget_period.as_deref() {
                let norm = bp.trim().to_ascii_lowercase();
                if !matches!(norm.as_str(), "day" | "week" | "month") {
                    errs.push(format!(
                        "invalid `[profiles.{pname}].budget_period`: '{bp}' (want 'day', 'week', or 'month')"
                    ));
                }
            }
            if let Some(b) = p.budget_usd
                && (!b.is_finite() || b < 0.0)
            {
                errs.push(format!(
                    "invalid `[profiles.{pname}].budget_usd`: {b} (want >= 0)"
                ));
            }
            if let Some(a) = p.alert
                && (!a.is_finite() || !(0.0..=100.0).contains(&a))
            {
                errs.push(format!(
                    "invalid `[profiles.{pname}].alert`: {a} (want 0..=100)"
                ));
            }
        }
        errs
    }

    pub fn starter_template() -> &'static str {
        r#"# usagenometer configuration (~/.config/usagenometer/config.toml)

# providers = ["codex", "claude", "cursor"]  # empty = all installed & ready
provider_order = ["codex", "claude", "cursor", "antigravity", "grok"]
watch_interval = 60
display = "left"        # "left" (remaining) or "used"
cache_ttl = 300         # seconds before live API refresh
history = true          # persist SQLite snapshots for ETA & runway
privacy = false         # redact account emails & project names
compact = false
compact_cost = false    # append today's token USD spend in `usg -c`

# Alerts & budgets
# alert = 80            # warn at >= 80% used
# alert_eta = 2         # warn when exhaustion ETA <= 2 hours
# alert_balance_usd = 2.0
# notify = false
# notify_webhook_url = "https://example.com/hook"
# notify_command = "echo \"$USG_ALERT_SUMMARY\""
# budget_usd = 5.0
# budget_period = "day" # "day" | "week" | "month"

# [alerts]
# codex = 90

# [pricing."my-local-model"]
# input = 1.0
# output = 2.0

# [profiles.work]
# providers = ["claude", "codex"]
# [profiles.work.env]
# CODEX_HOME = "~/.codex-work"
"#
    }
}

pub fn parse_provider_name(name: &str) -> Option<ProviderArg> {
    match name.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(ProviderArg::Codex),
        "cursor" => Some(ProviderArg::Cursor),
        "antigravity" => Some(ProviderArg::Antigravity),
        "claude" => Some(ProviderArg::Claude),
        "grok" => Some(ProviderArg::Grok),
        "glm" => Some(ProviderArg::Glm),
        "kimi" => Some(ProviderArg::Kimi),
        "devin-cloud" | "devin_cloud" | "devincloud" => Some(ProviderArg::DevinCloud),
        "fx" => Some(ProviderArg::Fx),
        "omp" => Some(ProviderArg::Omp),
        "commandcode" | "command-code" | "command_code" => Some(ProviderArg::CommandCode),
        "opencode" => Some(ProviderArg::OpenCode),
        "opencode-go" => Some(ProviderArg::OpenCodeGo),
        "opencode-zen" => Some(ProviderArg::OpenCodeZen),
        "droid" => Some(ProviderArg::Droid),
        "pi" => Some(ProviderArg::Pi),
        "devin-local" => Some(ProviderArg::DevinLocal),
        "gemini" => Some(ProviderArg::Gemini),
        "openrouter" => Some(ProviderArg::OpenRouter),
        "deepseek" => Some(ProviderArg::DeepSeek),
        "windsurf" => Some(ProviderArg::Windsurf),
        "cline" => Some(ProviderArg::Cline),
        "roo-code" | "roocode" | "roo_code" => Some(ProviderArg::RooCode),
        "aider" => Some(ProviderArg::Aider),
        "goose" => Some(ProviderArg::Goose),
        _ => None,
    }
}

/// Runtime settings after merging config + CLI flags.
#[derive(Debug, Clone)]
pub struct Settings {
    pub providers: Vec<ProviderArg>,
    pub selection: crate::providers::AgentSelection,
    pub display: DisplayMode,
    pub quiet: bool,
    pub compact: bool,
    pub compact_cost: bool,
    pub template: Option<String>,
    pub privacy: bool,
    pub json: bool,
    pub pretty: bool,
    pub format: Option<crate::cli::OutputFormat>,
    /// Global CLI alert override (used %).
    pub alert: Option<f64>,
    /// Alert when ETA ≤ this many hours (CLI overrides config).
    pub alert_eta: Option<f64>,
    /// Global CLI low-balance USD alert override.
    pub alert_balance_usd: Option<f64>,
    pub notify: bool,
    pub cache_ttl: u64,
    pub history: bool,
    pub watch_interval: u64,
    pub profile: Option<String>,
    pub active_project_config: Option<PathBuf>,
    pub config: ConfigFile,
}

impl Settings {
    pub fn alert_for(&self, provider_id: &str) -> Option<f64> {
        self.alert.or_else(|| self.config.alert_for(provider_id))
    }

    pub fn alert_balance_for(&self, provider_id: &str) -> Option<f64> {
        self.alert_balance_usd
            .or_else(|| self.config.alert_balance_for(provider_id))
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
        effective.compact_cost = self.compact_cost;
        effective.compact_template = self.template.clone().or(effective.compact_template);
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
        if let Some(b) = self.alert_balance_usd {
            effective.alert_balance_usd = Some(b);
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

    #[test]
    fn parses_budget_and_pricing() {
        let raw = r#"
budget_usd = 5
budget_period = "week"

[pricing."my-model"]
input = 1.0
output = 2.0
"#;
        let cfg: ConfigFile = toml::from_str(raw).unwrap();
        assert_eq!(cfg.budget(), Some(5.0));
        assert_eq!(cfg.budget_window().as_deref(), Some("week"));
        let o = cfg.pricing.get("my-model").unwrap();
        assert_eq!(o.input, Some(1.0));
        assert_eq!(o.output, Some(2.0));
        assert_eq!(o.cache_read, None);
    }

    #[test]
    fn budget_window_rejects_unknown() {
        let raw = "budget_period = \"year\"";
        let cfg: ConfigFile = toml::from_str(raw).unwrap();
        assert_eq!(cfg.budget_window(), None);
    }

    #[test]
    fn validate_catches_bad_provider_and_display() {
        let raw = r#"
providers = ["bogus-ai"]
display = "percent"
budget_period = "decade"
"#;
        let cfg: ConfigFile = toml::from_str(raw).unwrap();
        let errs = cfg.validate();
        assert_eq!(errs.len(), 3);
    }

    #[test]
    fn profile_and_project_merge() {
        let raw = r#"
providers = ["codex"]
[profiles.work]
providers = ["claude"]
budget_usd = 12.5
"#;
        let mut cfg: ConfigFile = toml::from_str(raw).unwrap();
        assert!(cfg.apply_profile("work"));
        assert_eq!(cfg.providers, vec!["claude"]);
        assert_eq!(cfg.budget(), Some(12.5));
    }

    #[test]
    fn validate_catches_bad_profile_fields() {
        let raw = r#"
[profiles.bad]
providers = ["not-an-agent"]
budget_period = "bogus-value"
budget_usd = -3.0
alert = 120.0
"#;
        let cfg: ConfigFile = toml::from_str(raw).unwrap();
        let errs = cfg.validate();
        assert_eq!(errs.len(), 4);
        assert!(errs.iter().any(|e| e.contains("budget_period")));
        assert!(errs.iter().any(|e| e.contains("providers")));
        assert!(errs.iter().any(|e| e.contains("budget_usd")));
        assert!(errs.iter().any(|e| e.contains("alert")));
    }
}
