//! Local history of usage snapshots (SQLite under XDG data).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::paths;
use crate::providers::types::ProviderSnapshot;
use crate::privacy;

#[derive(Debug, Clone)]
pub struct HistorySample {
    pub id: i64,
    pub recorded_at: f64,
    pub provider_id: String,
    pub provider_label: String,
    pub account: Option<String>,
    pub plan: Option<String>,
    pub meters_json: String,
}

#[derive(Debug, Clone)]
pub struct MeterPoint {
    pub recorded_at: f64,
    pub meter_id: String,
    pub meter_title: String,
    pub used_percent: f64,
}

pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    pub fn open() -> Result<Self> {
        let path = paths::history_db();
        paths::ensure_dir(&path)?;
        let store = Self { path };
        store.init()?;
        Ok(store)
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        paths::ensure_dir(&path)?;
        let store = Self { path };
        store.init()?;
        Ok(store)
    }

    fn conn(&self) -> Result<Connection> {
        Connection::open(&self.path)
            .with_context(|| format!("open history db {}", self.path.display()))
    }

    fn init(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                recorded_at REAL NOT NULL,
                provider_id TEXT NOT NULL,
                provider_label TEXT NOT NULL,
                account TEXT,
                plan TEXT,
                meters_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_provider_time
                ON snapshots(provider_id, recorded_at);
            "#,
        )?;
        Ok(())
    }

    pub fn record(&self, snap: &ProviderSnapshot) -> Result<()> {
        if snap.status != crate::providers::types::SnapshotStatus::Ok {
            return Ok(());
        }
        let meters = serde_json::to_string(&snap.meters).context("serialize meters")?;
        let conn = self.conn()?;
        conn.execute(
            r#"
            INSERT INTO snapshots (recorded_at, provider_id, provider_label, account, plan, meters_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                now_secs(),
                snap.id,
                snap.label,
                snap.account,
                snap.plan,
                meters,
            ],
        )?;
        Ok(())
    }

    pub fn record_many(&self, snaps: &[ProviderSnapshot]) -> Result<()> {
        for snap in snaps {
            self.record(snap)?;
        }
        Ok(())
    }

    pub fn recent(&self, limit: usize, provider: Option<&str>) -> Result<Vec<HistorySample>> {
        let conn = self.conn()?;
        let limit = limit.max(1) as i64;
        let mut out = Vec::new();
        if let Some(pid) = provider {
            let mut stmt = conn.prepare(
                r#"
                SELECT id, recorded_at, provider_id, provider_label, account, plan, meters_json
                FROM snapshots
                WHERE provider_id = ?1
                ORDER BY recorded_at DESC
                LIMIT ?2
                "#,
            )?;
            let rows = stmt.query_map(params![pid, limit], map_sample)?;
            for row in rows {
                out.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                r#"
                SELECT id, recorded_at, provider_id, provider_label, account, plan, meters_json
                FROM snapshots
                ORDER BY recorded_at DESC
                LIMIT ?1
                "#,
            )?;
            let rows = stmt.query_map(params![limit], map_sample)?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// Used-percent time series for a provider meter (oldest first).
    pub fn meter_series(
        &self,
        provider_id: &str,
        meter_id: &str,
        limit: usize,
    ) -> Result<Vec<(f64, f64)>> {
        let samples = self.recent(limit.max(2), Some(provider_id))?;
        let mut points = Vec::new();
        for sample in samples.into_iter().rev() {
            let meters: Vec<serde_json::Value> =
                serde_json::from_str(&sample.meters_json).unwrap_or_default();
            for m in meters {
                let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id != meter_id {
                    continue;
                }
                let used = m
                    .get("percent")
                    .and_then(|v| v.as_f64())
                    .or_else(|| {
                        m.get("left_percent")
                            .and_then(|v| v.as_f64())
                            .map(|lp| 1.0 - lp)
                    });
                if let Some(u) = used {
                    points.push((sample.recorded_at, u.clamp(0.0, 1.0)));
                }
            }
        }
        Ok(points)
    }

    /// All meter used% points for ETA / sparkline (recent N snapshots per provider).
    pub fn recent_meter_points(
        &self,
        provider_id: &str,
        limit: usize,
    ) -> Result<Vec<MeterPoint>> {
        let samples = self.recent(limit.max(2), Some(provider_id))?;
        let mut points = Vec::new();
        for sample in samples.into_iter().rev() {
            let meters: Vec<serde_json::Value> =
                serde_json::from_str(&sample.meters_json).unwrap_or_default();
            for m in meters {
                let meter_id = m
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = m
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or(meter_id.as_str())
                    .to_string();
                let used = m
                    .get("percent")
                    .and_then(|v| v.as_f64())
                    .or_else(|| {
                        m.get("left_percent")
                            .and_then(|v| v.as_f64())
                            .map(|lp| 1.0 - lp)
                    });
                if let Some(u) = used {
                    points.push(MeterPoint {
                        recorded_at: sample.recorded_at,
                        meter_id,
                        meter_title: title,
                        used_percent: u.clamp(0.0, 1.0),
                    });
                }
            }
        }
        Ok(points)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

fn map_sample(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistorySample> {
    Ok(HistorySample {
        id: row.get(0)?,
        recorded_at: row.get(1)?,
        provider_id: row.get(2)?,
        provider_label: row.get(3)?,
        account: row.get(4)?,
        plan: row.get(5)?,
        meters_json: row.get(6)?,
    })
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Compact sparkline from used-fraction samples (0..1), left→right oldest→newest.
pub fn sparkline(values: &[f64], width: usize) -> String {
    const BLOCKS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let width = width.max(1);
    let mut out = String::with_capacity(width);
    for i in 0..width {
        let idx = if values.len() == 1 {
            0
        } else {
            (i * (values.len() - 1)) / (width - 1).max(1)
        };
        let v = values.get(idx).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let bi = ((v * (BLOCKS.len() - 1) as f64).round() as usize).min(BLOCKS.len() - 1);
        out.push(BLOCKS[bi]);
    }
    out
}

pub fn format_history_line(sample: &HistorySample, privacy: bool) -> String {
    let age = {
        let delta = (now_secs() - sample.recorded_at).max(0.0);
        crate::ui::fmt_duration_secs(delta as u64)
    };
    let account = sample.account.as_deref().filter(|s| !s.is_empty()).map(|a| {
        if privacy {
            privacy::redact_account(a)
        } else {
            a.to_string()
        }
    });
    let meters: Vec<serde_json::Value> =
        serde_json::from_str(&sample.meters_json).unwrap_or_default();
    let mut parts = Vec::new();
    for m in meters.iter().take(4) {
        let title = m.get("title").and_then(|v| v.as_str()).unwrap_or("?");
        let pct = m
            .get("percent")
            .and_then(|v| v.as_f64())
            .map(|p| {
                let p = if p > 1.0 { p } else { p * 100.0 };
                format!("{p:.0}%")
            })
            .unwrap_or_else(|| "—".into());
        parts.push(format!("{title} {pct}"));
    }
    let mut line = format!(
        "{:<12} {ago:>6} ago",
        sample.provider_label,
        ago = age
    );
    if let Some(a) = account {
        line.push_str(&format!("  ·  {a}"));
    }
    if !parts.is_empty() {
        line.push_str("  ·  ");
        line.push_str(&parts.join(" · "));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderSnapshot, meter_from_used_percent};

    #[test]
    fn records_and_lists() {
        let path = std::env::temp_dir().join(format!(
            "usagenometer-hist-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = HistoryStore::open_at(path.clone()).unwrap();
        let mut snap = ProviderSnapshot::ok("codex", "Codex");
        snap.meters
            .push(meter_from_used_percent("5h", "5 hour", 40.0, None));
        store.record(&snap).unwrap();
        let recent = store.recent(10, Some("codex")).unwrap();
        assert_eq!(recent.len(), 1);
        let series = store.meter_series("codex", "5h", 10).unwrap();
        assert_eq!(series.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sparkline_nonempty() {
        let s = sparkline(&[0.1, 0.5, 0.9], 8);
        assert_eq!(s.chars().count(), 8);
    }
}
