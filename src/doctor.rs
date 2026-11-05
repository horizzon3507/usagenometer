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
    checks.push(auth_check(
        "cursor auth",
        crate::providers::cursor::auth_description(),
    ));
    checks.extend(check_claude());
    checks.push(auth_check(
        "grok auth",
        crate::providers::grok::auth_description(),
    ));
    checks.extend(check_devin_cloud());
    checks.extend(check_fx());
    checks.extend(check_commandcode());
    checks.push(auth_check(
        "antigravity auth",
        crate::providers::antigravity::auth_description(),
    ));
    checks.push(auth_check(
        "opencode-go auth",
        crate::providers::opencode::auth_description_go(),
    ));
    checks.push(auth_check(
        "opencode-zen auth",
        crate::providers::opencode::auth_description_zen(),
    ));
    checks.extend(check_xdg());
    checks.extend(check_token_ledger());
    checks
}

fn auth_check(name: &str, result: Result<String, String>) -> Check {
    let (status, detail) = match result {
        Ok(detail) => (CheckStatus::Pass, detail),
        Err(detail) => (CheckStatus::Fail, detail),
    };
    Check {
        status,
        name: name.into(),
        detail,
    }
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
    let fails = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();
    let warns = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
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
    let mut out = vec![
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
    ];
    out.push(match crate::tokens::TokenStore::open() {
        Ok(store) => Check {
            status: CheckStatus::Pass,
            name: "token ledger".into(),
            detail: format!("{} events", store.event_count().unwrap_or(0)),
        },
        Err(_) => Check {
            status: CheckStatus::Warn,
            name: "token ledger".into(),
            detail: "token_events table unavailable".into(),
        },
    });
    out
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

fn check_devin_cloud() -> Vec<Check> {
    let mut out = Vec::new();
    let env_key = std::env::var("DEVIN_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let path = crate::providers::devin_cloud::credentials_path();
    if env_key.is_some() {
        out.push(Check {
            status: CheckStatus::Pass,
            name: "devin api key".into(),
            detail: "DEVIN_API_KEY set".into(),
        });
    } else if path.exists() {
        out.push(Check {
            status: CheckStatus::Pass,
            name: "devin credentials".into(),
            detail: paths::display_path(&path),
        });
    } else {
        out.push(Check {
            status: CheckStatus::Warn,
            name: "devin api key".into(),
            detail: format!(
                "DEVIN_API_KEY unset and no {} — run devin auth login",
                paths::display_path(&path)
            ),
        });
    }
    out
}

/// fx credential presence in fx's own precedence order. Never reads tokens
/// beyond the login expiry field.
fn check_fx() -> Vec<Check> {
    let env = ["VERCEL_OIDC_TOKEN", "AI_GATEWAY_API_KEY"]
        .into_iter()
        .find(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()));
    let home = crate::providers::fx::fx_home();
    let auth = home.join("auth.json");
    let key = home.join("api-key");
    let check = if let Some(var) = env {
        Check {
            status: CheckStatus::Pass,
            name: "fx credential".into(),
            detail: format!("{var} set"),
        }
    } else if auth.exists() {
        let expires_ms = fs::read_to_string(&auth)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("expires_at_ms").and_then(|x| x.as_f64()));
        match expires_ms {
            Some(ms) if ms / 1000.0 <= now_secs() => Check {
                status: CheckStatus::Warn,
                name: "fx login".into(),
                detail: format!(
                    "{} expired — run any fx command to refresh",
                    paths::display_path(&auth)
                ),
            },
            _ => Check {
                status: CheckStatus::Pass,
                name: "fx login".into(),
                detail: paths::display_path(&auth),
            },
        }
    } else if key.exists() {
        Check {
            status: CheckStatus::Pass,
            name: "fx api key".into(),
            detail: paths::display_path(&key),
        }
    } else {
        Check {
            status: CheckStatus::Skip,
            name: "fx credential".into(),
            detail: "no AI Gateway credential — fx login / fx setup".into(),
        }
    };
    vec![check]
}

fn check_commandcode() -> Vec<Check> {
    let env_key = std::env::var("COMMANDCODE_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".commandcode")
        .join("auth.json");
    let check = if env_key.is_some() {
        Check {
            status: CheckStatus::Pass,
            name: "commandcode api key".into(),
            detail: "COMMANDCODE_API_KEY set".into(),
        }
    } else if !path.exists() {
        Check {
            status: CheckStatus::Skip,
            name: "commandcode auth".into(),
            detail: "no local login — run commandcode login".into(),
        }
    } else {
        match fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|value| {
                value
                    .get("apiKey")
                    .or_else(|| value.get("api_key"))
                    .and_then(|key| key.as_str())
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .map(str::to_string)
            }) {
            Some(_) => Check {
                status: CheckStatus::Pass,
                name: "commandcode auth".into(),
                detail: paths::display_path(&path),
            },
            None => Check {
                status: CheckStatus::Warn,
                name: "commandcode auth".into(),
                detail: format!(
                    "{} has no apiKey — run commandcode login",
                    paths::display_path(&path)
                ),
            },
        }
    };
    vec![check]
}

/// Token-ledger section: per-agent scan roots found/missing, stored event
/// counts, and the ledger db itself (path, events, last scan age).
fn check_token_ledger() -> Vec<Check> {
    use crate::tokens::{TokenStore, collect};

    let mut out = Vec::new();
    let store = TokenStore::open().ok();
    let counts = store
        .and_then(|s| s.provider_event_counts().ok())
        .unwrap_or_default();
    let count_of = |provider: &str| -> u64 {
        counts
            .iter()
            .find(|(p, _)| p == provider)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };

    for provider in collect::known_providers() {
        let roots = collect::scan_roots(provider);
        let found: Vec<PathBuf> = roots.iter().filter(|p| p.exists()).cloned().collect();
        let events = count_of(provider);
        if found.is_empty() {
            out.push(Check {
                status: CheckStatus::Skip,
                name: format!("ledger {provider}"),
                detail: format!("no scan roots ({} missing)", roots.len()),
            });
        } else {
            out.push(Check {
                status: CheckStatus::Pass,
                name: format!("ledger {provider}"),
                detail: format!(
                    "{} root(s) · {} events · {}",
                    found.len(),
                    events,
                    paths::display_path(&found[0])
                ),
            });
        }
    }

    let db = paths::history_db();
    match TokenStore::open() {
        Ok(store) => {
            let total: u64 = store
                .provider_event_counts()
                .map(|cs| cs.iter().map(|(_, n)| *n).sum())
                .unwrap_or(0);
            let age = store
                .last_scan_unix()
                .ok()
                .flatten()
                .map(|last| {
                    let secs = (now_secs() - last).max(0.0) as u64;
                    format!(" · last scan {} ago", crate::ui::fmt_duration_secs(secs))
                })
                .unwrap_or_else(|| " · never scanned".to_string());
            out.push(Check {
                status: CheckStatus::Pass,
                name: "token ledger db".into(),
                detail: format!("{} · {} events{}", paths::display_path(&db), total, age),
            });
        }
        Err(_) => out.push(Check {
            status: CheckStatus::Warn,
            name: "token ledger db".into(),
            detail: format!("could not open {}", paths::display_path(&db)),
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
                    detail: format!(
                        "valid · expires in {}",
                        crate::ui::fmt_duration_secs(left as u64)
                    ),
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
