//! Interactive TUI (ratatui) — B&W live meters + token ledger dashboard.

use std::collections::HashMap;
use std::io::{self, Stdout};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{
    BarChart, Block, Borders, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::cli::DisplayMode;
use crate::config::Settings;
use crate::providers::types::SnapshotStatus;
use crate::providers::{self, types::ProviderSnapshot};
use crate::tokens::{TokenEvent, TokenStore};

const DAY_SECS: f64 = 86_400.0;
const TOKENS_WINDOW_DAYS: f64 = 30.0;
const CHART_DAYS: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Quota,
    Tokens,
}

struct App {
    tab: Tab,
    snaps: Vec<ProviderSnapshot>,
    selected: usize,
    list_state: ListState,
    last_refresh: Instant,
    interval: Duration,
    message: String,
    settings: Settings,
    etas: HashMap<String, String>,
    tokens: TokenView,
    token_state: TableState,
}

pub fn run(settings: &Settings, start_on_tokens: bool) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, settings, start_on_tokens);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    settings: &Settings,
    start_on_tokens: bool,
) -> Result<()> {
    let (snaps, etas) = fetch_with_etas(settings);
    let mut app = App {
        tab: if start_on_tokens {
            Tab::Tokens
        } else {
            Tab::Quota
        },
        snaps,
        selected: 0,
        list_state: ListState::default().with_selected(Some(0)),
        last_refresh: Instant::now(),
        interval: Duration::from_secs(settings.watch_interval.max(5)),
        message: String::new(),
        settings: settings.clone(),
        etas,
        tokens: load_token_view(),
        token_state: TableState::default().with_selected(Some(0)),
    };
    app.message = help_for(app.tab).into();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = app
            .interval
            .checked_sub(app.last_refresh.elapsed())
            .unwrap_or(Duration::from_millis(50));
        if event::poll(timeout.min(Duration::from_millis(250)))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Tab | KeyCode::BackTab => {
                    app.tab = match app.tab {
                        Tab::Quota => Tab::Tokens,
                        Tab::Tokens => Tab::Quota,
                    };
                    app.message = help_for(app.tab).into();
                }
                KeyCode::Char('1') => {
                    app.tab = Tab::Quota;
                    app.message = help_for(app.tab).into();
                }
                KeyCode::Char('2') => {
                    app.tab = Tab::Tokens;
                    app.message = help_for(app.tab).into();
                }
                KeyCode::Char('r') => match app.tab {
                    Tab::Quota => {
                        let (snaps, etas) = fetch_with_etas(&app.settings);
                        app.snaps = snaps;
                        app.etas = etas;
                        app.last_refresh = Instant::now();
                        app.message = "refreshed".into();
                    }
                    Tab::Tokens => {
                        let ids = providers::resolve_providers(&app.settings.providers);
                        let added = crate::tokens::scan_all(&ids);
                        app.tokens = load_token_view();
                        app.clamp_token_selection();
                        app.message = format!("rescan · {added} new events");
                    }
                },
                KeyCode::Down | KeyCode::Char('j') => match app.tab {
                    Tab::Quota => {
                        if !app.snaps.is_empty() {
                            app.selected = (app.selected + 1) % app.snaps.len();
                            app.list_state.select(Some(app.selected));
                        }
                    }
                    Tab::Tokens => {
                        let rows = app.tokens.by_model.len();
                        if rows > 0 {
                            let i = app.token_state.selected().unwrap_or(0);
                            app.token_state.select(Some((i + 1) % rows));
                        }
                    }
                },
                KeyCode::Up | KeyCode::Char('k') => match app.tab {
                    Tab::Quota => {
                        if !app.snaps.is_empty() {
                            app.selected = if app.selected == 0 {
                                app.snaps.len() - 1
                            } else {
                                app.selected - 1
                            };
                            app.list_state.select(Some(app.selected));
                        }
                    }
                    Tab::Tokens => {
                        let rows = app.tokens.by_model.len();
                        if rows > 0 {
                            let i = app.token_state.selected().unwrap_or(0);
                            app.token_state
                                .select(Some(if i == 0 { rows - 1 } else { i - 1 }));
                        }
                    }
                },
                _ => {}
            }
        }

        if app.last_refresh.elapsed() >= app.interval {
            let (snaps, etas) = fetch_with_etas(&app.settings);
            app.snaps = snaps;
            app.etas = etas;
            if app.tab == Tab::Tokens {
                app.tokens = load_token_view();
                app.clamp_token_selection();
            }
            app.last_refresh = Instant::now();
            app.message = format!("auto refresh · every {}s", app.interval.as_secs());
        }
    }
    Ok(())
}

