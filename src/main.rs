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
use time::{Duration as TimeDuration, OffsetDateTime, Time};

use usagenometer::alerts::{self, AlertStateStore};
use usagenometer::cli::{
    BudgetPeriod, Cli, Command, OutputFormat, ProviderArg, ShellArg, TokenGroupBy, TokenPeriod,
};
use usagenometer::config::{ConfigFile, Settings};
use usagenometer::doctor;
use usagenometer::eta;
use usagenometer::explain;
use usagenometer::export;
use usagenometer::history::{self, HistoryStore};
use usagenometer::paths;
use usagenometer::pricing;
use usagenometer::privacy;
use usagenometer::providers::{self, resolve_providers};
use usagenometer::routing;
use usagenometer::tokens::{self, TokenEvent, TokenStore};
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
        Some(Command::Check {
            fail_under,
            budget_usd,
            period,
        }) => {
            cmd_check(&settings, fail_under, budget_usd, period)?;
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
        Some(Command::Tokens {
            period,
            by,
            since,
            cost,
        }) => {
            cmd_tokens(&settings, period, &by, since.as_deref(), cost)?;
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

fn cmd_check(
    settings: &Settings,
    fail_under: f64,
    budget_usd: Option<f64>,
    period: Option<BudgetPeriod>,
) -> Result<()> {
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
    let (mut ok, mut messages) = export::check_fail_under(&snaps, fail_under);

    // Token budget gate: CLI flag wins over config.toml `budget_usd`/`budget_period`.
    if let Some(budget) = budget_usd.or_else(|| settings.config.budget()) {
        let window = period
            .map(|p| p.id().to_string())
            .or_else(|| settings.config.budget_window())
            .unwrap_or_else(|| "day".to_string());
        let provider_ids: Vec<&str> = settings.providers.iter().map(|p| p.id()).collect();
        let scanned = tokens::scan_all(&provider_ids);
        let store = tokens::TokenStore::open()?;
        let events =
            events_for_providers(&store, Some(budget_window_start(&window)), &provider_ids)?;
        let (total, unpriced) = pricing::total_cost_usd(&events, &settings.config.pricing);
        if !settings.quiet && !settings.json {
            let mut line = format!(
                "tokens {window} · ${total:.4} / ${budget:.2} budget · {} events",
                events.len()
            );
            if unpriced > 0 {
                line.push_str(&format!(" · {unpriced} unpriced"));
            }
            if scanned > 0 {
                line.push_str(&format!(" · +{scanned} scanned"));
            }
            print_info(&line);
        }
        let (budget_ok, budget_msgs) = export::check_budget_usd(total, budget, &window);
        if !budget_ok {
            ok = false;
            messages.extend(budget_msgs);
        }
    }

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

/// Events since `since_unix`, filtered to `provider_ids` (empty = all).
fn events_for_providers(
    store: &tokens::TokenStore,
    since_unix: Option<f64>,
    provider_ids: &[&str],
) -> Result<Vec<TokenEvent>> {
    let events = store.events_since(since_unix, None)?;
    if provider_ids.is_empty() {
        return Ok(events);
    }
    Ok(events
        .into_iter()
        .filter(|ev| provider_ids.contains(&ev.provider.as_str()))
        .collect())
}

/// UTC start of the current day / week (Mon) / month, as unix seconds.
fn budget_window_start(window: &str) -> f64 {
    match window {
        "week" => week_start_unix(),
        "month" => month_start_unix(),
        _ => day_start_unix(),
    }
}

fn day_start_unix() -> f64 {
    OffsetDateTime::now_utc()
        .date()
        .with_time(Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp() as f64
}

fn week_start_unix() -> f64 {
    let now = OffsetDateTime::now_utc();
    let days_back = i64::from(now.weekday().number_from_monday()) - 1;
    (now.date() - TimeDuration::days(days_back))
        .with_time(Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp() as f64
}

fn month_start_unix() -> f64 {
    let now = OffsetDateTime::now_utc();
    now.date()
        .replace_day(1)
        .unwrap_or_else(|_| now.date())
        .with_time(Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp() as f64
}

fn fmt_usd(usd: f64) -> String {
    format!("${usd:.4}")
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
    cost: bool,
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
        return emit_tokens_json(&events, scanned, period, by, settings.pretty, cost, settings);
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
        print_tokens_overview(&events, local, cost, settings);
    } else {
        let groups: Vec<tokens::Group> = by.iter().map(|g| map_group(*g)).collect();
        print_tokens_table(&events, &groups, by, cost, settings);
    }
    if cost {
        let (total_cost, unpriced) = pricing::total_cost_usd(&events, &settings.config.pricing);
        print_info(&format!(
            "cost {} · {}{}",
            period.id(),
            fmt_usd(total_cost),
            if unpriced > 0 {
                format!(" · {unpriced} event(s) unpriced")
            } else {
                String::new()
            }
        ));
    }
    println!();
    Ok(())
}

/// Group keys for an event, mirroring `tokens::aggregate` (for per-row cost).
fn ev_group_keys(
    ev: &TokenEvent,
    groups: &[tokens::Group],
    local: time::UtcOffset,
) -> Vec<String> {
    groups
        .iter()
        .map(|g| match g {
            tokens::Group::Model => ev.model.clone().unwrap_or_else(|| "?".into()),
            tokens::Group::Project => ev.project.clone().unwrap_or_else(|| "?".into()),
            tokens::Group::Session => ev.session_id.clone().unwrap_or_else(|| "?".into()),
            tokens::Group::Day => tokens::day_key(ev.ts_unix, local),
        })
        .collect()
}

/// (provider, group keys) -> (usd, unpriced events) for a `--cost` column.
fn cost_by_group(
    events: &[TokenEvent],
    groups: &[tokens::Group],
    settings: &Settings,
) -> std::collections::HashMap<(String, Vec<String>), (f64, usize)> {
    let local = tokens::local_offset();
    let mut map: std::collections::HashMap<(String, Vec<String>), (f64, usize)> =
        std::collections::HashMap::new();
    for ev in events {
        let key = (ev.provider.clone(), ev_group_keys(ev, groups, local));
        let entry = map.entry(key).or_default();
        match pricing::event_cost_usd_merged(ev, &settings.config.pricing) {
            Some(c) => entry.0 += c,
            None => entry.1 += 1,
        }
    }
    map
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
    cost: bool,
    settings: &Settings,
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
        "  {:<12} {:<7} {:>12} {:>12} {:>12} {:>12} {:>14}{}",
        "provider", "period", "input", "output", "cache-read", "cache-write", "total",
        if cost { "         $" } else { "" },
    );
    println!("{}", header.with(usagenometer::ui::DIM));
    for (provider, evs) in &providers {
        for (label, start) in windows {
            let filtered: Vec<&usagenometer::tokens::TokenEvent> =
                evs.iter().filter(|e| e.ts_unix >= start).collect();
            let mut t = Totals::default();
            for e in &filtered {
                t.input += e.input_tokens;
                t.output += e.output_tokens;
                t.cache_read += e.cache_read_tokens;
                t.cache_write += e.cache_write_tokens;
            }
            let cost_cell = if cost {
                let window_events: Vec<TokenEvent> =
                    filtered.iter().map(|e| (*e).clone()).collect();
                let (usd, unpriced) =
                    pricing::total_cost_usd(&window_events, &settings.config.pricing);
                if unpriced == window_events.len() {
                    format!(" {:>10}", "—")
                } else {
                    format!(" {:>10}", fmt_usd(usd))
                }
            } else {
                String::new()
            };
            println!(
                "{}",
                format!(
                    "  {:<12} {:<7} {:>12} {:>12} {:>12} {:>12} {:>14}{}",
                    provider,
                    label,
                    fmt_num(t.input),
                    fmt_num(t.output),
                    fmt_num(t.cache_read),
                    fmt_num(t.cache_write),
                    fmt_num(t.total()),
                    cost_cell
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
    cost: bool,
    settings: &Settings,
) {
    use usagenometer::tokens::fmt_num;

    let rows = tokens::aggregate(events, groups);
    let costs = if cost {
        cost_by_group(events, groups, settings)
    } else {
        std::collections::HashMap::new()
    };
    let dim_names: Vec<String> = by.iter().map(|g| group_name(*g).to_string()).collect();
    let key_head = if dim_names.is_empty() {
        String::new()
    } else {
        format!(" {:<24}", dim_names.join(" / "))
    };
    println!(
        "{}",
        format!(
            "  {:<12}{} {:>12} {:>12} {:>12} {:>12} {:>14}{}",
            "provider",
            key_head,
            "input",
            "output",
            "cache-read",
            "cache-write",
            "total",
            if cost { "         $" } else { "" }
        )
        .with(usagenometer::ui::DIM)
    );
    for row in &rows {
        let key_cell = if row.keys.is_empty() {
            String::new()
        } else {
            format!(" {:<24}", truncate_key(&row.keys.join(" / "), 24))
        };
        let cost_cell = if cost {
            let (usd, unpriced) = costs
                .get(&(row.provider.clone(), row.keys.clone()))
                .copied()
                .unwrap_or_default();
            if unpriced > 0 && usd == 0.0 {
                format!(" {:>10}", "—")
            } else {
                format!(" {:>10}", fmt_usd(usd))
            }
        } else {
            String::new()
        };
        println!(
            "{}",
            format!(
                "  {:<12}{} {:>12} {:>12} {:>12} {:>12} {:>14}{}",
                row.provider,
                key_cell,
                fmt_num(row.totals.input),
                fmt_num(row.totals.output),
                fmt_num(row.totals.cache_read),
                fmt_num(row.totals.cache_write),
                fmt_num(row.totals.total()),
                cost_cell
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
    cost: bool,
    settings: &Settings,
) -> Result<()> {
    let groups: Vec<tokens::Group> = by.iter().map(|g| map_group(*g)).collect();
    let rows = tokens::aggregate(events, &groups);
    let costs = if cost {
        cost_by_group(events, &groups, settings)
    } else {
        std::collections::HashMap::new()
    };
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
        if cost {
            let (usd, unpriced) = costs
                .get(&(row.provider.clone(), row.keys.clone()))
                .copied()
                .unwrap_or_default();
            obj["cost_usd"] = serde_json::json!(usd);
            obj["unpriced_events"] = serde_json::json!(unpriced);
        }
        for (name, value) in dim_names.iter().zip(row.keys.iter()) {
            obj[name] = serde_json::Value::String(value.clone());
        }
        out_rows.push(obj);
    }
    let (total_cost, unpriced) = if cost {
        pricing::total_cost_usd(events, &settings.config.pricing)
    } else {
        (0.0, 0)
    };
    let doc = serde_json::json!({
        "scanned_new_events": scanned,
        "period": format!("{period:?}").to_lowercase(),
        "groups": out_rows,
        "totals": if cost {
            serde_json::json!({"cost_usd": total_cost, "unpriced_events": unpriced})
        } else {
            serde_json::Value::Null
        },
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
