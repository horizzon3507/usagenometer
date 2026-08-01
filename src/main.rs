//! usagenometer — AI usage meters in the terminal.
//!
//! Binaries: `usagenometer` and short alias `usg`.

use std::io;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use crossterm::style::Stylize;
use crossterm::terminal::{Clear, ClearType};
use crossterm::{ExecutableCommand, cursor};

use usagenometer::alerts::{self, AlertTracker};
use usagenometer::cli::{Cli, Command, OutputFormat, ProviderArg, ShellArg};
use usagenometer::config::{ConfigFile, Settings};
use usagenometer::doctor;
use usagenometer::eta;
use usagenometer::explain;
use usagenometer::export;
use usagenometer::history::{self, HistoryStore};
use usagenometer::paths;
use usagenometer::privacy;
use usagenometer::providers::{self, resolve_providers};
use usagenometer::routing;
use usagenometer::ui::{
    StatusOptions, WHITE, banner, bin_name, flush_stdout, print_compact, print_diff, print_error,
    print_info, print_status_opts, print_success, print_warn,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("{} {err}", "error:".with(WHITE));
        for cause in err.chain().skip(1) {
            eprintln!("  {} {cause}", "↳".with(usagenometer::ui::DIM));
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let settings = build_settings(&cli);
    let _bin = bin_name();

    match cli.command {
        None => {
            cmd_status(&settings, settings.compact)?;
        }
        Some(Command::Status { compact }) => {
            cmd_status(&settings, compact || settings.compact)?;
        }
        Some(Command::Watch {
            interval,
            alert,
            diff,
        }) => {
            let mut s = settings.clone();
            if let Some(a) = alert {
                s.alert = Some(a);
            }
            let interval = interval.unwrap_or(s.watch_interval).max(5);
            cmd_watch(&s, interval, diff)?;
        }
        Some(Command::Test { provider }) => {
            cmd_test(&settings, provider)?;
        }
        Some(Command::Providers) => {
            cmd_providers(settings.quiet);
        }
        Some(Command::Json) => {
            cmd_json(&settings)?;
        }
        Some(Command::Check { fail_under }) => {
            cmd_check(&settings, fail_under)?;
        }
        Some(Command::Doctor) => {
            let checks = doctor::run(settings.privacy);
            doctor::print_report(&checks);
            if checks.iter().any(|c| c.status == doctor::CheckStatus::Fail) {
                std::process::exit(1);
            }
        }
        Some(Command::Explain { provider }) => {
            print!("{}", explain::explain(provider));
        }
        Some(Command::History {
            limit,
            provider,
            spark,
        }) => {
            cmd_history(&settings, limit, provider, spark)?;
        }
        Some(Command::Config { dump }) => {
            cmd_config(&settings, dump);
        }
        Some(Command::Tui) => {
            usagenometer::tui::run(&settings)?;
        }
        Some(Command::Completions { shell }) => {
            cmd_completions(shell);
        }
        Some(Command::Version) => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}

fn build_settings(cli: &Cli) -> Settings {
    let config = ConfigFile::load();
    let providers = if !cli.providers.is_empty() {
        cli.providers.clone()
    } else {
        let from_cfg = config.parse_providers();
        if from_cfg.is_empty() {
            vec![]
        } else {
            from_cfg
        }
    };
    let display = cli.display.unwrap_or_else(|| config.display_mode());
    let compact = cli.compact || config.compact;
    let privacy = cli.privacy || config.privacy;
    let notify = cli.notify || config.notify;
    let alert = cli.alert.or(config.alert);
    let alert_eta = cli.alert_eta.or(config.alert_eta);
    let json = cli.json || matches!(cli.format, Some(OutputFormat::Json));
    Settings {
        providers,
        display,
        quiet: cli.quiet,
        compact,
        privacy,
        json,
        pretty: cli.pretty,
        format: cli.format,
        alert,
        alert_eta,
        notify,
        cache_ttl: config.cache_ttl_secs(),
        history: config.history,
        watch_interval: config.watch_interval_secs(),
        config,
    }
}

fn fetch(settings: &Settings) -> Vec<usagenometer::providers::types::ProviderSnapshot> {
    let mut snaps =
        providers::fetch_all_cached(&settings.providers, settings.cache_ttl, settings.history);
    if settings.privacy {
        privacy::redact_snapshots(&mut snaps);
    }
    // Apply provider_order from config when no CLI filter.
    if settings.providers.is_empty() && !settings.config.provider_order.is_empty() {
        snaps = order_snapshots(snaps, &settings.config.provider_order);
    }
    snaps
}

fn order_snapshots(
    mut snaps: Vec<usagenometer::providers::types::ProviderSnapshot>,
    order: &[String],
) -> Vec<usagenometer::providers::types::ProviderSnapshot> {
    let mut ordered = Vec::new();
    for name in order {
        if let Some(pos) = snaps.iter().position(|s| s.id == name.as_str()) {
            ordered.push(snaps.remove(pos));
        }
    }
    ordered.append(&mut snaps);
    ordered
}

fn cmd_status(settings: &Settings, compact: bool) -> Result<()> {
    let snaps = fetch(settings);

    if settings.json || matches!(settings.format, Some(OutputFormat::Json)) {
        return emit_json(&snaps, settings.pretty);
    }
    if matches!(settings.format, Some(OutputFormat::Prometheus)) {
        return export::emit_prometheus(&snaps);
    }

    if compact {
        print_compact(&snaps, settings.display);
        return Ok(());
    }

    if !settings.quiet {
        banner();
    }

    let history = HistoryStore::open().ok();
    let etas = history
        .as_ref()
        .map(|h| eta::eta_map_from_history(h, &snaps));
    let eta_secs = history
        .as_ref()
        .map(|h| eta::eta_seconds_from_history(h, &snaps));
    print_status_opts(
        &snaps,
        &StatusOptions {
            display: settings.display,
            privacy: settings.privacy,
            etas: etas.as_ref(),
        },
    );

    let events = alerts::evaluate(&snaps, settings, eta_secs.as_ref());
    alerts::print_alerts(&events, settings.quiet);
    alerts::maybe_notify(&events, settings.notify);

    if !settings.quiet
        && !compact
        && let Some(hint) = routing::compute(&snaps)
    {
        print_info(&hint.message);
        println!();
    }
    Ok(())
}

fn cmd_watch(settings: &Settings, interval: u64, diff: bool) -> Result<()> {
    if settings.json || matches!(settings.format, Some(OutputFormat::Json)) {
        loop {
            let snaps = fetch(settings);
            emit_json(&snaps, settings.pretty)?;
            thread::sleep(Duration::from_secs(interval));
        }
    }
    if matches!(settings.format, Some(OutputFormat::Prometheus)) {
        loop {
            let snaps = fetch(settings);
            export::emit_prometheus(&snaps)?;
            thread::sleep(Duration::from_secs(interval));
        }
    }

    let mut prev: Option<Vec<_>> = None;
    let mut tracker = AlertTracker::new();

    loop {
        let snaps = fetch(settings);

        if diff {
            if let Some(ref p) = prev {
                let mut stdout = io::stdout();
                let _ = stdout.execute(cursor::MoveTo(0, 0));
                let _ = stdout.execute(Clear(ClearType::All));
                if !settings.quiet {
                    banner();
                    print_info(&format!(
                        "watch diff · every {interval}s · Ctrl-C to quit"
                    ));
                    println!();
                }
                print_diff(p, &snaps, settings.display);
            } else if !settings.quiet {
                banner();
                print_info("watch diff · collecting baseline…");
                println!();
                print_status_opts(
                    &snaps,
                    &StatusOptions {
                        display: settings.display,
                        privacy: settings.privacy,
                        etas: None,
                    },
                );
            }
        } else {
            let mut stdout = io::stdout();
            let _ = stdout.execute(cursor::MoveTo(0, 0));
            let _ = stdout.execute(Clear(ClearType::All));
            if settings.compact {
                print_compact(&snaps, settings.display);
            } else {
                if !settings.quiet {
                    banner();
                    print_info(&format!("refresh every {interval}s · Ctrl-C to quit"));
                    println!();
                }
                let etas = HistoryStore::open()
                    .ok()
                    .map(|h| eta::eta_map_from_history(&h, &snaps));
                print_status_opts(
                    &snaps,
                    &StatusOptions {
                        display: settings.display,
                        privacy: settings.privacy,
                        etas: etas.as_ref(),
                    },
                );
            }
        }

        let eta_secs = HistoryStore::open()
            .ok()
            .map(|h| eta::eta_seconds_from_history(&h, &snaps));
        let events = tracker.filter_new(alerts::evaluate(&snaps, settings, eta_secs.as_ref()));
        alerts::print_alerts(&events, settings.quiet);
        alerts::maybe_notify(&events, settings.notify);

        if !settings.quiet && !settings.compact && !diff
            && let Some(hint) = routing::compute(&snaps)
        {
            print_info(&hint.message);
        }

        prev = Some(snaps);
        flush_stdout();
        thread::sleep(Duration::from_secs(interval));
    }
}

fn cmd_test(settings: &Settings, provider: Option<ProviderArg>) -> Result<()> {
    if !settings.quiet {
        banner();
    }
    let ids: Vec<&str> = match provider {
        Some(p) => vec![p.id()],
        None => resolve_providers(&settings.providers),
    };

    let mut failures = 0usize;
    for id in ids {
        let (ok, message, snap) = providers::test_provider(id);
        let message = if settings.privacy {
            if let Some(account) = snap.account.as_deref() {
                message.replace(account, &privacy::redact_account(account))
            } else {
                message
            }
        } else {
            message
        };
        if ok {
            print_success(&format!("{:<12} {message}", providers::provider_label(id)));
        } else {
            failures += 1;
            print_warn(&format!("{:<12} {message}", providers::provider_label(id)));
        }
    }
    println!();
    if failures > 0 {
        print_error(&format!("{failures} provider(s) failed"));
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_providers(quiet: bool) {
    if !quiet {
        banner();
    }
    for p in ProviderArg::all() {
        println!(
            "  {}  {}",
            p.id().with(WHITE),
            providers::provider_label(p.id()).with(usagenometer::ui::GRAY)
        );
    }
    println!();
}

fn cmd_json(settings: &Settings) -> Result<()> {
    let snaps = fetch(settings);
    emit_json(&snaps, settings.pretty)
}

fn cmd_check(settings: &Settings, fail_under: f64) -> Result<()> {
    let snaps = fetch(settings);
    if settings.json {
        emit_json(&snaps, settings.pretty)?;
    } else if matches!(settings.format, Some(OutputFormat::Prometheus)) {
        export::emit_prometheus(&snaps)?;
    } else if settings.compact {
        print_compact(&snaps, settings.display);
    } else if !settings.quiet {
        banner();
        print_status_opts(
            &snaps,
            &StatusOptions {
                display: settings.display,
                privacy: settings.privacy,
                etas: None,
            },
        );
    }
    let (ok, messages) = export::check_fail_under(&snaps, fail_under);
    if !ok {
        for m in &messages {
            print_error(m);
        }
        std::process::exit(2);
    }
    if !settings.quiet && !settings.compact && !settings.json {
        print_success(&format!("all meters above {fail_under:.0}% remaining"));
    }
    Ok(())
}

fn cmd_history(
    settings: &Settings,
    limit: usize,
    provider: Option<ProviderArg>,
    spark: bool,
) -> Result<()> {
    let store = HistoryStore::open()?;
    if !settings.quiet {
        banner();
        print_info(&format!("db {}", paths::display_path(store.path())));
        println!();
    }
    let pid = provider.map(|p| p.id());
    let rows = store.recent(limit, pid)?;
    if rows.is_empty() {
        print_info("no history yet — run usg status a few times");
        println!();
        return Ok(());
    }
    for sample in &rows {
        println!(
            "  {}",
            history::format_history_line(sample, settings.privacy)
                .with(usagenometer::ui::GRAY)
        );
    }
    if spark {
        let providers: Vec<String> = if let Some(p) = pid {
            vec![p.to_string()]
        } else {
            let mut ids: Vec<String> = rows.iter().map(|r| r.provider_id.clone()).collect();
            ids.sort();
            ids.dedup();
            ids
        };
        println!();
        for pid in providers {
            if let Ok(points) = store.recent_meter_points(&pid, 48) {
                let mut by_meter: std::collections::HashMap<String, Vec<f64>> =
                    std::collections::HashMap::new();
                for p in points {
                    by_meter
                        .entry(format!("{}|{}", p.meter_id, p.meter_title))
                        .or_default()
                        .push(p.used_percent);
                }
                for (key, vals) in by_meter {
                    let title = key.split('|').nth(1).unwrap_or("?");
                    let spark = history::sparkline(&vals, 24);
                    println!(
                        "  {} {:<16} {}",
                        providers::provider_label(&pid).with(WHITE),
                        title.with(usagenometer::ui::GRAY),
                        spark.with(usagenometer::ui::BRIGHT)
                    );
                }
            }
        }
    }
    println!();
    Ok(())
}

fn cmd_config(settings: &Settings, dump: bool) {
    if !settings.quiet {
        banner();
    }
    println!(
        "  {} {}",
        "path".with(WHITE),
        paths::display_path(&ConfigFile::path()).with(usagenometer::ui::GRAY)
    );
    println!(
        "  {} {}",
        "data".with(WHITE),
        paths::display_path(&paths::data_dir()).with(usagenometer::ui::GRAY)
    );
    println!(
        "  {} {}",
        "cache".with(WHITE),
        paths::display_path(&paths::cache_dir()).with(usagenometer::ui::GRAY)
    );
    println!();
    if dump {
        print!("{}", settings.dump_toml());
    } else {
        print_info("use `usg config --dump` for effective TOML");
        println!();
    }
}

fn cmd_completions(shell: ShellArg) {
    let mut cmd = Cli::command();
    let name = "usg";
    let shell = match shell {
        ShellArg::Bash => Shell::Bash,
        ShellArg::Zsh => Shell::Zsh,
        ShellArg::Fish => Shell::Fish,
        ShellArg::Elvish => Shell::Elvish,
        ShellArg::Powershell => Shell::PowerShell,
    };
    generate(shell, &mut cmd, name, &mut io::stdout());
}

fn emit_json(
    snaps: &[usagenometer::providers::types::ProviderSnapshot],
    pretty: bool,
) -> Result<()> {
    if pretty {
        serde_json::to_writer_pretty(io::stdout().lock(), snaps)?;
    } else {
        serde_json::to_writer(io::stdout().lock(), snaps)?;
    }
    println!();
    Ok(())
}