impl App {
    fn clamp_token_selection(&mut self) {
        let rows = self.tokens.by_model.len();
        let sel = self.token_state.selected().unwrap_or(0);
        self.token_state.select(if rows == 0 {
            None
        } else {
            Some(sel.min(rows - 1))
        });
    }
}

fn help_for(tab: Tab) -> &'static str {
    match tab {
        Tab::Quota => "q quit · r refresh · j/k select · 2/tab tokens",
        Tab::Tokens => "q quit · r rescan · j/k select · 1/tab quota",
    }
}

fn fetch(settings: &Settings) -> Vec<ProviderSnapshot> {
    providers::fetch_all_cached(&settings.providers, settings.cache_ttl, settings.history)
}

fn fetch_with_etas(settings: &Settings) -> (Vec<ProviderSnapshot>, HashMap<String, String>) {
    let snaps = fetch(settings);
    let etas = crate::history::HistoryStore::open()
        .ok()
        .map(|history| crate::eta::eta_map_from_history(&history, &snaps))
        .unwrap_or_default();
    (snaps, etas)
}

fn ui(f: &mut Frame, app: &mut App) {
    match app.tab {
        Tab::Quota => ui_quota(f, app),
        Tab::Tokens => ui_tokens(f, app),
    }
}

fn render_title(f: &mut Frame, app: &App, area: Rect) {
    let on = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let off = Style::default().fg(Color::DarkGray);
    let (quota, tokens) = match app.tab {
        Tab::Quota => (on, off),
        Tab::Tokens => (off, on),
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled("◈ usagenometer tui   ", on),
        Span::styled("[1] quota", quota),
        Span::styled("   ", off),
        Span::styled("[2] tokens", tokens),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(title, area);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let help = Paragraph::new(app.message.as_str()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(help, area);
}

fn bordered(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
}

fn ui_quota(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Percentage(40),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(f.area());

    render_title(f, app, chunks[0]);

    let items: Vec<ListItem> = app
        .snaps
        .iter()
        .map(|s| {
            let status = match s.status {
                SnapshotStatus::Ok => "ok",
                SnapshotStatus::Auth => "auth",
                SnapshotStatus::Error => "err",
                SnapshotStatus::Disabled => "off",
            };
            let stale = s
                .stale_age_secs
                .map(|a| format!(" (stale {}m)", a / 60))
                .unwrap_or_default();
            let line = format!("{:<12} {status}{stale}", s.label);
            ListItem::new(line).style(Style::default().fg(Color::Gray))
        })
        .collect();
    let list = List::new(items)
        .block(bordered("providers"))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    f.render_stateful_widget(list, chunks[1], &mut app.list_state);

    let detail = app
        .snaps
        .get(app.selected)
        .map(|s| format_detail(s, app.settings.display, app.settings.privacy, &app.etas))
        .unwrap_or_else(|| "no providers".into());
    let detail_w = Paragraph::new(detail)
        .style(Style::default().fg(Color::Gray))
        .wrap(Wrap { trim: false })
        .block(bordered("detail"));
    f.render_widget(detail_w, chunks[2]);

    render_footer(f, app, chunks[3]);
}

fn ui_tokens(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(9),
            Constraint::Length(2),
        ])
        .split(f.area());

    render_title(f, app, chunks[0]);
    render_footer(f, app, chunks[3]);

    if app.tokens.events == 0 {
        let empty = Paragraph::new("no token data yet — run `usg tokens` to scan")
            .style(Style::default().fg(Color::Gray))
            .block(bordered("tokens"));
        f.render_widget(empty, chunks[1]);
        render_daily_chart(f, &app.tokens.daily, chunks[2]);
        return;
    }

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(5)])
        .split(chunks[1]);
    render_totals_strip(f, &app.tokens, body[0]);

    let tables = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(body[1]);
    render_model_table(f, app, tables[0]);
    render_project_table(
        f,
        &app.tokens.by_project,
        app.tokens.month.total(),
        tables[1],
    );

    render_daily_chart(f, &app.tokens.daily, chunks[2]);
}

