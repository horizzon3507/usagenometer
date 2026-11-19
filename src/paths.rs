//! XDG paths for config / data / cache. Tokens are never written here.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

const APP: &str = "usagenometer";

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    if let Some(dir) = env_dir("USG_CONFIG_DIR") {
        return dir;
    }
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join(APP)
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = env_dir("USG_DATA_DIR") {
        return dir;
    }
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join(APP)
}

pub fn cache_dir() -> PathBuf {
    if let Some(dir) = env_dir("USG_CACHE_DIR") {
        return dir;
    }
    dirs::cache_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".cache")
        })
        .join(APP)
}

pub fn config_file() -> PathBuf {
    if let Some(path) = env_dir("USG_CONFIG") {
        return path;
    }
    let primary = config_dir().join("config.toml");
    if primary.exists() {
        return primary;
    }
    if let Some(home) = dirs::home_dir() {
        let dot_config = home.join(".config").join(APP).join("config.toml");
        if dot_config.exists() {
            return dot_config;
        }
    }
    primary
}

/// Walk upward from `cwd` looking for `.usg.toml` (stops at `$HOME` or `/`).
pub fn project_config_file() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let home = dirs::home_dir();
    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join(".usg.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if home.as_deref() == Some(dir) {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    None
}

pub fn history_db() -> PathBuf {
    data_dir().join("history.sqlite3")
}

pub fn ensure_dir(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    } else {
        fs::create_dir_all(path).with_context(|| format!("create dir {}", path.display()))?;
    }
    Ok(())
}

pub fn ensure_app_dirs() -> Result<()> {
    fs::create_dir_all(config_dir()).context("create config dir")?;
    fs::create_dir_all(data_dir()).context("create data dir")?;
    fs::create_dir_all(cache_dir()).context("create cache dir")?;
    Ok(())
}

/// Truncate a path for display (no secrets). Keep last few components.
pub fn display_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    if s.len() <= 64 {
        return s;
    }
    let comps: Vec<_> = path.components().collect();
    if comps.len() <= 3 {
        return format!("…{s}");
    }
    let tail: PathBuf = comps[comps.len().saturating_sub(3)..].iter().collect();
    format!("…/{}", tail.display())
}
