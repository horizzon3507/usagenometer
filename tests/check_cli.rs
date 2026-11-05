use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use usagenometer::providers::types::{ProviderSnapshot, meter_from_used_percent};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-check-cli-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn cache(&self, provider: &str, used: Option<f64>, age: u64) {
        let mut snapshot = ProviderSnapshot::ok(provider, provider);
        if let Some(used) = used {
            snapshot
                .meters
                .push(meter_from_used_percent("quota", "Quota", used, None));
        }
        let dir = self.0.join("cache/usagenometer/snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        std::fs::write(
            dir.join(format!("{provider}.json")),
            serde_json::to_vec(&serde_json::json!({
                "saved_at": now - age as f64,
                "snapshot": snapshot,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn check(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_usg"))
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("CODEX_HOME", self.0.join("codex"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("claude"))
            .arg("check")
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_auth_is_unavailable_and_json_remains_parseable() {
    let sandbox = Sandbox::new();
    let output = sandbox.check(&["-p", "codex", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let snapshots: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots[0]["status"], "auth");
    assert!(!output.stderr.is_empty());
}

#[test]
fn rejected_auth_cannot_be_hidden_by_stale_quota() {
    let sandbox = Sandbox::new();
    sandbox.cache("codex", Some(20.0), 600);
    let output = sandbox.check(&["-p", "codex", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let snapshots: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots[0]["status"], "auth");
    assert!(snapshots[0].get("stale_age_secs").is_none());
}

#[test]
fn quota_at_remaining_threshold_passes() {
    let sandbox = Sandbox::new();
    sandbox.cache("codex", Some(50.0), 0);
    let output = sandbox.check(&["-p", "codex", "--fail-under", "50", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let snapshots: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots[0]["status"], "ok");
    assert!(output.stderr.is_empty());
}

#[test]
fn quota_breach_wins_over_an_unavailable_provider() {
    let sandbox = Sandbox::new();
    sandbox.cache("codex", Some(95.0), 0);
    let output = sandbox.check(&["-p", "codex", "-p", "claude", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let snapshots: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots.as_array().unwrap().len(), 2);
}

#[test]
fn snapshot_without_quota_is_unavailable() {
    let sandbox = Sandbox::new();
    sandbox.cache("codex", None, 0);
    let output = sandbox.check(&["-p", "codex", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let snapshots: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots[0]["meters"], serde_json::json!([]));
}

#[test]
fn budget_breach_wins_over_unavailable_quota() {
    let sandbox = Sandbox::new();
    let output = sandbox.check(&["-p", "codex", "--budget-usd", "0", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let snapshots: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshots[0]["status"], "auth");
}

#[test]
fn rejected_auth_invalidates_a_fresh_snapshot_for_later_checks() {
    let sandbox = Sandbox::new();
    sandbox.cache("codex", Some(20.0), 0);
    let config = sandbox.0.join("config/usagenometer/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "cache_ttl = 0\n").unwrap();
    let rejected = sandbox.check(&["-p", "codex", "--json"]);
    assert_eq!(rejected.status.code(), Some(1));
    std::fs::write(&config, "cache_ttl = 300\n").unwrap();
    let subsequent = sandbox.check(&["-p", "codex", "--json"]);
    assert_eq!(subsequent.status.code(), Some(1));
    let snapshots: serde_json::Value = serde_json::from_slice(&subsequent.stdout).unwrap();
    assert_eq!(snapshots[0]["status"], "auth");
}
