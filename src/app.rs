//! usagenometer — AI usage meters in the terminal.
//!
//! Binaries: `usagenometer` and short alias `usg`.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, IsTerminal};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use crossterm::style::Stylize;
use crossterm::terminal::{Clear, ClearType};
use crossterm::{ExecutableCommand, cursor};
use time::{Duration as TimeDuration, OffsetDateTime, Time};

use usagenometer::alerts::{self, AlertStateStore, AlertTracker};
use usagenometer::cli::{
    BudgetPeriod, Cli, Command, OutputFormat, ProviderArg, ShellArg, TokenGroupBy, TokenPeriod,
};
use usagenometer::config::{ConfigFile, Settings};
use usagenometer::doctor;
use usagenometer::eta;
use usagenometer::explain;
use usagenometer::export;
use usagenometer::history::{self, HistoryStore};
use usagenometer::http::HttpClient;
use usagenometer::paths;
use usagenometer::pricing;
use usagenometer::privacy;
use usagenometer::providers::{self, resolve_providers};
use usagenometer::routing;
use usagenometer::tokens::{self, TokenEvent, TokenStore};
use usagenometer::ui::{
    StatusOptions, WHITE, banner, bin_name, flush_stdout, print_compact_ext, print_diff,
    print_error, print_info, print_status_opts, print_success, print_warn,
};

