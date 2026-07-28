//! Terminal UI — black & white, compact (optMusic-inspired).

use std::io::{self, IsTerminal, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::style::{Color, Stylize};

use crate::cli::DisplayMode;
use crate::providers::types::{ProviderSnapshot, SnapshotStatus, UsageMeter};

pub const WHITE: Color = Color::White;
pub const BRIGHT: Color = Color::Rgb {
    r: 245,
    g: 245,
    b: 245,
};
pub const GRAY: Color = Color::Rgb {
    r: 140,
    g: 140,
    b: 140,
};
pub const DIM: Color = Color::Rgb {
    r: 80,
    g: 80,
    b: 80,
};

pub const APP_NAME: &str = "usagenometer";

/// Binary name as invoked (`usagenometer` or `usg`).
pub fn bin_name() -> String {
    std::env::args()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "usagenometer".into())
}

pub fn banner() {
    println!();
    println!("  {} {}", "◈".with(BRIGHT), APP_NAME.with(BRIGHT).bold());
    println!();
}

pub fn print_info(msg: &str) {
    println!("  {} {}", "·".with(DIM), msg.with(GRAY));
}

pub fn print_success(msg: &str) {
    println!("  {} {}", "✓".with(BRIGHT), msg.with(BRIGHT));
}

pub fn print_warn(msg: &str) {
    println!("  {} {}", "!".with(GRAY), msg.with(GRAY));
}

pub fn print_error(msg: &str) {
    eprintln!("  {} {}", "✗".with(WHITE), msg.with(WHITE));
}

/// Render provider snapshots as a compact B&W panel.
pub fn print_status(snapshots: &[ProviderSnapshot], display: DisplayMode) {
    let color = io::stdout().is_terminal();
    for (i, snap) in snapshots.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_provider(snap, display, color);
    }
    if !snapshots.is_empty() {
        println!();
    }
}

fn print_provider(snap: &ProviderSnapshot, display: DisplayMode, color: bool) {
    let title = format_provider_header(snap);
    if color {
        println!("  {}", title.with(BRIGHT).bold());
    } else {
        println!("  {title}");
    }

    match snap.status {
        SnapshotStatus::Ok if !snap.meters.is_empty() => {
            for meter in &snap.meters {
                print_meter(meter, display, color);
            }
        }
        SnapshotStatus::Ok => {
            let note = snap
                .error
                .as_deref()
                .unwrap_or("no meters");
            if color {
                println!("    {}", note.with(GRAY));
            } else {
                println!("    {note}");
            }
        }
        SnapshotStatus::Auth | SnapshotStatus::Error | SnapshotStatus::Disabled => {
            let note = snap.error.as_deref().unwrap_or(snap.status.as_str());
            if color {
                println!("    {} {}", snap.status.as_str().with(DIM), note.with(GRAY));
            } else {
                println!("    {} {note}", snap.status.as_str());
            }
        }
    }
}

fn format_provider_header(snap: &ProviderSnapshot) -> String {
    let mut parts = vec![snap.label.clone()];
    if let Some(account) = snap.account.as_deref().filter(|s| !s.is_empty()) {
        parts.push(account.to_string());
    }
    if let Some(plan) = snap.plan.as_deref().filter(|s| !s.is_empty()) {
        parts.push(plan.to_string());
    }
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        format!("{}  ·  {}", parts[0], parts[1..].join(" · "))
    }
}

fn print_meter(meter: &UsageMeter, display: DisplayMode, color: bool) {
    let fraction = match display {
        DisplayMode::Left => meter.left_percent.or_else(|| meter.percent.map(|p| 1.0 - p)),
        DisplayMode::Used => meter.percent.or_else(|| meter.left_percent.map(|p| 1.0 - p)),
    };
    let bar = meter_bar(fraction.unwrap_or(0.0), 18);
    let pct = fraction
        .map(|f| format!("{:>3.0}%", f * 100.0))
        .unwrap_or_else(|| "  —".into());
    let reset = format_reset(meter);
    let unit = match meter.unit.as_str() {
        "usd" => {
            let used = meter.used.map(|u| format!("${u:.2}")).unwrap_or_else(|| "?".into());
            let limit = meter
                .limit
                .map(|l| format!("${l:.2}"))
                .unwrap_or_else(|| "∞".into());
            format!("  {used}/{limit}")
        }
        "credits" | "tokens" | "requests" => {
            if fraction.is_none() {
                match (meter.used, meter.limit) {
                    (Some(u), Some(l)) => format!("  {u:.0}/{l:.0}"),
                    (Some(u), None) => format!("  {u:.0} used"),
                    _ => String::new(),
                }
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };

    let line = format!(
        "    {:<18} {} {}{}{}",
        truncate(&meter.title, 18),
        bar,
        pct,
        unit,
        reset
    );
    if color {
        println!("{}", line.with(GRAY));
    } else {
        println!("{line}");
    }
}

fn meter_bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    format!("{}{}", "━".repeat(filled), "─".repeat(empty))
}

fn format_reset(meter: &UsageMeter) -> String {
    if let Some(reset_at) = meter.reset_at {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let delta = reset_at - now;
        if delta > 0.0 {
            return format!("  ·  reset {}", fmt_duration(Duration::from_secs_f64(delta)));
        }
    }
    if let Some(after) = meter.reset_after_seconds.filter(|s| *s > 0.0) {
        return format!("  ·  reset {}", fmt_duration(Duration::from_secs_f64(after)));
    }
    String::new()
}

fn fmt_duration(d: Duration) -> String {
    let total = d.as_secs();
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let mins = (total % 3600) / 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else {
        format!("{mins}m")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn flush_stdout() {
    let _ = io::stdout().flush();
}
