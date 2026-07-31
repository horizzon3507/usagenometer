//! Diagnostic checks — auth paths, expiry, env. Never prints secrets.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::jwt::{jwt_exp, normalize_bearer};
use crate::paths;
use crate::privacy;
use crate::ui::{BRIGHT, DIM, GRAY, WHITE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Warn,
    Skip,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub status: CheckStatus,
    pub name: String,
    pub detail: String,
}

pub fn run(privacy: bool) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.extend(check_codex(privacy));
    checks.extend(check_cursor(privacy));
    checks.extend(check_claude());
    checks.extend(check_grok(privacy));
    checks.extend(check_antigravity());
    checks.extend(check_xdg());
    checks
}

pub fn print_report(checks: &[Check]) {
    println!();
    println!("  {} {}", "◈".with_style(), "doctor".with_style());
    println!();
    for c in checks {
        let mark = match c.status {
            CheckStatus::Pass => "✓",
            CheckStatus::Fail => "✗",
            CheckStatus::Warn => "!",
            CheckStatus::Skip => "·",
        };
        let color = match c.status {
            CheckStatus::Pass | CheckStatus::Fail => WHITE,
            CheckStatus::Warn => GRAY,
            CheckStatus::Skip => DIM,
        };
        use crossterm::style::Stylize;
        println!(
            "  {} {:<28} {}",
            mark.with(color),
            c.name.as_str().with(BRIGHT),
            c.detail.as_str().with(GRAY)
        );
    }
    println!();
    let fails = checks.iter().filter(|c| c.status == CheckStatus::Fail).count();
    let warns = checks.iter().filter(|c| c.status == CheckStatus::Warn).count();
    use crossterm::style::Stylize;
    println!(
        "  {} {} fail · {} warn · {} checks",
        "·".with(DIM),
        fails.to_string().with(GRAY),
        warns.to_string().with(GRAY),
        checks.len().to_string().with(GRAY)
    );
    println!();
}

trait StyleExt {
    fn with_style(&self) -> String;
}
impl StyleExt for str {
    fn with_style(&self) -> String {
        use crossterm::style::Stylize;
        format!("{}", self.with(BRIGHT).bold())
    }
}

fn check_xdg() -> Vec<Check> {
    vec![
        Check {
            status: CheckStatus::Pass,
            name: "config path".into(),
            detail: paths::display_path(&paths::config_file()),
        },
        Check {
            status: CheckStatus::Pass,
            name: "history db".into(),
            detail: paths::display_path(&paths::history_db()),
        },
        Check {
            status: CheckStatus::Pass,
            name: "cache dir".into(),
            detail: paths::display_path(&paths::cache_dir()),
        },
    ]
}

fn check_codex(privacy: bool) -> Vec<Check> {
    let path = codex_auth_path();
    let mut out = Vec::new();
    if !path.exists() {
        out.push(Check {
            status: CheckStatus::Fail,
            name: "codex auth".into(),
            detail: format!("missing {}", paths::display_path(&path)),
        });
        return out;
    }
    out.push(Check {
        status: CheckStatus::Pass,
        name: "codex auth file".into(),
        detail: paths::display_path(&path),
    });
    match fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => {
                let token = v
                    .pointer("/tokens/access_token")
                    .or_else(|| v.get("access_token"))
                    .and_then(|x| x.as_str())
                    .map(normalize_bearer);
                match token {
                    Some(t) if !t.is_empty() => {
                        out.push(token_expiry_check("codex token", &t));
                        if let Some(email) = v
                            .get("email")
                            .or_else(|| v.pointer("/account/email"))
                            .and_then(|x| x.as_str())
                        {
                            let shown = if privacy {
                                privacy::redact_account(email)
                            } else {
                                email.to_string()
                            };
                            out.push(Check {
                                status: CheckStatus::Pass,
                                name: "codex account".into(),
                                detail: shown,
                            });
                        }
                    }
                    _ => out.push(Check {
                        status: CheckStatus::Fail,
                        name: "codex token".into(),
                        detail: "no access token — run codex login".into(),
                    }),
                }
            }
            Err(_) => out.push(Check {
                status: CheckStatus::Fail,
                name: "codex auth json".into(),
                detail: "invalid JSON".into(),
            }),
        },
        Err(_) => out.push(Check {
            status: CheckStatus::Fail,
            name: "codex auth read".into(),
            detail: "unreadable".into(),
        }),
    }
    out
}