fn render_totals_strip(f: &mut Frame, view: &TokenView, area: Rect) {
    let row = |label: &str, t: &TokenTotals| {
        Line::from(format!(
            "{label:<6} in {:>7}   out {:>7}   cache {:>7}   ·   total {:>8}",
            fmt_tokens(t.input),
            fmt_tokens(t.output),
            fmt_tokens(t.cache()),
            fmt_tokens(t.total()),
        ))
    };
    let strip = Paragraph::new(vec![
        row("today", &view.today),
        row("7d", &view.week),
        row("30d", &view.month),
    ])
    .style(Style::default().fg(Color::Gray))
    .block(bordered("tokens (UTC days)"));
    f.render_widget(strip, area);
}

fn breakdown_rows(rows: &[(String, TokenTotals)], grand_total: u64) -> Vec<Row<'static>> {
    rows.iter()
        .map(|(name, t)| {
            let share = if grand_total > 0 {
                format!("{:.0}%", t.total() as f64 * 100.0 / grand_total as f64)
            } else {
                "—".into()
            };
            Row::new(vec![name.clone(), fmt_tokens(t.total()), share])
                .style(Style::default().fg(Color::Gray))
        })
        .collect()
}

fn breakdown_table<'a>(title: &'static str, header: &'static str, rows: Vec<Row<'a>>) -> Table<'a> {
    Table::new(
        rows,
        [
            Constraint::Min(8),
            Constraint::Length(8),
            Constraint::Length(5),
        ],
    )
    .header(Row::new(vec![header, "tokens", "share"]).style(Style::default().fg(Color::DarkGray)))
    .block(bordered(title))
}

fn render_model_table(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = breakdown_rows(&app.tokens.by_model, app.tokens.month.total());
    let table = breakdown_table("by model · 30d", "model", rows)
        .row_highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    f.render_stateful_widget(table, area, &mut app.token_state);
}

fn render_project_table(f: &mut Frame, rows: &[(String, TokenTotals)], grand: u64, area: Rect) {
    let rows = breakdown_rows(rows, grand);
    let table = breakdown_table("by project · 30d", "project", rows);
    f.render_widget(table, area);
}

fn render_daily_chart(f: &mut Frame, daily: &[(String, u64)], area: Rect) {
    let data: Vec<(&str, u64)> = daily.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    let total: u64 = daily.iter().map(|(_, v)| v).sum();
    let chart = BarChart::default()
        .block(bordered(format!(
            "last {CHART_DAYS} days · {}",
            fmt_tokens(total)
        )))
        .data(&data)
        .bar_width(4)
        .bar_gap(1)
        .bar_style(Style::default().fg(Color::Gray))
        .value_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )
        .label_style(Style::default().fg(Color::DarkGray));
    f.render_widget(chart, area);
}

