#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "usg-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        Self(root)
    }

    fn install(&self, name: &str) {
        let path = self.0.join("bin").join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn auth(&self, raw: &str) {
        std::fs::create_dir_all(self.0.join("codex")).unwrap();
        std::fs::write(self.0.join("codex/auth.json"), raw).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_usg"))
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", self.0.join("bin"))
            .env("CODEX_HOME", self.0.join("codex"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .args(args)
            .output()
            .unwrap()
    }

    fn list(&self, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["providers", "-p", "codex", "--json"];
        args.extend_from_slice(extra);
        let output = self.run(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn credentials_without_an_installed_agent_do_not_appear_by_default() {
    let sandbox = Sandbox::new();
    sandbox.auth(r#"{"tokens":{"access_token":"opaque-token"}}"#);
    assert_eq!(sandbox.list(&[]), serde_json::json!([]));
    assert_eq!(
        sandbox.list(&["--include-unconfigured"]),
        serde_json::json!([])
    );
    let all = sandbox.list(&["--all"]);
    assert_eq!(all[0]["installed"], false);
    sandbox.install("codex");
    let ready = sandbox.list(&[]);
    assert_eq!(ready[0]["id"], "codex");
    assert_eq!(ready[0]["ready"], true);
    assert_eq!(
        sandbox.list(&["--only-unconfigured"]),
        serde_json::json!([])
    );
}

#[test]
fn missing_malformed_empty_and_expired_credentials_require_setup() {
    let sandbox = Sandbox::new();
    sandbox.install("codex");
    for raw in [
        None,
        Some("not-json"),
        Some(r#"{"tokens":{"access_token":" "}}"#),
        Some(r#"{"tokens":{"access_token":"e30.eyJleHAiOjF9.signature"}}"#),
    ] {
        if let Some(raw) = raw {
            sandbox.auth(raw);
        }
        assert_eq!(sandbox.list(&[]), serde_json::json!([]));
        let missing = sandbox.list(&["--only-unconfigured"]);
        assert_eq!(missing[0]["installed"], true);
        assert_eq!(missing[0]["ready"], false);
        assert!(
            missing[0]["setup"]
                .as_str()
                .unwrap()
                .contains("codex login")
        );
        assert_eq!(sandbox.list(&["--include-unconfigured"])[0]["id"], "codex");
    }
}

#[test]
fn session_files_and_non_executable_binaries_are_not_installations() {
    let sandbox = Sandbox::new();
    sandbox.auth(r#"{"tokens":{"access_token":"opaque-token"}}"#);
    std::fs::create_dir_all(sandbox.0.join("codex/sessions")).unwrap();
    std::fs::write(sandbox.0.join("bin/codex"), "not executable").unwrap();
    assert_eq!(sandbox.list(&[]), serde_json::json!([]));
    assert_eq!(sandbox.list(&["--all"])[0]["installed"], false);
}

#[test]
fn selection_flags_are_mutually_exclusive() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["providers", "--all", "--only-unconfigured"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn historical_usage_is_hidden_without_credentials_but_remains_available_with_all() {
    let sandbox = Sandbox::new();
    sandbox.install("fx");
    std::fs::create_dir_all(sandbox.0.join(".fx")).unwrap();
    std::fs::write(
        sandbox.0.join(".fx/usage.jsonl"),
        concat!(
            "{\"schema_version\":1,\"kind\":\"generation\",\"fact\":",
            "{\"id\":\"selection-event\",\"created_at_ms\":1790856000000,",
            "\"model\":\"openai/gpt-5.5\",\"input_tokens\":100,\"output_tokens\":20}}\n"
        ),
    )
    .unwrap();
    let hidden = sandbox.run(&["tokens", "-p", "fx", "--period", "all", "--json"]);
    assert!(hidden.status.success());
    let hidden: serde_json::Value = serde_json::from_slice(&hidden.stdout).unwrap();
    assert_eq!(hidden["totals"]["input_tokens"], 0);
    let all = sandbox.run(&["tokens", "-p", "fx", "--period", "all", "--json", "--all"]);
    assert!(all.status.success());
    let all: serde_json::Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all["totals"]["input_tokens"], 100);
    assert_eq!(all["totals"]["output_tokens"], 20);
}

#[test]
fn all_escapes_config_selection_but_keeps_an_explicit_provider_filter() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.0.join("config/usagenometer")).unwrap();
    std::fs::write(
        sandbox.0.join("config/usagenometer/config.toml"),
        "providers = [\"fx\"]\n",
    )
    .unwrap();
    let output = sandbox.run(&["providers", "--all", "--json"]);
    assert!(output.status.success());
    let all: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codex = all
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "codex")
        .unwrap();
    assert_eq!(codex["installed"], false);
    let filtered = sandbox.list(&["--all"]);
    assert_eq!(
        filtered
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["codex"]
    );
}

#[test]
fn opencode_go_and_zen_are_selectable_with_independent_credentials() {
    let sandbox = Sandbox::new();
    sandbox.install("opencode");
    std::fs::create_dir_all(sandbox.0.join("data/opencode")).unwrap();
    let db = rusqlite::Connection::open(sandbox.0.join("data/opencode/opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE credential (integration_id TEXT, value TEXT, active INTEGER, time_updated INTEGER);
         INSERT INTO credential VALUES (
           'opencode',
           '{\"type\":\"oauth\",\"access\":\"console-access\",\"expires\":4102444800000,\"metadata\":{\"server\":\"https://opencode.ai/console\",\"orgID\":\"org_test\"}}',
           1, 1
         );",
    ).unwrap();
    let output = sandbox.run(&[
        "providers",
        "-p",
        "opencode-go",
        "-p",
        "opencode-zen",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ready: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        ready
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["opencode-zen"]
    );
    let output = sandbox.run(&[
        "providers",
        "-p",
        "opencode-go",
        "-p",
        "opencode-zen",
        "--json",
        "--only-unconfigured",
    ]);
    assert!(output.status.success());
    let missing: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        missing
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["opencode-go"]
    );
}

#[test]
fn zen_model_keys_do_not_make_console_billing_ready() {
    let sandbox = Sandbox::new();
    sandbox.install("opencode");
    std::fs::create_dir_all(sandbox.0.join("data/opencode")).unwrap();
    std::fs::write(
        sandbox.0.join("data/opencode/auth.json"),
        r#"{"opencode":{"type":"api","key":"model-api-key"}}"#,
    )
    .unwrap();
    let ready = sandbox.run(&["providers", "-p", "opencode-zen", "--json"]);
    assert!(ready.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ready.stdout).unwrap(),
        serde_json::json!([])
    );
    let setup = sandbox.run(&[
        "providers",
        "-p",
        "opencode-zen",
        "--only-unconfigured",
        "--json",
    ]);
    assert!(setup.status.success());
    let setup: serde_json::Value = serde_json::from_slice(&setup.stdout).unwrap();
    assert_eq!(setup[0]["credentials"], "missing");
    assert_eq!(setup[0]["ready"], false);
}

#[test]
fn native_opencode_credentials_respect_selection_expiry_and_logout() {
    let sandbox = Sandbox::new();
    sandbox.install("opencode");
    std::fs::create_dir_all(sandbox.0.join("data/opencode")).unwrap();
    std::fs::write(
        sandbox.0.join("data/opencode/auth.json"),
        r#"{"opencode-go":{"type":"api","key":"obsolete-go-key"}}"#,
    )
    .unwrap();
    let db = rusqlite::Connection::open(sandbox.0.join("data/opencode/opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE credential (integration_id TEXT, value TEXT, active INTEGER, time_updated INTEGER);
         INSERT INTO credential VALUES ('opencode-go','{\"type\":\"key\",\"key\":\"current-go-key\"}',NULL,1);
         INSERT INTO credential VALUES ('opencode','{\"type\":\"oauth\",\"access\":\"expired-console\",\"expires\":1}',1,2);
         INSERT INTO credential VALUES ('opencode','{\"type\":\"oauth\",\"access\":\"inactive-console\",\"expires\":4102444800000}',0,3);",
    ).unwrap();
    for provider in ["opencode", "opencode-go"] {
        let ready = sandbox.run(&["providers", "-p", provider, "--json"]);
        assert!(ready.status.success());
        let ready: serde_json::Value = serde_json::from_slice(&ready.stdout).unwrap();
        assert_eq!(ready[0]["id"], provider);
        assert_eq!(ready[0]["ready"], true);
    }
    let expired = sandbox.run(&[
        "providers",
        "-p",
        "opencode-zen",
        "--only-unconfigured",
        "--json",
    ]);
    assert!(expired.status.success());
    let expired: serde_json::Value = serde_json::from_slice(&expired.stdout).unwrap();
    assert_eq!(expired[0]["credentials"], "expired");
    db.execute(
        "DELETE FROM credential WHERE integration_id='opencode-go'",
        [],
    )
    .unwrap();
    for provider in ["opencode", "opencode-go"] {
        let logged_out = sandbox.run(&["providers", "-p", provider, "--json"]);
        assert!(logged_out.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&logged_out.stdout).unwrap(),
            serde_json::json!([])
        );
    }
    db.execute_batch(
        "UPDATE credential SET active=0 WHERE integration_id='opencode';
         UPDATE credential SET active=1 WHERE value LIKE '%inactive-console%';",
    )
    .unwrap();
    for provider in ["opencode", "opencode-zen"] {
        let switched = sandbox.run(&["providers", "-p", provider, "--json"]);
        assert!(switched.status.success());
        let switched: serde_json::Value = serde_json::from_slice(&switched.stdout).unwrap();
        assert_eq!(switched[0]["ready"], true);
    }
}

#[test]
fn budgets_count_usage_even_when_the_agent_is_not_installed_or_authenticated() {
    let sandbox = Sandbox::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    std::fs::create_dir_all(sandbox.0.join(".fx")).unwrap();
    let event = serde_json::json!({
        "schema_version": 1, "kind": "generation",
        "fact": {"id": "retained-spend", "created_at_ms": now,
            "model": "claude-sonnet-4-5", "input_tokens": 100, "output_tokens": 20}
    });
    std::fs::write(sandbox.0.join(".fx/usage.jsonl"), format!("{event}\n")).unwrap();
    let check = sandbox.run(&[
        "check",
        "-p",
        "fx",
        "--budget-usd",
        "0.0001",
        "--period",
        "day",
        "--json",
    ]);
    assert_eq!(
        check.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
