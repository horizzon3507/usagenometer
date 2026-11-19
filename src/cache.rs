//! Short TTL cache of successful ProviderSnapshots (XDG cache).

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::providers::types::{ProviderSnapshot, SnapshotStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    saved_at: f64,
    snapshot: ProviderSnapshot,
}

pub struct SnapshotCache {
    dir: PathBuf,
    ttl_secs: u64,
}

impl SnapshotCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self::with_profile(ttl_secs, None)
    }

    pub fn with_profile(ttl_secs: u64, profile: Option<&str>) -> Self {
        let base = paths::cache_dir().join("snapshots");
        let dir = match profile.map(str::trim).filter(|s| !s.is_empty()) {
            Some(p) => {
                let safe: String = p
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                base.join(format!("profile-{safe}"))
            }
            None => base,
        };
        Self { dir, ttl_secs }
    }

    pub fn save(&self, snap: &ProviderSnapshot) -> Result<()> {
        if snap.status != SnapshotStatus::Ok {
            return Ok(());
        }
        paths::ensure_dir(&self.dir.join("x"))?;
        let entry = CacheEntry {
            saved_at: now_secs(),
            snapshot: snap.clone(),
        };
        let path = self.path_for(&snap.id);
        let raw = serde_json::to_string(&entry).context("serialize cache entry")?;
        let tmp = self
            .dir
            .join(format!(".{}.json.tmp.{}", snap.id, std::process::id()));
        if fs::write(&tmp, &raw).is_ok() {
            if fs::rename(&tmp, &path).is_err() {
                let _ = fs::remove_file(&tmp);
                fs::write(&path, &raw)
                    .with_context(|| format!("write cache {}", path.display()))?;
            }
        } else {
            fs::write(&path, &raw).with_context(|| format!("write cache {}", path.display()))?;
        }
        Ok(())
    }

    pub fn invalidate(&self, provider_id: &str) -> Result<()> {
        match fs::remove_file(self.path_for(provider_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("invalidate rejected authentication snapshot"),
        }
    }

    fn load_entry(&self, provider_id: &str) -> Option<(CacheEntry, u64)> {
        let path = self.path_for(provider_id);
        let raw = fs::read_to_string(path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&raw).ok()?;
        let age = (now_secs() - entry.saved_at).max(0.0) as u64;
        Some((entry, age))
    }

    /// Load a cached snapshot if still within TTL for “fresh”, or any age for stale fallback.
    /// Returns `(snapshot, age_secs)`.
    pub fn load(&self, provider_id: &str) -> Option<(ProviderSnapshot, u64)> {
        let (entry, age) = self.load_entry(provider_id)?;
        let mut snap = entry.snapshot;
        snap.stale_age_secs = Some(age);
        Some((snap, age))
    }

    pub fn load_fresh(&self, provider_id: &str) -> Option<ProviderSnapshot> {
        let (entry, age) = self.load_entry(provider_id)?;
        if age > self.ttl_secs {
            return None;
        }
        let now = now_secs();
        let reset_crossed = entry.snapshot.meters.iter().any(|m| {
            m.reset_at
                .is_some_and(|r| r > 0.0 && entry.saved_at < r && now >= r)
        });
        if reset_crossed {
            return None;
        }
        let mut s = entry.snapshot;
        s.stale_age_secs = None;
        Some(s)
    }

    /// Stale fallback when live fetch fails (any age, marked stale).
    pub fn load_stale(&self, provider_id: &str) -> Option<ProviderSnapshot> {
        let (mut snap, age) = self.load(provider_id)?;
        snap.stale_age_secs = Some(age);
        // Keep original error context? Prefer showing cached meters with stale marker.
        // Status stays Ok so meters render; stale_age_secs flags UI.
        snap.status = SnapshotStatus::Ok;
        Some(snap)
    }

    fn path_for(&self, provider_id: &str) -> PathBuf {
        self.dir.join(format!("{provider_id}.json"))
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::ProviderSnapshot;

    #[test]
    fn roundtrip_cache() {
        let dir =
            std::env::temp_dir().join(format!("usagenometer-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cache = SnapshotCache {
            dir: dir.clone(),
            ttl_secs: 60,
        };
        let snap = ProviderSnapshot::ok("codex", "Codex");
        cache.save(&snap).unwrap();
        let loaded = cache.load_fresh("codex").unwrap();
        assert_eq!(loaded.id, "codex");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_fresh_invalidates_after_meter_reset_at() {
        let dir = std::env::temp_dir().join(format!(
            "usagenometer-cache-reset-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cache = SnapshotCache {
            dir: dir.clone(),
            ttl_secs: 600,
        };
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        let now = now_secs();
        snap.meters
            .push(crate::providers::types::meter_from_used_percent(
                "5h",
                "5 hour",
                90.0,
                Some(now - 5.0),
            ));
        let entry = CacheEntry {
            saved_at: now - 30.0,
            snapshot: snap,
        };
        fs::write(
            cache.path_for("codex"),
            serde_json::to_string(&entry).unwrap(),
        )
        .unwrap();
        // Saved before reset_at, now is past reset_at -> stale even inside 600s TTL.
        assert!(cache.load_fresh("codex").is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
