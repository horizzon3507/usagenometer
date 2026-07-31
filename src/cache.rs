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
        Self {
            dir: paths::cache_dir().join("snapshots"),
            ttl_secs,
        }
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
        fs::write(&path, raw).with_context(|| format!("write cache {}", path.display()))?;
        Ok(())
    }

    /// Load a cached snapshot if still within TTL for “fresh”, or any age for stale fallback.
    /// Returns `(snapshot, age_secs)`.
    pub fn load(&self, provider_id: &str) -> Option<(ProviderSnapshot, u64)> {
        let path = self.path_for(provider_id);
        let raw = fs::read_to_string(path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&raw).ok()?;
        let age = (now_secs() - entry.saved_at).max(0.0) as u64;
        let mut snap = entry.snapshot;
        snap.stale_age_secs = Some(age);
        Some((snap, age))
    }

    pub fn load_fresh(&self, provider_id: &str) -> Option<ProviderSnapshot> {
        let (snap, age) = self.load(provider_id)?;
        if age <= self.ttl_secs {
            let mut s = snap;
            s.stale_age_secs = None;
            Some(s)
        } else {
            None
        }
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
        let dir = std::env::temp_dir().join(format!(
            "usagenometer-cache-test-{}",
            std::process::id()
        ));
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
}
