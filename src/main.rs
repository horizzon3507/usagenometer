//! usagenometer — AI usage meters in the terminal.
//!
//! Binaries: `usagenometer` and short alias `usg`.

use std::io;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use crossterm::style::Stylize;
use crossterm::terminal::{Clear, ClearType};
use crossterm::{ExecutableCommand, cursor};
use time::{Duration as TimeDuration, OffsetDateTime, Time};

use usagenometer::alerts::{self, AlertStateStore};
use usagenometer::cli::{
    BudgetPeriod, Cli, Command, OutputFormat, ProviderArg, ShellArg, TokenGroup, TokenPeriod,
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
use usagenometer::tokens::{self, TokenEvent};
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
        Some(Command::History {
            limit,
            provider,
            spark,
            runway,
        }) => {
            cmd_history(&settings, limit, provider, spark, runway)?;
        }
        Some(Command::Tokens {
            period,
            by,
            since,
            cost,
        }) => {
            cmd_tokens(&settings, period, by, since, cost)?;
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

fn token_period_start(period: TokenPeriod) -> Option<f64> {
    match period {
        TokenPeriod::Today => Some(day_start_unix()),
        TokenPeriod::Week => Some(week_start_unix()),
        TokenPeriod::Month => Some(month_start_unix()),
        TokenPeriod::All => None,
    }
}

/// `--since` accepts unix seconds or `YYYY-MM-DD` (UTC midnight).
fn parse_since(s: &str) -> Result<f64> {
    let s = s.trim();
    if let Ok(ts) = s.parse::<f64>() {
        return Ok(ts);
    }
    let fmt =
        time::format_description::parse("[year]-[month]-[day]").context("parse date format")?;
    let date = time::Date::parse(s, &fmt)
        .with_context(|| format!("invalid --since '{s}' (use unix seconds or YYYY-MM-DD)"))?;
    Ok(date.with_time(Time::MIDNIGHT).assume_utc().unix_timestamp() as f64)
}

fn fmt_day_key(ts_unix: f64) -> String {
    OffsetDateTime::from_unix_timestamp(ts_unix as i64)
        .map(|dt| dt.date().to_string())
        .unwrap_or_default()
}

fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_usd(usd: f64) -> String {
    format!("${usd:.4}")
}

#[derive(Default)]
struct TokenAgg {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    events: usize,
    cost: f64,
    unpriced: usize,
}

impl TokenAgg {
    fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, ev: &TokenEvent, cost: Option<f64>) {
        self.input += ev.input_tokens;
        self.output += ev.output_tokens;
        self.cache_read += ev.cache_read_tokens;
        self.cache_write += ev.cache_write_tokens;
        self.events += 1;
        match cost {
            Some(c) => self.cost += c,
            None => self.unpriced += 1,
        }
    }
}

fn cmd_tokens(
    settings: &Settings,
    period: TokenPeriod,
    by: Option<TokenGroup>,
    since_arg: Option<String>,
    cost: bool,
) -> Result<()> {
    let provider_ids: Vec<&str> = settings.providers.iter().map(|p| p.id()).collect();
    let scanned = tokens::scan_all(&provider_ids);
    let store = tokens::TokenStore::open()?;
    let since = match &since_arg {
        Some(s) => Some(parse_since(s)?),
        None => token_period_start(period),
    };
    let events = events_for_providers(&store, since, &provider_ids)?;

    let group = by.unwrap_or(TokenGroup::Day);
    let mut groups: std::collections::HashMap<String, TokenAgg> = std::collections::HashMap::new();
    let mut totals = TokenAgg::default();
    for ev in &events {
        let price = if cost {
            pricing::event_cost_usd_merged(ev, &settings.config.pricing)
        } else {
            None
        };
        let key = match group {
            TokenGroup::Day => fmt_day_key(ev.ts_unix),
            TokenGroup::Model => ev.model.clone().unwrap_or_else(|| "(unknown)".into()),
            TokenGroup::Project => ev.project.clone().unwrap_or_else(|| "(none)".into()),
            TokenGroup::Session => ev.session_id.clone().unwrap_or_else(|| "(none)".into()),
        };
        groups.entry(key).or_default().add(ev, price);
        totals.add(ev, price);
    }
    let mut rows: Vec<(String, TokenAgg)> = groups.into_iter().collect();
    match group {
        TokenGroup::Day => rows.sort_by(|a, b| a.0.cmp(&b.0)),
        _ => rows.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then(a.0.cmp(&b.0))),
    }

    if settings.json {
        let mut rows_json: Vec<serde_json::Value> = Vec::new();
        for (key, agg) in &rows {
            let mut row = serde_json::json!({
                "key": key,
                "input_tokens": agg.input,
                "output_tokens": agg.output,
                "cache_read_tokens": agg.cache_read,
                "cache_write_tokens": agg.cache_write,
                "total_tokens": agg.total(),
                "events": agg.events,
            });
            if cost {
                row["cost_usd"] = serde_json::json!(agg.cost);
                row["unpriced_events"] = serde_json::json!(agg.unpriced);
            }
            rows_json.push(row);
        }
        let mut totals_json = serde_json::json!({
            "input_tokens": totals.input,
            "output_tokens": totals.output,
            "cache_read_tokens": totals.cache_read,
            "cache_write_tokens": totals.cache_write,
            "total_tokens": totals.total(),
            "events": totals.events,
        });
        if cost {
            totals_json["cost_usd"] = serde_json::json!(totals.cost);
            totals_json["unpriced_events"] = serde_json::json!(totals.unpriced);
        }
        let out = serde_json::json!({
            "period": period.id(),
            "since": since,
            "group_by": group.id(),
            "scan_new_events": scanned,
            "rows": rows_json,
            "totals": totals_json,
        });
        if settings.pretty {
            serde_json::to_writer_pretty(io::stdout().lock(), &out)?;
        } else {
            serde_json::to_writer(io::stdout().lock(), &out)?;
        }
        println!();
        return Ok(());
    }

    if !settings.quiet {
        banner();
        let since_label = since
            .map(fmt_day_key)
            .unwrap_or_else(|| "start".to_string());
        let mut head = format!(
            "tokens · {} · since {since_label} · by {}",
            period.id(),
            group.id()
        );
        if scanned > 0 {
            head.push_str(&format!(" · +{scanned} scanned"));
        }
        print_info(&head);
        println!();
    }

    if rows.is_empty() {
        print_info("no token events yet — JSONL scanners land with the tokens-core PR");
        println!();
        return Ok(());
    }

    let dollar = |agg: &TokenAgg| -> String {
        if !cost {
            String::new()
        } else if agg.events == agg.unpriced {
            format!("{:>10}", "—")
        } else {
            format!("{:>10}", fmt_usd(agg.cost))
        }
    };
    println!(
        "  {:<28} {:>12} {:>12} {:>12} {:>14}{}",
        group.id().with(WHITE),
        "in".with(WHITE),
        "out".with(WHITE),
        "cache".with(WHITE),
        "total".with(WHITE),
        if cost { "         $" } else { "" }
    );
    for (key, agg) in &rows {
        println!(
            "  {:<28} {:>12} {:>12} {:>12} {:>14}{}",
            truncate_key(key, 28).with(usagenometer::ui::GRAY),
            fmt_int(agg.input).with(usagenometer::ui::GRAY),
            fmt_int(agg.output).with(usagenometer::ui::GRAY),
            fmt_int(agg.cache_read + agg.cache_write).with(usagenometer::ui::GRAY),
            fmt_int(agg.total()),
            dollar(agg)
        );
    }
    println!(
        "  {:<28} {:>12} {:>12} {:>12} {:>14}{}",
        "total".with(WHITE),
        fmt_int(totals.input).with(WHITE),
        fmt_int(totals.output).with(WHITE),
        fmt_int(totals.cache_read + totals.cache_write).with(WHITE),
        fmt_int(totals.total()).with(WHITE),
        dollar(&totals)
    );
    if cost && totals.unpriced > 0 {
        print_info(&format!(
            "{} event(s) have unknown-model pricing and are excluded from $",
            totals.unpriced
        ));
    }
    println!();
    Ok(())
}

fn truncate_key(key: &str, max: usize) -> String {
    if key.len() <= max {
        return key.to_string();
    }
    format!("…{}", &key[key.len() - (max - 1)..])
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