pub fn cli_main() {
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
    let settings = build_settings(&cli)?;
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
            cmd_providers(&settings, &cli.providers, verbose)?;
        }
        Some(Command::Json) => {
            cmd_json(&settings)?;
        }
        Some(Command::Check {
            fail_under,
            min_balance_usd,
            fail_eta_hours,
            budget_usd,
            period,
            here,
            projects,
        }) => {
            cmd_check(
                &settings,
                &cli.providers,
                fail_under,
                min_balance_usd,
                fail_eta_hours,
                budget_usd,
                period,
                here,
                &projects,
            )?;
        }
        Some(Command::Pick {
            min_left,
            strategy,
            cmd,
            allow_stale,
        }) => {
            cmd_pick(&settings, min_left, strategy, cmd, allow_stale)?;
        }
        Some(Command::Run {
            min_left,
            strategy,
            args,
        }) => {
            cmd_run(&settings, min_left, strategy, &args)?;
        }
        Some(Command::Doctor { fix }) => {
            if fix {
                let actions = doctor::run_fixes();
                if !settings.json {
                    for a in &actions {
                        print_success(a);
                    }
                }
            }
            let checks = doctor::run(settings.privacy);
            let fails = checks
                .iter()
                .filter(|c| c.status == doctor::CheckStatus::Fail)
                .count();
            if settings.json {
                let doc = serde_json::json!({
                    "ok": fails == 0,
                    "checks": checks,
                });
                if settings.pretty {
                    serde_json::to_writer_pretty(io::stdout().lock(), &doc)?;
                } else {
                    serde_json::to_writer(io::stdout().lock(), &doc)?;
                }
                println!();
            } else {
                doctor::print_report(&checks);
            }
            if fails > 0 {
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
            until,
            here,
            projects,
            session,
            top,
            compare,
            spark,
            heatmap,
            rescan,
            cost,
        }) => {
            cmd_tokens(
                &settings,
                &cli.providers,
                TokensOpts {
                    period,
                    by: &by,
                    since: since.as_deref(),
                    until: until.as_deref(),
                    here,
                    projects: &projects,
                    session: session.as_deref(),
                    top,
                    compare,
                    spark,
                    heatmap,
                    rescan,
                    cost,
                },
            )?;
        }
        Some(Command::History {
            limit,
            provider,
            spark,
            runway,
            prune,
            diff,
        }) => {
            cmd_history(
                &settings,
                limit,
                provider,
                spark,
                runway,
                prune,
                diff.as_deref(),
            )?;
        }
        Some(Command::Config {
            dump,
            path,
            init,
            force,
            validate,
        }) => {
            cmd_config(
                &settings,
                cli.config.as_deref(),
                dump,
                path,
                init,
                force,
                validate,
            )?;
        }
        Some(Command::Tui { tokens }) => {
            usagenometer::tui::run(&settings, tokens)?;
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

fn build_settings(cli: &Cli) -> Result<Settings> {
    let (config, active_project_config, warnings) = ConfigFile::load_with_options(
        cli.config.as_deref(),
        cli.profile.as_deref(),
        !cli.no_project_config,
    )?;
    for w in warnings {
        if !cli.quiet {
            eprintln!("warning: {w}");
        }
    }
    let providers = if !cli.providers.is_empty() {
        cli.providers.clone()
    } else if cli.all || cli.include_unconfigured || cli.only_unconfigured {
        vec![]
    } else {
        config.parse_providers()
    };
    let display = cli.display.unwrap_or_else(|| config.display_mode());
    let compact = cli.compact || config.compact;
    let compact_cost = cli.compact_cost || config.compact_cost;
    let template = cli
        .template
        .clone()
        .or_else(|| config.compact_template.clone());
    let privacy = cli.privacy || config.privacy;
    let notify = cli.notify || config.notify;
    let alert = cli.alert.or(config.alert);
    let alert_eta = cli.alert_eta.or(config.alert_eta);
    let alert_balance_usd = cli.alert_balance_usd.or(config.alert_balance_usd);
    let json = cli.json || matches!(cli.format, Some(OutputFormat::Json));
    Ok(Settings {
        providers,
        selection: if cli.all {
            providers::AgentSelection::All
        } else if cli.only_unconfigured {
            providers::AgentSelection::OnlyUnconfigured
        } else if cli.include_unconfigured {
            providers::AgentSelection::IncludeUnconfigured
        } else {
            providers::AgentSelection::Ready
        },
        display,
        quiet: cli.quiet,
        compact,
        compact_cost,
        template,
        privacy,
        json,
        pretty: cli.pretty,
        format: cli.format,
        alert,
        alert_eta,
        alert_balance_usd,
        notify,
        cache_ttl: config.cache_ttl_secs(),
        history: config.history,
        watch_interval: config.watch_interval_secs(),
        profile: cli.profile.clone(),
        active_project_config,
        config,
    })
}

fn fetch(
    settings: &Settings,
    diagnostic: bool,
) -> Vec<usagenometer::providers::types::ProviderSnapshot> {
    fetch_with_client(settings, diagnostic, None)
}

fn fetch_with_client(
    settings: &Settings,
    diagnostic: bool,
    client: Option<&HttpClient>,
) -> Vec<usagenometer::providers::types::ProviderSnapshot> {
    let selection = if diagnostic && !settings.providers.is_empty() {
        providers::AgentSelection::All
    } else {
        settings.selection
    };
    let mut snaps = providers::fetch_all_cached_with_profile(
        &settings.providers,
        settings.cache_ttl,
        settings.history,
        selection,
        settings.profile.as_deref(),
        client,
    );
    if !diagnostic && settings.selection == providers::AgentSelection::Ready {
        snaps.retain(|snap| snap.status != usagenometer::providers::types::SnapshotStatus::Auth);
    }
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

fn compact_cost_suffix(settings: &Settings) -> Option<String> {
    if !settings.compact_cost {
        return None;
    }
    let store = TokenStore::open().ok()?;
    let now = tokens::now_unix();
    let today = tokens::day_start_unix(now, tokens::local_offset());
    let events = store.events_since(Some(today), None).ok()?;
    if events.is_empty() {
        return None;
    }
    let (usd, _) = pricing::total_cost_usd(&events, &settings.config.pricing);
    Some(format!("${usd:.2}"))
}

fn has_notification_hooks(settings: &Settings) -> bool {
    settings.notify
        || settings
            .config
            .notify_webhook_url
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        || settings
            .config
            .notify_command
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
}

fn handle_alerts(
    snaps: &[usagenometer::providers::types::ProviderSnapshot],
    settings: &Settings,
    eta_secs: Option<&HashMap<String, f64>>,
    store: &mut Option<AlertStateStore>,
    tracker: &mut AlertTracker,
    print_output: bool,
) {
    let events = alerts::evaluate(snaps, settings, eta_secs);
    if let Some(st) = store.as_mut() {
        let transitions = st.reconcile(events);
        if print_output {
            alerts::print_alerts(&transitions.new_events, settings.quiet);
        }
        alerts::maybe_notify(&transitions.new_events, settings.notify);
        alerts::maybe_notify_recoveries(&transitions.recovered, settings.notify);
        alerts::dispatch_hooks(&transitions, settings);
    } else if print_output {
        let fresh = tracker.filter_new(events);
        alerts::print_alerts(&fresh, settings.quiet);
    }
}

fn cmd_status(settings: &Settings, compact: bool) -> Result<()> {
    let snaps = fetch(settings, false);

    if settings.json || matches!(settings.format, Some(OutputFormat::Json)) {
        return emit_json(&snaps, settings.pretty);
    }
    if matches!(settings.format, Some(OutputFormat::Prometheus)) {
        return export::emit_prometheus_with_pricing(&snaps, &settings.config.pricing);
    }
    if matches!(settings.format, Some(OutputFormat::Waybar)) {
        let history = HistoryStore::open().ok();
        let etas = history
            .as_ref()
            .map(|h| eta::eta_map_from_history(h, &snaps));
        let cost = compact_cost_suffix(settings);
        return export::emit_waybar(
            &snaps,
            settings.display,
            |id| settings.alert_for(id),
            etas.as_ref(),
            settings.template.as_deref(),
            cost.as_deref(),
        );
    }

    if compact {
        let cost = compact_cost_suffix(settings);
        print_compact_ext(
            &snaps,
            settings.display,
            settings.template.as_deref(),
            cost.as_deref(),
            settings.quiet,
        );
        return Ok(());
    }

    if !settings.quiet {
        banner();
    }
    if snaps.is_empty() {
        if !settings.quiet {
            print_info(
                "no matching agents; use usg providers --only-unconfigured to set up credentials, or --all to list every agent",
            );
        }
        return Ok(());
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
    if has_notification_hooks(settings) {
        let mut store = AlertStateStore::open();
        let transitions = store.reconcile(events);
        alerts::print_alerts(&transitions.new_events, settings.quiet);
        alerts::maybe_notify(&transitions.new_events, settings.notify);
        alerts::maybe_notify_recoveries(&transitions.recovered, settings.notify);
        alerts::dispatch_hooks(&transitions, settings);
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
    let shared_client = HttpClient::new().ok();
    let mut prev: Option<Vec<_>> = None;
    let mut alert_store = has_notification_hooks(settings).then(AlertStateStore::open);
    let mut alert_tracker = AlertTracker::new();
    let stream_compact = settings.compact && !io::stdout().is_terminal();

    loop {
        let snaps = fetch_with_client(settings, false, shared_client.as_ref());
        let history = HistoryStore::open().ok();
        let eta_secs = history
            .as_ref()
            .map(|h| eta::eta_seconds_from_history(h, &snaps));

        if settings.json || matches!(settings.format, Some(OutputFormat::Json)) {
            emit_json(&snaps, false)?;
            handle_alerts(
                &snaps,
                settings,
                eta_secs.as_ref(),
                &mut alert_store,
                &mut alert_tracker,
                false,
            );
            flush_stdout();
            thread::sleep(Duration::from_secs(interval));
            continue;
        }
        if matches!(settings.format, Some(OutputFormat::Prometheus)) {
            export::emit_prometheus_with_pricing(&snaps, &settings.config.pricing)?;
            handle_alerts(
                &snaps,
                settings,
                eta_secs.as_ref(),
                &mut alert_store,
                &mut alert_tracker,
                false,
            );
            flush_stdout();
            thread::sleep(Duration::from_secs(interval));
            continue;
        }
        if matches!(settings.format, Some(OutputFormat::Waybar)) {
            let etas = history
                .as_ref()
                .map(|h| eta::eta_map_from_history(h, &snaps));
            let cost = compact_cost_suffix(settings);
            export::emit_waybar(
                &snaps,
                settings.display,
                |id| settings.alert_for(id),
                etas.as_ref(),
                settings.template.as_deref(),
                cost.as_deref(),
            )?;
            handle_alerts(
                &snaps,
                settings,
                eta_secs.as_ref(),
                &mut alert_store,
                &mut alert_tracker,
                false,
            );
            flush_stdout();
            thread::sleep(Duration::from_secs(interval));
            continue;
        }

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
        } else if stream_compact {
            let cost = compact_cost_suffix(settings);
            print_compact_ext(
                &snaps,
                settings.display,
                settings.template.as_deref(),
                cost.as_deref(),
                settings.quiet,
            );
        } else {
            let mut stdout = io::stdout();
            let _ = stdout.execute(cursor::MoveTo(0, 0));
            let _ = stdout.execute(Clear(ClearType::All));
            if settings.compact {
                let cost = compact_cost_suffix(settings);
                print_compact_ext(
                    &snaps,
                    settings.display,
                    settings.template.as_deref(),
                    cost.as_deref(),
                    settings.quiet,
                );
            } else {
                if !settings.quiet {
                    banner();
                    print_info(&format!("refresh every {interval}s · Ctrl-C to quit"));
                    println!();
                }
                let etas = history
                    .as_ref()
                    .map(|h| eta::eta_map_from_history(h, &snaps));
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

        handle_alerts(
            &snaps,
            settings,
            eta_secs.as_ref(),
            &mut alert_store,
            &mut alert_tracker,
            !stream_compact,
        );

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
        None => {
            providers::selected_ids(&resolve_providers(&settings.providers), settings.selection)
        }
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

fn cmd_providers(settings: &Settings, filter: &[ProviderArg], verbose: bool) -> Result<()> {
    if !settings.quiet && !settings.json {
        banner();
    }
    let mut rows = Vec::new();
    for p in ProviderArg::catalog() {
        if !filter.is_empty() && !filter.contains(p) {
            continue;
        }
        let state = providers::availability::inspect(p.id());
        if !settings.selection.matches(&state) {
            continue;
        }
        let c = providers::provider_capabilities(p.id());
        if settings.json {
            rows.push(serde_json::json!({
                "id": p.id(),
                "label": providers::provider_label(p.id()),
                "installed": state.installed,
                "credentials": state.credentials,
                "credential_validation": "local",
                "ready": state.ready(),
                "detail": state.detail,
                "setup": state.setup,
                "quota": c.real_quota,
                "balance": c.money_balance,
                "token_ledger": c.token_ledger,
            }));
            continue;
        }
        let mut line = format!(
            "  {}  {}  ·  {}  ·  {}",
            p.id(),
            providers::provider_label(p.id()),
            if state.installed {
                "installed"
            } else {
                "not installed"
            },
            state.credentials
        );
        if verbose {
            line.push_str(&format!("  ·  {}", state.detail));
            let mut facts = Vec::new();
            if c.real_quota {
                facts.push("quota");
            }
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
            if facts.is_empty() {
                facts.push("no usage API");
            }
            line.push_str(&format!("  ·  {}", facts.join(", ")));
        }
        if !state.ready() {
            line.push_str(&format!("  ·  {}", state.setup));
        }
        println!("{}", line.with(WHITE));
    }
    if settings.json {
        if settings.pretty {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!("{}", serde_json::to_string(&rows)?);
        }
    } else {
        println!();
    }
    Ok(())
}

fn cmd_json(settings: &Settings) -> Result<()> {
    let snaps = fetch(settings, false);
    emit_json(&snaps, settings.pretty)
}

#[allow(clippy::too_many_arguments)]
fn cmd_check(
    settings: &Settings,
    ledger_filter: &[ProviderArg],
    fail_under: f64,
    min_balance_usd: Option<f64>,
    fail_eta_hours: Option<f64>,
    budget_usd: Option<f64>,
    period: Option<BudgetPeriod>,
    here: bool,
    projects: &[String],
) -> Result<()> {
    let snaps = fetch(settings, true);
    let eta_secs = if fail_eta_hours.is_some() {
        HistoryStore::open()
            .ok()
            .map(|h| eta::eta_seconds_from_history(&h, &snaps))
    } else {
        None
    };

    if settings.json {
        emit_json(&snaps, settings.pretty)?;
    } else if matches!(settings.format, Some(OutputFormat::Prometheus)) {
        export::emit_prometheus_with_pricing(&snaps, &settings.config.pricing)?;
    } else if settings.compact {
        let cost = compact_cost_suffix(settings);
        print_compact_ext(
            &snaps,
            settings.display,
            settings.template.as_deref(),
            cost.as_deref(),
            settings.quiet,
        );
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
    let mut report = export::check_fail_under_ext(
        &snaps,
        fail_under,
        min_balance_usd,
        eta_secs.as_ref(),
        fail_eta_hours,
    );

    // Token budget gate: CLI flag wins over config.toml `budget_usd`/`budget_period`.
    if let Some(budget) = budget_usd.or_else(|| settings.config.budget()) {
        let window = period
            .map(|p| p.id().to_string())
            .or_else(|| settings.config.budget_window())
            .unwrap_or_else(|| "day".to_string());
        // Removing an agent or its login must not erase spend from a budget check.
        let provider_ids =
            providers::selected_ledger_ids(ledger_filter, providers::AgentSelection::All);
        let scanned = tokens::scan_all(&provider_ids)?;
        let store = tokens::TokenStore::open()?;
        let mut events =
            events_for_providers(&store, Some(budget_window_start(&window)), &provider_ids)?;
        let project_filters = resolve_project_filters(here, projects);
        if !project_filters.is_empty() {
            events.retain(|ev| {
                ev.project.as_deref().is_some_and(|p| {
                    let norm = tokens::canonical_project_name(p);
                    project_filters
                        .iter()
                        .any(|f| f.eq_ignore_ascii_case(&norm) || f.eq_ignore_ascii_case(p))
                })
            });
        }
        let (total, unpriced) = pricing::total_cost_usd(&events, &settings.config.pricing);
        if !settings.quiet
            && !settings.json
            && !matches!(settings.format, Some(OutputFormat::Prometheus))
        {
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
            report.outcome = export::CheckOutcome::Breached;
            report.messages.extend(budget_msgs);
        }
    }

    let exit_code = match report.outcome {
        export::CheckOutcome::Passed => 0,
        export::CheckOutcome::Breached => 2,
        export::CheckOutcome::Unavailable => 1,
    };
    if exit_code != 0 {
        for message in &report.messages {
            eprintln!("error: {message}");
        }
        std::process::exit(exit_code);
    }
    if !settings.quiet
        && !settings.compact
        && !settings.json
        && !matches!(settings.format, Some(OutputFormat::Prometheus))
    {
        print_success(&format!("all meters above {fail_under:.0}% remaining"));
    }
    Ok(())
}

fn cmd_pick(
    settings: &Settings,
    min_left: f64,
    strategy: routing::RoutingStrategy,
    cmd: bool,
    allow_stale: bool,
) -> Result<()> {
    let snaps = fetch(settings, true);
    let eta_secs = HistoryStore::open()
        .ok()
        .map(|h| eta::eta_seconds_from_history(&h, &snaps));
    let ranked =
        routing::rank_candidates(&snaps, strategy, min_left, eta_secs.as_ref(), allow_stale);
    if settings.json {
        if settings.pretty {
            serde_json::to_writer_pretty(io::stdout().lock(), &ranked)?;
        } else {
            serde_json::to_writer(io::stdout().lock(), &ranked)?;
        }
        println!();
        if ranked.is_empty() {
            let has_quota = snaps.iter().any(|s| {
                s.status == usagenometer::providers::types::SnapshotStatus::Ok
                    && (allow_stale || s.stale_age_secs.is_none())
                    && s.meters
                        .iter()
                        .any(|m| m.percent.is_some() || m.left_percent.is_some())
            });
            std::process::exit(if has_quota { 2 } else { 1 });
        }
        return Ok(());
    }
    let Some(winner) = ranked.first() else {
        let has_quota = snaps.iter().any(|s| {
            s.status == usagenometer::providers::types::SnapshotStatus::Ok
                && (allow_stale || s.stale_age_secs.is_none())
                && s.meters
                    .iter()
                    .any(|m| m.percent.is_some() || m.left_percent.is_some())
        });
        eprintln!(
            "error: no eligible provider meets {:.0}% remaining quota",
            min_left
        );
        std::process::exit(if has_quota { 2 } else { 1 });
    };
    if cmd {
        if let Some(ref bin) = winner.binary {
            println!("{bin}");
        } else {
            eprintln!(
                "error: no executable binary found for {}",
                winner.provider_id
            );
            std::process::exit(1);
        }
    } else {
        println!("{}", winner.provider_id);
    }
    Ok(())
}

fn cmd_run(
    settings: &Settings,
    min_left: f64,
    strategy: routing::RoutingStrategy,
    args: &[String],
) -> Result<()> {
    let snaps = fetch(settings, true);
    let eta_secs = HistoryStore::open()
        .ok()
        .map(|h| eta::eta_seconds_from_history(&h, &snaps));
    let ranked = routing::rank_candidates(&snaps, strategy, min_left, eta_secs.as_ref(), false);
    let Some(winner) = ranked.into_iter().find(|c| c.binary.is_some()) else {
        eprintln!("error: no eligible installed provider CLI meets {min_left:.0}% remaining");
        std::process::exit(1);
    };
    let bin = winner.binary.unwrap();
    let status = ProcessCommand::new(&bin)
        .args(args)
        .env("USG_PROVIDER", &winner.provider_id)
        .env("USG_LEFT_PCT", format!("{:.0}", winner.worst_left * 100.0))
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn resolve_project_filters(here: bool, projects: &[String]) -> Vec<String> {
    let mut out = projects.to_vec();
    if here && let Some(curr) = tokens::resolve_here_project() {
        out.push(curr);
    }
    out
}

/// Events since `since_unix`, restricted to the selected agents.
fn events_for_providers(
    store: &tokens::TokenStore,
    since_unix: Option<f64>,
    provider_ids: &[&str],
) -> Result<Vec<TokenEvent>> {
    let events = store.events_since(since_unix, None)?;
    Ok(events
        .into_iter()
        .filter(|ev| provider_ids.contains(&ev.provider.as_str()))
        .collect())
}

/// Start of the current local day / UTC ISO week (Mon) / UTC calendar month, as unix seconds.
fn budget_window_start(window: &str) -> f64 {
    match window {
        "week" => week_start_unix(),
        "month" => month_start_unix(),
        _ => tokens::day_start_unix(tokens::now_unix(), tokens::local_offset()),
    }
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
    prune: Option<u64>,
    diff: Option<&str>,
) -> Result<()> {
    let store = HistoryStore::open()?;
    let pid = provider.map(|p| p.id());

    if let Some(days) = prune {
        let deleted = store.prune(days)?;
        if settings.json {
            let doc = serde_json::json!({ "deleted": deleted, "older_than_days": days });
            if settings.pretty {
                serde_json::to_writer_pretty(io::stdout().lock(), &doc)?;
            } else {
                serde_json::to_writer(io::stdout().lock(), &doc)?;
            }
            println!();
        } else {
            print_success(&format!("pruned {deleted} snapshot(s) older than {days}d"));
        }
        return Ok(());
    }

    if let Some(when) = diff {
        let ts = tokens::parse_since(when).ok_or_else(|| {
            anyhow::anyhow!("invalid --diff '{when}' (want RFC3339 or YYYY-MM-DD)")
        })?;
        let base_samples = store.samples_at_or_after(ts, pid)?;
        let recent_samples = store.recent(200, pid)?;
        let to_snap = |s: &history::HistorySample| {
            let mut snap = usagenometer::providers::types::ProviderSnapshot::ok(
                &s.provider_id,
                &s.provider_label,
            );
            snap.meters = serde_json::from_str(&s.meters_json).unwrap_or_default();
            snap
        };
        let mut latest_by_id: BTreeMap<String, usagenometer::providers::types::ProviderSnapshot> =
            BTreeMap::new();
        for s in recent_samples {
            latest_by_id
                .entry(s.provider_id.clone())
                .or_insert_with(|| to_snap(&s));
        }
        let prev: Vec<_> = base_samples.iter().map(to_snap).collect();
        let next: Vec<_> = latest_by_id.into_values().collect();
        if !settings.quiet {
            banner();
            print_info(&format!("diff since {when}"));
            println!();
        }
        print_diff(&prev, &next, settings.display);
        println!();
        return Ok(());
    }

    if !settings.quiet && !settings.json {
        banner();
        print_info(&format!("db {}", paths::display_path(store.path())));
        println!();
    }
    if runway {
        let rows = store.runway(pid)?;
        if settings.json {
            if settings.pretty {
                serde_json::to_writer_pretty(io::stdout().lock(), &rows)?;
            } else {
                serde_json::to_writer(io::stdout().lock(), &rows)?;
            }
            println!();
            return Ok(());
        }
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
    if settings.json {
        if settings.pretty {
            serde_json::to_writer_pretty(io::stdout().lock(), &rows)?;
        } else {
            serde_json::to_writer(io::stdout().lock(), &rows)?;
        }
        println!();
        return Ok(());
    }
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
                let mut by_meter: BTreeMap<String, Vec<f64>> = BTreeMap::new();
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

struct TokensOpts<'a> {
    period: TokenPeriod,
    by: &'a [TokenGroupBy],
    since: Option<&'a str>,
    until: Option<&'a str>,
    here: bool,
    projects: &'a [String],
    session: Option<&'a str>,
    top: Option<usize>,
    compare: bool,
    spark: bool,
    heatmap: bool,
    rescan: bool,
    cost: bool,
}

fn filter_ledger_events(
    mut events: Vec<TokenEvent>,
    scan_ids: &[&str],
    project_filters: &[String],
    session_prefix: Option<&str>,
    privacy_mode: bool,
) -> Vec<TokenEvent> {
    events.retain(|e| scan_ids.contains(&e.provider.as_str()));
    if !project_filters.is_empty() {
        events.retain(|e| {
            e.project.as_deref().is_some_and(|p| {
                let norm = tokens::canonical_project_name(p);
                project_filters
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(&norm) || f.eq_ignore_ascii_case(p))
            })
        });
    }
    if let Some(prefix) = session_prefix {
        events.retain(|e| {
            e.session_id
                .as_deref()
                .is_some_and(|sid| sid.starts_with(prefix))
        });
    }
    if privacy_mode {
        for e in &mut events {
            if let Some(p) = e.project.as_mut() {
                *p = privacy::redact_account(p);
            }
        }
    }
    events
}

/// `usg tokens` — scan local agent logs (incremental) and report the ledger.
fn cmd_tokens(
    settings: &Settings,
    ledger_filter: &[ProviderArg],
    opts: TokensOpts<'_>,
) -> Result<()> {
    let scan_ids = providers::selected_ledger_ids(ledger_filter, settings.selection);
    if opts.rescan {
        let store = TokenStore::open()?;
        store.reset_scan_offsets()?;
    }
    let scanned = tokens::scan_all(&scan_ids)?;
    let store = TokenStore::open()?;
    let local = tokens::local_offset();

    // Window: period lower bound, tightened by --since when present.
    let now = tokens::now_unix();
    let period_start = match opts.period {
        TokenPeriod::Today => Some(tokens::day_start_unix(now, local)),
        TokenPeriod::Week => Some(now - 7.0 * 86400.0),
        TokenPeriod::Month => Some(now - 30.0 * 86400.0),
        TokenPeriod::All => None,
    };
    let since_unix = match opts.since {
        Some(s) => Some(tokens::parse_since(s).ok_or_else(|| {
            anyhow::anyhow!("invalid --since '{s}' (want RFC3339 or YYYY-MM-DD)")
        })?),
        None => None,
    };
    let until_unix = match opts.until {
        Some(u) => Some(tokens::parse_since(u).ok_or_else(|| {
            anyhow::anyhow!("invalid --until '{u}' (want RFC3339 or YYYY-MM-DD)")
        })?),
        None => None,
    };
    let lower = [period_start, since_unix]
        .into_iter()
        .flatten()
        .reduce(f64::max);

    let project_filters = resolve_project_filters(opts.here, opts.projects);
    let raw_events = store.events_in_range(lower, until_unix, None)?;
    let events = filter_ledger_events(
        raw_events,
        &scan_ids,
        &project_filters,
        opts.session,
        settings.privacy,
    );

    let prev_events = if opts.compare
        && let Some(lo) = lower
    {
        let hi = until_unix.unwrap_or(now);
        let span = (hi - lo).max(60.0);
        let prev_raw = store.events_in_range(Some(lo - span), Some(lo), None)?;
        Some(filter_ledger_events(
            prev_raw,
            &scan_ids,
            &project_filters,
            opts.session,
            settings.privacy,
        ))
    } else {
        None
    };

    if matches!(settings.format, Some(OutputFormat::Prometheus)) {
        let mut out = io::stdout().lock();
        export::emit_token_metrics_with_pricing(&mut out, &settings.config.pricing)?;
        return Ok(());
    }

    if matches!(settings.format, Some(OutputFormat::Csv)) {
        let groups: Vec<tokens::Group> = opts.by.iter().map(|g| map_group(*g)).collect();
        let mut rows = tokens::aggregate(&events, &groups);
        let costs = opts.cost.then(|| cost_by_group(&events, &groups, settings));
        if let Some(limit) = opts.top {
            sort_and_truncate_rows(&mut rows, costs.as_ref(), limit);
        }
        let dim_names: Vec<String> = opts.by.iter().map(|g| group_name(*g).to_string()).collect();
        return export::emit_tokens_csv(&rows, &dim_names, costs.as_ref());
    }

    if settings.json || matches!(settings.format, Some(OutputFormat::Json)) {
        return emit_tokens_json(
            &events,
            prev_events.as_deref(),
            scanned,
            opts.period,
            opts.by,
            opts.top,
            settings.pretty,
            opts.cost,
            settings,
        );
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
        print_info(
            "no token events for the selected agents and period; use usg providers --only-unconfigured for setup or usg tokens --all for historical usage",
        );
        println!();
        return Ok(());
    }

    if opts.heatmap {
        usagenometer::ui::print_heatmap(&events, local, opts.cost, &settings.config.pricing);
        println!();
        return Ok(());
    }

    let default_by;
    let effective_by =
        if opts.by.is_empty() && (!project_filters.is_empty() || opts.session.is_some()) {
            default_by = [TokenGroupBy::Model, TokenGroupBy::Session];
            &default_by[..]
        } else {
            opts.by
        };

    if effective_by.is_empty()
        && opts.since.is_none()
        && opts.until.is_none()
        && opts.period == TokenPeriod::Week
        && opts.top.is_none()
        && !opts.spark
        && !opts.compare
    {
        print_tokens_overview(&events, local, opts.cost, settings);
    } else {
        let groups: Vec<tokens::Group> = effective_by.iter().map(|g| map_group(*g)).collect();
        print_tokens_table(
            &events,
            prev_events.as_deref(),
            &groups,
            effective_by,
            opts.cost,
            opts.top,
            opts.spark,
            (lower, until_unix.unwrap_or(now)),
            settings,
        );
    }
    if opts.cost {
        let (total_cost, unpriced) = pricing::total_cost_usd(&events, &settings.config.pricing);
        let savings = pricing::total_cache_savings_usd(&events, &settings.config.pricing);
        let mut totals = tokens::Totals::default();
        for ev in &events {
            totals.add(ev);
        }
        let mut msg = format!("cost {} · {}", opts.period.id(), fmt_usd(total_cost));
        if savings.abs() >= 0.0001 {
            msg.push_str(&format!(" · saved ${savings:.4}"));
        }
        if let Some(hit) = totals.cache_hit_rate()
            && hit > 0.0
        {
            msg.push_str(&format!(" · {:.1}% cache hit", hit * 100.0));
        }
        if unpriced > 0 {
            let missing = pricing::unpriced_models(&events, &settings.config.pricing);
            let names: Vec<String> = missing.keys().take(3).cloned().collect();
            msg.push_str(&format!(" · {unpriced} unpriced ({})", names.join(", ")));
        }
        print_info(&msg);
    }
    println!();
    Ok(())
}

fn sort_and_truncate_rows(
    rows: &mut Vec<tokens::GroupRow>,
    costs: Option<&export::GroupCostMap>,
    limit: usize,
) {
    rows.sort_by(|a, b| {
        if let Some(c) = costs {
            let ca = c
                .get(&(a.provider.clone(), a.keys.clone()))
                .map(|x| x.0)
                .unwrap_or(0.0);
            let cb = c
                .get(&(b.provider.clone(), b.keys.clone()))
                .map(|x| x.0)
                .unwrap_or(0.0);
            cb.partial_cmp(&ca)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.totals.total().cmp(&a.totals.total()))
        } else {
            b.totals.total().cmp(&a.totals.total())
        }
    });
    rows.truncate(limit.max(1));
}

fn row_sparkline(
    events: &[TokenEvent],
    provider: &str,
    keys: &[String],
    groups: &[tokens::Group],
    local: time::UtcOffset,
    lo: f64,
    hi: f64,
) -> String {
    const BINS: usize = 12;
    let span = (hi - lo).max(1.0);
    let mut bins = vec![0u64; BINS];
    for ev in events {
        if ev.provider == provider && ev_group_keys(ev, groups, local) == keys {
            let frac = ((ev.ts_unix - lo) / span).clamp(0.0, 0.9999);
            let idx = (frac * BINS as f64) as usize;
            bins[idx] +=
                ev.input_tokens + ev.output_tokens + ev.cache_read_tokens + ev.cache_write_tokens;
        }
    }
    history::sparkline_u64(&bins, BINS)
}

/// Group keys for an event, mirroring `tokens::aggregate` (for per-row cost).
fn ev_group_keys(ev: &TokenEvent, groups: &[tokens::Group], local: time::UtcOffset) -> Vec<String> {
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
        providers
            .entry(e.provider.clone())
            .or_default()
            .push(e.clone());
    }
    let header = format!(
        "  {:<12} {:<7} {:>12} {:>12} {:>12} {:>12} {:>14}{}",
        "provider",
        "period",
        "input",
        "output",
        "cache-read",
        "cache-write",
        "total",
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

#[allow(clippy::too_many_arguments)]
fn print_tokens_table(
    events: &[usagenometer::tokens::TokenEvent],
    prev_events: Option<&[usagenometer::tokens::TokenEvent]>,
    groups: &[tokens::Group],
    by: &[TokenGroupBy],
    cost: bool,
    top: Option<usize>,
    spark: bool,
    window: (Option<f64>, f64),
    settings: &Settings,
) {
    use usagenometer::tokens::fmt_num;

    let mut rows = tokens::aggregate(events, groups);
    let costs = if cost {
        cost_by_group(events, groups, settings)
    } else {
        std::collections::HashMap::new()
    };
    if let Some(limit) = top {
        sort_and_truncate_rows(&mut rows, cost.then_some(&costs), limit);
    }
    let prev_map: HashMap<(String, Vec<String>), u64> = prev_events
        .map(|evs| {
            tokens::aggregate(evs, groups)
                .into_iter()
                .map(|r| ((r.provider, r.keys), r.totals.total()))
                .collect()
        })
        .unwrap_or_default();

    let dim_names: Vec<String> = by.iter().map(|g| group_name(*g).to_string()).collect();
    let key_head = if dim_names.is_empty() {
        String::new()
    } else {
        format!(" {:<24}", dim_names.join(" / "))
    };
    let delta_head = if prev_events.is_some() {
        "      Δ%"
    } else {
        ""
    };
    let spark_head = if spark { "  trend       " } else { "" };
    println!(
        "{}",
        format!(
            "  {:<12}{} {:>12} {:>12} {:>12} {:>12} {:>14}{}{}{}",
            "provider",
            key_head,
            "input",
            "output",
            "cache-read",
            "cache-write",
            "total",
            if cost { "         $" } else { "" },
            delta_head,
            spark_head,
        )
        .with(usagenometer::ui::DIM)
    );
    let local = tokens::local_offset();
    let lo = window.0.unwrap_or_else(|| {
        events
            .iter()
            .map(|e| e.ts_unix)
            .fold(window.1 - 30.0 * 86400.0, f64::min)
    });
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
        let delta_cell = if prev_events.is_some() {
            let prev_total = prev_map
                .get(&(row.provider.clone(), row.keys.clone()))
                .copied()
                .unwrap_or(0);
            let cur = row.totals.total();
            if prev_total == 0 {
                format!(" {:>7}", if cur > 0 { "new" } else { "—" })
            } else {
                let pct = (cur as f64 - prev_total as f64) * 100.0 / prev_total as f64;
                format!(" {:>+6.0}%", pct)
            }
        } else {
            String::new()
        };
        let spark_cell = if spark {
            format!(
                "  {}",
                row_sparkline(
                    events,
                    &row.provider,
                    &row.keys,
                    groups,
                    local,
                    lo,
                    window.1
                )
            )
        } else {
            String::new()
        };
        println!(
            "{}",
            format!(
                "  {:<12}{} {:>12} {:>12} {:>12} {:>12} {:>14}{}{}{}",
                row.provider,
                key_cell,
                fmt_num(row.totals.input),
                fmt_num(row.totals.output),
                fmt_num(row.totals.cache_read),
                fmt_num(row.totals.cache_write),
                fmt_num(row.totals.total()),
                cost_cell,
                delta_cell,
                spark_cell
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

#[allow(clippy::too_many_arguments)]
fn emit_tokens_json(
    events: &[usagenometer::tokens::TokenEvent],
    prev_events: Option<&[usagenometer::tokens::TokenEvent]>,
    scanned: usize,
    period: TokenPeriod,
    by: &[TokenGroupBy],
    top: Option<usize>,
    pretty: bool,
    cost: bool,
    settings: &Settings,
) -> Result<()> {
    let groups: Vec<tokens::Group> = by.iter().map(|g| map_group(*g)).collect();
    let mut rows = tokens::aggregate(events, &groups);
    let costs = if cost {
        cost_by_group(events, &groups, settings)
    } else {
        std::collections::HashMap::new()
    };
    if let Some(limit) = top {
        sort_and_truncate_rows(&mut rows, cost.then_some(&costs), limit);
    }
    let prev_map: HashMap<(String, Vec<String>), u64> = prev_events
        .map(|evs| {
            tokens::aggregate(evs, &groups)
                .into_iter()
                .map(|r| ((r.provider, r.keys), r.totals.total()))
                .collect()
        })
        .unwrap_or_default();
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
        if prev_events.is_some() {
            let prev = prev_map
                .get(&(row.provider.clone(), row.keys.clone()))
                .copied()
                .unwrap_or(0);
            obj["prev_total_tokens"] = serde_json::json!(prev);
            if prev > 0 {
                obj["delta_pct"] = serde_json::json!(
                    (row.totals.total() as f64 - prev as f64) * 100.0 / prev as f64
                );
            }
        }
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
    let mut total_tokens = tokens::Totals::default();
    for event in events {
        total_tokens.add(event);
    }
    let mut totals = serde_json::json!({
        "input_tokens": total_tokens.input,
        "output_tokens": total_tokens.output,
        "cache_read_tokens": total_tokens.cache_read,
        "cache_write_tokens": total_tokens.cache_write,
        "total_tokens": total_tokens.total(),
    });
    if let Some(hit) = total_tokens.cache_hit_rate() {
        totals["cache_hit_ratio"] = serde_json::json!(hit);
    }
    if cost {
        let (total_cost, unpriced) = pricing::total_cost_usd(events, &settings.config.pricing);
        let savings = pricing::total_cache_savings_usd(events, &settings.config.pricing);
        totals["cost_usd"] = serde_json::json!(total_cost);
        totals["cache_savings_usd"] = serde_json::json!(savings);
        totals["unpriced_events"] = serde_json::json!(unpriced);
    }
    let doc = serde_json::json!({
        "scanned_new_events": scanned,
        "period": period.id(),
        "groups": out_rows,
        "totals": totals,
    });
    if pretty {
        serde_json::to_writer_pretty(io::stdout().lock(), &doc)?;
    } else {
        serde_json::to_writer(io::stdout().lock(), &doc)?;
    }
    println!();
    Ok(())
}

fn cmd_config(
    settings: &Settings,
    explicit_path: Option<&std::path::Path>,
    dump: bool,
    path_only: bool,
    init: bool,
    force: bool,
    validate: bool,
) -> Result<()> {
    let target_path = explicit_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(ConfigFile::path);

    if path_only {
        println!("{}", target_path.display());
        return Ok(());
    }

    if init {
        if target_path.exists() && !force {
            anyhow::bail!(
                "{} already exists (pass --force to overwrite)",
                target_path.display()
            );
        }
        paths::ensure_dir(&target_path)?;
        std::fs::write(&target_path, ConfigFile::starter_template())?;
        print_success(&format!("wrote {}", target_path.display()));
        return Ok(());
    }

    if validate {
        match ConfigFile::load_from(&target_path) {
            Ok(cfg) => {
                let errs = cfg.validate();
                if errs.is_empty() {
                    print_success(&format!("{} is valid", target_path.display()));
                    return Ok(());
                }
                for e in &errs {
                    print_error(e);
                }
                std::process::exit(1);
            }
            Err(e) => {
                print_error(&format!("{e}"));
                std::process::exit(1);
            }
        }
    }

    if !settings.quiet {
        banner();
    }
    println!(
        "  {} {}",
        "path".with(WHITE),
        paths::display_path(&target_path).with(usagenometer::ui::GRAY)
    );
    if let Some(ref proj) = settings.active_project_config {
        println!(
            "  {} {}",
            "project".with(WHITE),
            paths::display_path(proj).with(usagenometer::ui::GRAY)
        );
    }
    if let Some(ref prof) = settings.profile {
        println!(
            "  {} {}",
            "profile".with(WHITE),
            prof.as_str().with(usagenometer::ui::GRAY)
        );
    }
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
    Ok(())
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