fn format_detail(
    snap: &ProviderSnapshot,
    display: DisplayMode,
    privacy: bool,
    etas: &HashMap<String, String>,
) -> String {
    let mut lines = Vec::new();
    let mut header = snap.label.clone();
    if let Some(account) = snap.account.as_deref().filter(|s| !s.is_empty()) {
        let a = if privacy {
            crate::privacy::redact_account(account)
        } else {
            account.to_string()
        };
        header.push_str(&format!("  ·  {a}"));
    }
    if let Some(plan) = snap.plan.as_deref().filter(|s| !s.is_empty()) {
        header.push_str(&format!("  ·  {plan}"));
    }
    lines.push(header);
    match snap.status {
        SnapshotStatus::Ok if !snap.meters.is_empty() => {
            for m in &snap.meters {
                let fraction = match display {
                    DisplayMode::Left => m.left_percent.or_else(|| m.percent.map(|p| 1.0 - p)),
                    DisplayMode::Used => m.percent.or_else(|| m.left_percent.map(|p| 1.0 - p)),
                };
                let pct = fraction
                    .map(|f| format!("{:.0}%", f * 100.0))
                    .unwrap_or_else(|| "—".into());
                let eta = etas
                    .get(&format!("{}/{}", snap.id, m.id))
                    .map(|value| format!("  ·  runway {value}"))
                    .unwrap_or_default();
                lines.push(format!("  {:<20} {pct}{eta}", m.title));
            }
        }
        _ => {
            let note = snap.error.as_deref().unwrap_or(snap.status.as_str());
            lines.push(format!("  {} {note}", snap.status.as_str()));
        }
    }
    lines.join("\n")
}