fn check_cursor(privacy: bool) -> Vec<Check> {
    let path = cursor_state_path();
    let mut out = Vec::new();
    if !path.exists() {
        out.push(Check {
            status: CheckStatus::Fail,
            name: "cursor state.vscdb".into(),
            detail: format!("missing {}", paths::display_path(&path)),
        });
        return out;
    }
    out.push(Check {
        status: CheckStatus::Pass,
        name: "cursor state.vscdb".into(),
        detail: paths::display_path(&path),
    });
    // Best-effort: open RO and look for token key
    let uri = format!("file:{}?mode=ro", path.display());
    match rusqlite::Connection::open(uri) {
        Ok(conn) => {
            let token: Option<String> = conn
                .query_row(
                    "SELECT value FROM ItemTable WHERE key = ?1",
                    ["cursorAuth/accessToken"],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            match token {
                Some(t) if !t.is_empty() => {
                    out.push(token_expiry_check("cursor token", &normalize_bearer(&t)));
                }
                _ => out.push(Check {
                    status: CheckStatus::Fail,
                    name: "cursor token".into(),
                    detail: "accessToken missing — sign in to Cursor".into(),
                }),
            }
            let email: Option<String> = conn
                .query_row(
                    "SELECT value FROM ItemTable WHERE key = ?1",
                    ["cursorAuth/cachedEmail"],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            if let Some(email) = email.filter(|s| !s.is_empty()) {
                let shown = if privacy {
                    privacy::redact_account(&email)
                } else {
                    email
                };
                out.push(Check {
                    status: CheckStatus::Pass,
                    name: "cursor account".into(),
                    detail: shown,
                });
            }
        }
        Err(_) => out.push(Check {
            status: CheckStatus::Warn,
            name: "cursor db open".into(),
            detail: "could not open state db (locked?)".into(),
        }),
    }
    out
}

fn check_claude() -> Vec<Check> {
    let path = claude_credentials_path();
    let mut out = Vec::new();
    if path.exists() {
        out.push(Check {
            status: CheckStatus::Pass,
            name: "claude credentials".into(),
            detail: paths::display_path(&path),
        });
        if let Ok(raw) = fs::read_to_string(&path)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
        {
            let oauth = v.get("claudeAiOauth").or_else(|| v.get("claude_ai_oauth"));
            if let Some(oauth) = oauth {
                if let Some(t) = oauth
                    .get("accessToken")
                    .or_else(|| oauth.get("access_token"))
                    .and_then(|x| x.as_str())
                {
                    out.push(token_expiry_check("claude token", &normalize_bearer(t)));
                }
                if let Some(exp_ms) = oauth.get("expiresAt").and_then(|x| x.as_f64()) {
                    let now_ms = now_secs() * 1000.0;
                    if exp_ms < now_ms {
                        out.push(Check {
                            status: CheckStatus::Fail,
                            name: "claude expiresAt".into(),
                            detail: "expired — run claude login".into(),
                        });
                    } else {
                        out.push(Check {
                            status: CheckStatus::Pass,
                            name: "claude expiresAt".into(),
                            detail: "not expired".into(),
                        });
                    }
                }
            } else {
                out.push(Check {
                    status: CheckStatus::Warn,
                    name: "claude oauth".into(),
                    detail: "file present but no claudeAiOauth object".into(),
                });
            }
        }
    } else {
        out.push(Check {
            status: CheckStatus::Warn,
            name: "claude credentials".into(),
            detail: format!(
                "missing {} (may use keyring / Antigravity fallback)",
                paths::display_path(&path)
            ),
        });
    }
    out
}

fn check_grok(privacy: bool) -> Vec<Check> {
    let path = grok_auth_path();
    let mut out = Vec::new();
    if !path.exists() {
        out.push(Check {
            status: CheckStatus::Fail,
            name: "grok auth".into(),
            detail: format!("missing {} — run grok login", paths::display_path(&path)),
        });
        return out;
    }
    out.push(Check {
        status: CheckStatus::Pass,
        name: "grok auth file".into(),
        detail: paths::display_path(&path),
    });
    if let Ok(raw) = fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        let token = v
            .get("access_token")
            .or_else(|| v.get("accessToken"))
            .or_else(|| v.pointer("/session/access_token"))
            .and_then(|x| x.as_str());
        match token {
            Some(t) if !t.is_empty() => {
                out.push(token_expiry_check("grok token", &normalize_bearer(t)));
            }
            _ => out.push(Check {
                status: CheckStatus::Fail,
                name: "grok token".into(),
                detail: "no session token".into(),
            }),
        }
        if let Some(email) = v.get("email").and_then(|x| x.as_str()) {
            let shown = if privacy {
                privacy::redact_account(email)
            } else {
                email.to_string()
            };
            out.push(Check {
                status: CheckStatus::Pass,
                name: "grok account".into(),
                detail: shown,
            });
        }
    }
    out
}

fn check_antigravity() -> Vec<Check> {
    let mut out = Vec::new();
    let oauth = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gemini")
        .join("oauth_creds.json");
    let accounts = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gemini")
        .join("google_accounts.json");

    if oauth.exists() {
        out.push(Check {
            status: CheckStatus::Pass,
            name: "antigravity oauth file".into(),
            detail: paths::display_path(&oauth),
        });
    } else {
        out.push(Check {
            status: CheckStatus::Warn,
            name: "antigravity oauth file".into(),
            detail: format!(
                "missing {} (may use secret store)",
                paths::display_path(&oauth)
            ),
        });
    }
    if accounts.exists() {
        out.push(Check {
            status: CheckStatus::Pass,
            name: "antigravity accounts".into(),
            detail: paths::display_path(&accounts),
        });
    } else {
        out.push(Check {
            status: CheckStatus::Skip,
            name: "antigravity accounts".into(),
            detail: "google_accounts.json not found".into(),
        });
    }

    let has_id = std::env::var_os("USAGENOMETER_GOOGLE_CLIENT_ID")
        .filter(|v| !v.is_empty())
        .is_some();
    let has_secret = std::env::var_os("USAGENOMETER_GOOGLE_CLIENT_SECRET")
        .filter(|v| !v.is_empty())
        .is_some();
    match (has_id, has_secret) {
        (true, true) => out.push(Check {
            status: CheckStatus::Pass,
            name: "antigravity oauth env".into(),
            detail: "USAGENOMETER_GOOGLE_CLIENT_ID/SECRET set".into(),
        }),
        _ => out.push(Check {
            status: CheckStatus::Warn,
            name: "antigravity oauth env".into(),
            detail: "CLIENT_ID/SECRET unset — refresh may fail".into(),
        }),
    }
    out
}

fn token_expiry_check(name: &str, token: &str) -> Check {
    match jwt_exp(token) {
        Some(exp) => {
            let now = now_secs();
            if exp < now + 30.0 {
                Check {
                    status: CheckStatus::Fail,
                    name: name.into(),
                    detail: "expired (or within 30s) — re-login".into(),
                }
            } else {
                let left = exp - now;
                Check {
                    status: CheckStatus::Pass,
                    name: name.into(),
                    detail: format!("valid · expires in {}", crate::ui::fmt_duration_secs(left as u64)),
                }
            }
        }
        None => Check {
            status: CheckStatus::Warn,
            name: name.into(),
            detail: "present (no JWT exp claim)".into(),
        },
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn codex_auth_path() -> PathBuf {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("auth.json");
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("auth.json")
}

fn cursor_state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

fn claude_credentials_path() -> PathBuf {
    if let Ok(custom) = std::env::var("CLAUDE_CREDENTIALS_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join(".credentials.json")
}

fn grok_auth_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
        .join("auth.json")
}
