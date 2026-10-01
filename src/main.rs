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

use usagenometer::alerts::{self, AlertStateStore};
use usagenometer::cli::{
    Cli, Command, OutputFormat, ProviderArg, ShellArg, TokenGroupBy, TokenPeriod,
};
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
use usagenometer::tokens::{self, TokenStore};
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
        Some(Command::Providers { verbose }) => {
            cmd_providers(settings.quiet, verbose);
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
        Some(Command::Tokens { period, by, since }) => {
            cmd_tokens(&settings, period, &by, since.as_deref())?;
        }
        Some(Command::History {
            limit,
            provider,
            spark,
            runway,
        }) => {
            cmd_history(&settings, limit, provider, spark, runway)?;
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
    if settings.notify {
        let mut store = AlertStateStore::open();
        let transitions = store.reconcile(events);
        alerts::print_alerts(&transitions.new_events, settings.quiet);
        alerts::maybe_notify(&transitions.new_events, true);
        alerts::maybe_notify_recoveries(&transitions.recovered, true);
    } else {
        alerts::print_alerts(&events, settings.quiet);
    }

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
    let mut alert_store = settings.notify.then(AlertStateStore::open);

    loop {
        let snaps = fetch(settings);

        if diff {
            if let Some(ref p) = prev {
                let mut stdout = io::stdout();
                let _ = stdout.execute(cursor::MoveTo(0, 0));
                let _ = stdout.execute(Clear(ClearType::All));
                if !settings.quiet {
                    banner();
                    print_info(&format!("watch diff · every {interval}s · Ctrl-C to quit"));
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
        let events = alerts::evaluate(&snaps, settings, eta_secs.as_ref());
        if let Some(store) = alert_store.as_mut() {
            let transitions = store.reconcile(events);
            alerts::print_alerts(&transitions.new_events, settings.quiet);
            alerts::maybe_notify(&transitions.new_events, true);
            alerts::maybe_notify_recoveries(&transitions.recovered, true);
        } else {
            alerts::print_alerts(&events, settings.quiet);
        }

        if !settings.quiet
            && !settings.compact
            && !diff
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

fn cmd_providers(quiet: bool, verbose: bool) {
    if !quiet {
        banner();
    }
    for p in ProviderArg::all() {
        let mut line = format!("  {}  {}", p.id(), providers::provider_label(p.id()));
        if verbose {
            let c = providers::provider_capabilities(p.id());
            let mut facts = vec![if c.real_quota { "quota" } else { "status only" }];
            if c.money_balance {
                facts.push("balance");
            }
            if c.reset_windows {
                facts.push("resets");
            }
            if c.local_history {
                facts.push("history");
            }
            if c.token_ledger {
                facts.push("ledger");
            }
            line.push_str(&format!("  ·  {}", facts.join(", ")));
        }
        println!("{}", line.with(WHITE));
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
    runway: bool,
) -> Result<()> {
    let store = HistoryStore::open()?;
    if !settings.quiet {
        banner();
        print_info(&format!("db {}", paths::display_path(store.path())));
        println!();
    }
    let pid = provider.map(|p| p.id());
    if runway {
        let rows = store.runway(pid)?;
        if rows.is_empty() {
            print_info("no history yet — run usg status a few times");
        } else {
            print_info(
                "runway is a local linear estimate; flat or reset-heavy meters show no estimate",
            );
            println!();
            for row in rows {
                println!(
                    "  {}",
                    history::format_runway_line(&row).with(usagenometer::ui::GRAY)
                );
            }
        }
        println!();
        return Ok(());
    }
    let rows = store.recent(limit, pid)?;
    if rows.is_empty() {
        print_info("no history yet — run usg status a few times");
        println!();
        return Ok(());
    }
    for sample in &rows {
        println!(
            "  {}",
            history::format_history_line(sample, settings.privacy).with(usagenometer::ui::GRAY)
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

/// `usg tokens` — scan local agent logs (incremental) and report the ledger.
fn cmd_tokens(
    settings: &Settings,
    period: TokenPeriod,
    by: &[TokenGroupBy],
    since: Option<&str>,
) -> Result<()> {
    let selected: Vec<&str> = settings.providers.iter().map(|p| p.id()).collect();
    let scan_ids: Vec<&str> = if selected.is_empty() {
        vec!["claude", "codex", "grok", "gemini", "cursor"]
    } else {
        selected.clone()
    };
    let scanned = tokens::scan_all(&scan_ids);
    let store = TokenStore::open()?;
    let local = tokens::local_offset();

    // Window: period lower bound, tightened by --since when present.
    let now = tokens::now_unix();
    let period_start = match period {
        TokenPeriod::Today => Some(tokens::day_start_unix(now, local)),
        TokenPeriod::Week => Some(now - 7.0 * 86400.0),
        TokenPeriod::Month => Some(now - 30.0 * 86400.0),
        TokenPeriod::All => None,
    };
    let since_unix = match since {
        Some(s) => Some(tokens::parse_since(s).ok_or_else(|| {
            anyhow::anyhow!("invalid --since '{s}' (want RFC3339 or YYYY-MM-DD)")
        })?),
        None => None,
    };
    let lower = [period_start, since_unix]
        .into_iter()
        .flatten()
        .reduce(f64::max);

    let mut events = store.events_since(lower, None)?;
    if !selected.is_empty() {
        let wanted: std::collections::HashSet<&str> = selected.iter().copied().collect();
        events.retain(|e| wanted.contains(e.provider.as_str()));
    }
    if settings.privacy {
        for e in &mut events {
            if let Some(p) = e.project.as_mut() {
                *p = privacy::redact_account(p);
            }
        }
    }

    if settings.json || matches!(settings.format, Some(OutputFormat::Json)) {
        return emit_tokens_json(&events, scanned, period, by, settings.pretty);
    }

    if !settings.quiet {
        banner();
        print_info(&format!(
            "token ledger · +{} new events · {}",
            tokens::fmt_num(scanned as u64),
            paths::display_path(store.path())
        ));
        println!();
    }

    if events.is_empty() {
        print_info("no token events yet — scanned ~/.claude and ~/.codex local logs");
        println!();
        return Ok(());
    }

    if by.is_empty() && since.is_none() && period == TokenPeriod::Week {
        print_tokens_overview(&events, local);
    } else {
        let groups: Vec<tokens::Group> = by.iter().map(|g| map_group(*g)).collect();
        print_tokens_table(&events, &groups, by);
    }
    println!();
    Ok(())
}

fn map_group(g: TokenGroupBy) -> tokens::Group {
    match g {
        TokenGroupBy::Model => tokens::Group::Model,
        TokenGroupBy::Project => tokens::Group::Project,
        TokenGroupBy::Session => tokens::Group::Session,
        TokenGroupBy::Day => tokens::Group::Day,
    }
}

fn print_tokens_overview(
    events: &[usagenometer::tokens::TokenEvent],
    local: time::UtcOffset,
) {
    use std::collections::BTreeMap;
    use usagenometer::tokens::{Totals, day_start_unix, fmt_num, now_unix};

    let now = now_unix();
    let today = day_start_unix(now, local);
    let windows: [(&str, f64); 3] = [
        ("today", today),
        ("7d", now - 7.0 * 86400.0),
        ("30d", now - 30.0 * 86400.0),
    ];
    let mut providers: BTreeMap<String, Vec<usagenometer::tokens::TokenEvent>> = BTreeMap::new();
    for e in events {
        providers.entry(e.provider.clone()).or_default().push(e.clone());
    }
    let header = format!(
        "  {:<12} {:<7} {:>12} {:>12} {:>12} {:>12} {:>14}",
        "provider", "period", "input", "output", "cache-read", "cache-write", "total"
    );
    println!("{}", header.with(usagenometer::ui::DIM));
    for (provider, evs) in &providers {
        for (label, start) in windows {
            let mut t = Totals::default();
            for e in evs.iter().filter(|e| e.ts_unix >= start) {
                t.input += e.input_tokens;
                t.output += e.output_tokens;
                t.cache_read += e.cache_read_tokens;
                t.cache_write += e.cache_write_tokens;
            }
            println!(
                "{}",
                format!(
                    "  {:<12} {:<7} {:>12} {:>12} {:>12} {:>12} {:>14}",
                    provider,
                    label,
                    fmt_num(t.input),
                    fmt_num(t.output),
                    fmt_num(t.cache_read),
                    fmt_num(t.cache_write),
                    fmt_num(t.total())
                )
                .with(usagenometer::ui::GRAY)
            );
        }
    }
}

fn print_tokens_table(
    events: &[usagenometer::tokens::TokenEvent],
    groups: &[tokens::Group],
    by: &[TokenGroupBy],
) {
    use usagenometer::tokens::fmt_num;

    let rows = tokens::aggregate(events, groups);
    let dim_names: Vec<String> = by.iter().map(|g| group_name(*g).to_string()).collect();
    let key_head = if dim_names.is_empty() {
        String::new()
    } else {
        format!(" {:<24}", dim_names.join(" / "))
    };
    println!(
        "{}",
        format!(
            "  {:<12}{} {:>12} {:>12} {:>12} {:>12} {:>14}",
            "provider", key_head, "input", "output", "cache-read", "cache-write", "total"
        )
        .with(usagenometer::ui::DIM)
    );
    for row in &rows {
        let key_cell = if row.keys.is_empty() {
            String::new()
        } else {
            format!(" {:<24}", truncate_key(&row.keys.join(" / "), 24))
        };
        println!(
            "{}",
            format!(
                "  {:<12}{} {:>12} {:>12} {:>12} {:>12} {:>14}",
                row.provider,
                key_cell,
                fmt_num(row.totals.input),
                fmt_num(row.totals.output),
                fmt_num(row.totals.cache_read),
                fmt_num(row.totals.cache_write),
                fmt_num(row.totals.total())
            )
            .with(usagenometer::ui::GRAY)
        );
    }
}

fn group_name(g: TokenGroupBy) -> &'static str {
    match g {
        TokenGroupBy::Model => "model",
        TokenGroupBy::Project => "project",
        TokenGroupBy::Session => "session",
        TokenGroupBy::Day => "day",
    }
}

fn truncate_key(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn emit_tokens_json(
    events: &[usagenometer::tokens::TokenEvent],
    scanned: usize,
    period: TokenPeriod,
    by: &[TokenGroupBy],
    pretty: bool,
) -> Result<()> {
    let groups: Vec<tokens::Group> = by.iter().map(|g| map_group(*g)).collect();
    let rows = tokens::aggregate(events, &groups);
    let dim_names: Vec<String> = by.iter().map(|g| group_name(*g).to_string()).collect();
    let mut out_rows = Vec::new();
    for row in &rows {
        let mut obj = serde_json::json!({
            "provider": row.provider,
            "input_tokens": row.totals.input,
            "output_tokens": row.totals.output,
            "cache_read_tokens": row.totals.cache_read,
            "cache_write_tokens": row.totals.cache_write,
            "total_tokens": row.totals.total(),
        });
        for (name, value) in dim_names.iter().zip(row.keys.iter()) {
            obj[name] = serde_json::Value::String(value.clone());
        }
        out_rows.push(obj);
    }
    let doc = serde_json::json!({
        "scanned_new_events": scanned,
        "period": format!("{period:?}").to_lowercase(),
        "groups": out_rows,
    });
    if pretty {
        serde_json::to_writer_pretty(io::stdout().lock(), &doc)?;
    } else {
        serde_json::to_writer(io::stdout().lock(), &doc)?;
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
