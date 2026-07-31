//! XDG paths for config / data / cache. Tokens are never written here.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

const APP: &str = "usagenometer";

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join(APP)
}

pub fn data_dir() -> PathBuf {
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
    dirs::cache_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".cache")
        })
        .join(APP)
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
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
    let tail: PathBuf = comps[comps.len().saturating_sub(3)..]
        .iter()
        .collect();
    format!("…/{}", tail.display())
}
