use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::Connection;

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "usagenometer-ledger-cli-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn scan(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_usg"))
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("claude"))
            .args([
                "tokens", "--all", "-p", "claude", "-p", "fx", "--period", "all", "--json",
            ])
            .output()
            .unwrap()
    }

    fn scan_commandcode(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_usg"))
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .args([
                "tokens",
                "--all",
                "-p",
                "commandcode",
                "--period",
                "all",
                "--json",
            ])
            .output()
            .unwrap()
    }

    fn write_logs(&self) {
        let claude = self.0.join("claude/projects/demo/session.jsonl");
        std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
        let record = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-10-01T12:00:00Z",
            "sessionId": "atomic-session",
            "cwd": "/atomic/project",
            "message": {"model": "claude-sonnet-4-5", "usage": {
                "input_tokens": 100, "output_tokens": 20,
                "cache_read_input_tokens": 10, "cache_creation_input_tokens": 2,
            }},
        });
        std::fs::write(claude, format!("{record}\n")).unwrap();
        let fx = self.0.join(".fx/usage.jsonl");
        std::fs::create_dir_all(fx.parent().unwrap()).unwrap();
        let record = serde_json::json!({
            "schema_version": 1,
            "kind": "generation",
            "fact": {"id": "atomic-fx", "created_at_ms": 1790856000000_u64,
                "model": "openai/gpt-5.5", "input_tokens": 200,
                "output_tokens": 40, "cache_read_tokens": 30, "cache_write_tokens": 4},
        });
        std::fs::write(fx, format!("{record}\n")).unwrap();
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_failed_scan_can_recover(trigger: &str) {
    let sandbox = Sandbox::new();
    let initial = sandbox.scan();
    assert_eq!(
        initial.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let db = Connection::open(sandbox.0.join("data/usagenometer/history.sqlite3")).unwrap();
    let last_scan: String = db
        .query_row(
            "SELECT value FROM token_scan_meta WHERE key = 'last_scan_unix'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    db.execute_batch(trigger).unwrap();
    sandbox.write_logs();

    let failed = sandbox.scan();
    assert_eq!(
        failed.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&failed.stdout)
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM token_events", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM token_scan_offsets", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT value FROM token_scan_meta WHERE key = 'last_scan_unix'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        last_scan
    );

    db.execute_batch("DROP TRIGGER fail_scan").unwrap();
    let retry = sandbox.scan();
    assert_eq!(
        retry.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(body["scanned_new_events"], 2);
    assert_eq!(body["totals"]["total_tokens"], 372);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM token_events", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM token_scan_offsets", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        2
    );

    let repeat = sandbox.scan();
    assert_eq!(repeat.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&repeat.stdout).unwrap();
    assert_eq!(body["scanned_new_events"], 0);
    assert_eq!(body["totals"]["total_tokens"], 372);
}

#[test]
fn commandcode_transcripts_are_scanned_by_the_selected_provider() {
    let sandbox = Sandbox::new();
    let path = sandbox
        .0
        .join(".commandcode/projects/home-u-demo/sess-1.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lines = [
        r#"{"type":"session","version":3,"id":"sess-1","timestamp":"2026-10-01T12:00:00Z","cwd":"/home/u/demo"}"#,
        r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-10-01T12:00:05Z","message":{"role":"assistant","content":[]},"usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":10,"cacheWriteTokens":2},"model":"deepseek/deepseek-v4-flash"}"#,
    ];
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let output = sandbox.scan_commandcode();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["scanned_new_events"], 1);
    assert_eq!(body["totals"]["input_tokens"], 100);
    assert_eq!(body["totals"]["output_tokens"], 20);
    assert_eq!(body["totals"]["cache_read_tokens"], 10);
    assert_eq!(body["totals"]["cache_write_tokens"], 2);
    assert_eq!(body["groups"][0]["provider"], "commandcode");

    let repeat = sandbox.scan_commandcode();
    assert_eq!(repeat.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&repeat.stdout).unwrap();
    assert_eq!(body["scanned_new_events"], 0);
    assert_eq!(body["totals"]["input_tokens"], 100);
}

#[test]
fn event_insert_failure_preserves_both_collectors_for_retry() {
    assert_failed_scan_can_recover(
        "CREATE TRIGGER fail_scan BEFORE INSERT ON token_events
         BEGIN SELECT RAISE(ABORT, 'event insert failed'); END;",
    );
}

#[test]
fn checkpoint_failure_does_not_commit_events_or_scan_completion() {
    assert_failed_scan_can_recover(
        "CREATE TRIGGER fail_scan BEFORE INSERT ON token_scan_offsets
         BEGIN SELECT RAISE(ABORT, 'checkpoint write failed'); END;",
    );
}

#[test]
fn scan_completion_failure_rolls_back_events_and_checkpoints() {
    assert_failed_scan_can_recover(
        "CREATE TRIGGER fail_scan BEFORE UPDATE ON token_scan_meta
         BEGIN SELECT RAISE(ABORT, 'scan metadata write failed'); END;",
    );
}