// ---- token aggregation (pure, unit-tested) ----

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TokenTotals {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl TokenTotals {
    fn add(&mut self, event: &TokenEvent) {
        self.input += event.input_tokens;
        self.output += event.output_tokens;
        self.cache_read += event.cache_read_tokens;
        self.cache_write += event.cache_write_tokens;
    }

    fn cache(&self) -> u64 {
        self.cache_read + self.cache_write
    }

    fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[derive(Debug, Default)]
struct TokenView {
    events: usize,
    today: TokenTotals,
    week: TokenTotals,
    month: TokenTotals,
    by_model: Vec<(String, TokenTotals)>,
    by_project: Vec<(String, TokenTotals)>,
    daily: Vec<(String, u64)>,
}

impl TokenView {
    fn build(events: &[TokenEvent], now_unix: f64) -> Self {
        let day0 = day_start(now_unix);
        Self {
            events: events.len(),
            today: totals_in_range(events, day0),
            week: totals_in_range(events, day0 - 6.0 * DAY_SECS),
            month: totals_in_range(events, day0 - (TOKENS_WINDOW_DAYS - 1.0) * DAY_SECS),
            by_model: breakdown_by(events, |e| e.model.as_deref()),
            by_project: breakdown_by(events, |e| e.project.as_deref()),
            daily: daily_totals(events, CHART_DAYS, now_unix),
        }
    }
}

fn load_token_view() -> TokenView {
    let now = now_unix();
    let from = day_start(now) - (TOKENS_WINDOW_DAYS - 1.0) * DAY_SECS;
    let events = TokenStore::open()
        .ok()
        .and_then(|store| store.events_since(Some(from), None).ok())
        .unwrap_or_default();
    TokenView::build(&events, now)
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Start (00:00 UTC) of the day containing `ts_unix`.
fn day_start(ts_unix: f64) -> f64 {
    ts_unix.div_euclid(DAY_SECS) * DAY_SECS
}

fn totals_in_range(events: &[TokenEvent], from_unix: f64) -> TokenTotals {
    let mut totals = TokenTotals::default();
    for event in events.iter().filter(|e| e.ts_unix >= from_unix) {
        totals.add(event);
    }
    totals
}

fn breakdown_by<'a>(
    events: &'a [TokenEvent],
    key: impl Fn(&'a TokenEvent) -> Option<&'a str>,
) -> Vec<(String, TokenTotals)> {
    let mut map: HashMap<String, TokenTotals> = HashMap::new();
    for event in events {
        let name = key(event).filter(|s| !s.is_empty()).unwrap_or("unknown");
        map.entry(name.to_string()).or_default().add(event);
    }
    let mut rows: Vec<(String, TokenTotals)> = map.into_iter().collect();
    rows.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Per-day totals for the last `days` UTC days, oldest first.
fn daily_totals(events: &[TokenEvent], days: usize, now_unix: f64) -> Vec<(String, u64)> {
    let last = (now_unix / DAY_SECS).floor() as i64;
    let first = last - days as i64 + 1;
    let mut sums = vec![0u64; days.max(1)];
    for event in events {
        let idx = (event.ts_unix / DAY_SECS).floor() as i64;
        if idx >= first && idx <= last {
            sums[(idx - first) as usize] += event_total(event);
        }
    }
    (0..days)
        .map(|i| {
            let ts = (first + i as i64) * DAY_SECS as i64;
            (day_label(ts), sums.get(i).copied().unwrap_or(0))
        })
        .collect()
}

fn day_label(day_start_unix: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(day_start_unix)
        .map(|d| format!("{:02}", d.day()))
        .unwrap_or_else(|_| "?".into())
}

fn event_total(event: &TokenEvent) -> u64 {
    event.input_tokens + event.output_tokens + event.cache_read_tokens + event.cache_write_tokens
}

/// Compact token count: `942`, `1.5k`, `12k`, `2.3M`.
fn fmt_tokens(n: u64) -> String {
    const K: f64 = 1_000.0;
    const M: f64 = 1_000_000.0;
    let n_f = n as f64;
    if n_f >= M {
        trim1(n_f / M, "M")
    } else if n_f >= K {
        trim1(n_f / K, "k")
    } else {
        n.to_string()
    }
}

fn trim1(v: f64, suffix: &str) -> String {
    let s = format!("{v:.1}");
    format!("{}{suffix}", s.strip_suffix(".0").unwrap_or(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        provider: &str,
        model: Option<&str>,
        project: Option<&str>,
        ts: f64,
        total: u64,
    ) -> TokenEvent {
        TokenEvent {
            provider: provider.into(),
            model: model.map(Into::into),
            session_id: None,
            project: project.map(Into::into),
            ts_unix: ts,
            input_tokens: total / 2,
            output_tokens: total / 4,
            cache_read_tokens: total / 8,
            cache_write_tokens: total - total / 2 - total / 4 - total / 8,
        }
    }

    #[test]
    fn fmt_tokens_compact() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(942), "942");
        assert_eq!(fmt_tokens(1_000), "1k");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(12_340), "12.3k");
        assert_eq!(fmt_tokens(2_000_000), "2M");
        assert_eq!(fmt_tokens(2_345_678), "2.3M");
    }

    #[test]
    fn day_start_floors_to_utc_midnight() {
        assert_eq!(day_start(86_400.0 * 10.0 + 3_600.0), 86_400.0 * 10.0);
        assert_eq!(day_start(0.0), 0.0);
    }

    #[test]
    fn totals_in_range_filters_and_sums() {
        let events = vec![
            event("codex", Some("m"), None, 100.0, 80),
            event("codex", Some("m"), None, 300.0, 40),
        ];
        let t = totals_in_range(&events, 200.0);
        assert_eq!(t.input, 20);
        assert_eq!(t.output, 10);
        assert_eq!(t.cache(), 10);
        assert_eq!(t.total(), 40);
        assert_eq!(totals_in_range(&events, 0.0).total(), 120);
    }

    #[test]
    fn breakdown_sorts_desc_and_labels_missing() {
        let events = vec![
            event("codex", Some("small"), Some("a"), 0.0, 10),
            event("codex", Some("big"), Some("a"), 0.0, 90),
            event("codex", Some("big"), None, 0.0, 20),
            event("codex", None, None, 0.0, 5),
        ];
        let models = breakdown_by(&events, |e| e.model.as_deref());
        assert_eq!(models[0].0, "big");
        assert_eq!(models[0].1.total(), 110);
        assert_eq!(models[1].0, "small");
        assert_eq!(models[2].0, "unknown");
        let projects = breakdown_by(&events, |e| e.project.as_deref());
        assert_eq!(projects[0].0, "a");
        assert_eq!(projects[1].0, "unknown");
    }

    #[test]
    fn daily_totals_buckets_last_14_days() {
        let now = 86_400.0 * 20.0 + 12_000.0; // midday of day 20
        let events = vec![
            event("codex", None, None, 86_400.0 * 20.0, 16), // today
            event("codex", None, None, 86_400.0 * 19.5, 8),  // yesterday
            event("codex", None, None, 86_400.0 * 6.0, 8),   // day 6 — outside 14d window
        ];
        let daily = daily_totals(&events, 14, now);
        assert_eq!(daily.len(), 14);
        assert_eq!(daily[13].1, 16); // today is last bucket
        assert_eq!(daily[12].1, 8); // yesterday
        assert_eq!(daily.iter().map(|(_, v)| *v).sum::<u64>(), 24);
        assert_eq!(daily[13].0, day_label(86_400 * 20));
    }

    #[test]
    fn token_view_empty_is_zeroed() {
        let view = TokenView::build(&[], 86_400.0 * 20.0);
        assert_eq!(view.events, 0);
        assert_eq!(view.today.total(), 0);
        assert_eq!(view.daily.len(), CHART_DAYS);
        assert!(view.by_model.is_empty());
    }

    #[test]
    fn token_view_windows() {
        let now = 86_400.0 * 20.0 + 100.0;
        let events = vec![
            event("codex", Some("m"), None, 86_400.0 * 20.0 + 50.0, 16), // today
            event("codex", Some("m"), None, 86_400.0 * 15.0, 8),         // this week
            event("codex", Some("m"), None, 86_400.0 * 2.0, 8),          // within 30d
            event("codex", Some("m"), None, 86_400.0 * -20.0, 8), // before window — dropped only if loaded; build() still counts it in breakdowns
        ];
        let view = TokenView::build(&events, now);
        assert_eq!(view.today.total(), 16);
        assert_eq!(view.week.total(), 24);
        assert_eq!(view.month.total(), 32);
        // breakdowns cover every loaded event
        assert_eq!(view.by_model[0].1.total(), 40);
    }

    // ---- headless rendering ----

    fn test_settings() -> Settings {
        Settings {
            providers: vec![],
            display: DisplayMode::Left,
            quiet: true,
            compact: false,
            privacy: false,
            json: false,
            pretty: false,
            format: None,
            alert: None,
            alert_eta: None,
            notify: false,
            cache_ttl: 0,
            history: false,
            watch_interval: 60,
            config: crate::config::ConfigFile::default(),
        }
    }

    fn test_app(tab: Tab, events: &[TokenEvent], now: f64) -> App {
        App {
            tab,
            snaps: vec![],
            selected: 0,
            list_state: ListState::default(),
            last_refresh: Instant::now(),
            interval: Duration::from_secs(60),
            message: help_for(tab).into(),
            settings: test_settings(),
            etas: HashMap::new(),
            tokens: TokenView::build(events, now),
            token_state: TableState::default().with_selected(Some(0)),
        }
    }

    fn buffer_text(terminal: &Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let (w, h) = (buf.area.width as usize, buf.area.height as usize);
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x as u16, y as u16)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn tokens_tab_renders_empty_state() {
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app(Tab::Tokens, &[], 86_400.0 * 20.0);
        terminal.draw(|f| ui_tokens(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("no token data yet"));
        assert!(text.contains("usg tokens"));
        assert!(text.contains("[2] tokens"));
    }

    #[test]
    fn tokens_tab_renders_sections() {
        let backend = ratatui::backend::TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        let now = 86_400.0 * 20.0 + 100.0;
        let events = vec![
            event("codex", Some("gpt-5"), Some("app"), now - 50.0, 1_600),
            event(
                "claude",
                Some("claude-4"),
                Some("site"),
                now - 86_400.0,
                800,
            ),
        ];
        let mut app = test_app(Tab::Tokens, &events, now);
        terminal.draw(|f| ui_tokens(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        for needle in [
            "today",
            "7d",
            "30d",
            "by model",
            "by project",
            "last 14 days",
            "gpt-5",
            "claude-4",
        ] {
            assert!(text.contains(needle), "missing {needle}");
        }
    }
}
