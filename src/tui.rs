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
use crate::pricing::ModelPriceOverride;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenFocus {
    Models,
    Projects,
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
    sparks: HashMap<String, String>,
    raw_events: Vec<TokenEvent>,
    tokens: TokenView,
    token_state: TableState,
    project_state: TableState,
    token_focus: TokenFocus,
    cost_mode: bool,
    model_filter: Option<String>,
    project_filter: Option<String>,
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
    let (snaps, etas, sparks) = fetch_with_history(settings, settings.cache_ttl);
    let raw_events = load_raw_events(settings);
    let tokens = build_token_view(&raw_events, settings, false, None, None);
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
        sparks,
        raw_events,
        tokens,
        token_state: TableState::default().with_selected(Some(0)),
        project_state: TableState::default().with_selected(Some(0)),
        token_focus: TokenFocus::Models,
        cost_mode: false,
        model_filter: None,
        project_filter: None,
    };
    app.clamp_quota_selection();
    app.clamp_token_selection();
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
                KeyCode::Char('u') => {
                    app.settings.display = match app.settings.display {
                        DisplayMode::Left => DisplayMode::Used,
                        DisplayMode::Used => DisplayMode::Left,
                    };
                    app.message = match app.settings.display {
                        DisplayMode::Left => "display: remaining %".into(),
                        DisplayMode::Used => "display: used %".into(),
                    };
                }
                KeyCode::Char('p') => {
                    app.settings.privacy = !app.settings.privacy;
                    app.rebuild_tokens();
                    app.message = if app.settings.privacy {
                        "privacy: redacted".into()
                    } else {
                        "privacy: full".into()
                    };
                }
                KeyCode::Char('c') if app.tab == Tab::Tokens => {
                    app.cost_mode = !app.cost_mode;
                    app.rebuild_tokens();
                    app.message = if app.cost_mode {
                        "tokens mode: USD cost (chart in cents)".into()
                    } else {
                        "tokens mode: token counts".into()
                    };
                }
                KeyCode::Left | KeyCode::Char('h') if app.tab == Tab::Tokens => {
                    app.token_focus = TokenFocus::Models;
                    app.message = "focus: models".into();
                }
                KeyCode::Right | KeyCode::Char('l') if app.tab == Tab::Tokens => {
                    app.token_focus = TokenFocus::Projects;
                    app.message = "focus: projects".into();
                }
                KeyCode::Enter if app.tab == Tab::Tokens => {
                    match app.token_focus {
                        TokenFocus::Models => {
                            if let Some(i) = app.token_state.selected()
                                && let Some((name, _)) = app.tokens.by_model.get(i)
                            {
                                let next = name.clone();
                                app.model_filter = if app.model_filter.as_deref() == Some(&next) {
                                    None
                                } else {
                                    Some(next)
                                };
                            }
                        }
                        TokenFocus::Projects => {
                            if let Some(i) = app.project_state.selected()
                                && let Some((name, _)) = app.tokens.by_project.get(i)
                            {
                                let next = name.clone();
                                app.project_filter = if app.project_filter.as_deref() == Some(&next)
                                {
                                    None
                                } else {
                                    Some(next)
                                };
                            }
                        }
                    }
                    app.rebuild_tokens();
                    app.message = match (&app.model_filter, &app.project_filter) {
                        (Some(m), Some(p)) => format!("filter: model={m} project={p}"),
                        (Some(m), None) => format!("filter: model={m}"),
                        (None, Some(p)) => format!("filter: project={p}"),
                        (None, None) => help_for(Tab::Tokens).into(),
                    };
                }
                KeyCode::Backspace if app.tab == Tab::Tokens => {
                    app.model_filter = None;
                    app.project_filter = None;
                    app.rebuild_tokens();
                    app.message = help_for(Tab::Tokens).into();
                }
                KeyCode::Char('r') => match app.tab {
                    Tab::Quota => {
                        let (snaps, etas, sparks) = fetch_with_history(&app.settings, 0);
                        app.snaps = snaps;
                        app.etas = etas;
                        app.sparks = sparks;
                        app.clamp_quota_selection();
                        app.last_refresh = Instant::now();
                        app.message = "refreshed live".into();
                    }
                    Tab::Tokens => {
                        let ids = providers::selected_ledger_ids(
                            &app.settings.providers,
                            app.settings.selection,
                        );
                        match crate::tokens::scan_all(&ids) {
                            Ok(added) => {
                                app.raw_events = load_raw_events(&app.settings);
                                app.rebuild_tokens();
                                app.message = format!("rescan · {added} new events");
                            }
                            Err(error) => app.message = format!("rescan failed: {error}"),
                        }
                    }
                },
                KeyCode::Down | KeyCode::Char('j') => match app.tab {
                    Tab::Quota => {
                        if !app.snaps.is_empty() {
                            app.selected = (app.selected + 1) % app.snaps.len();
                            app.list_state.select(Some(app.selected));
                        }
                    }
                    Tab::Tokens => match app.token_focus {
                        TokenFocus::Models => {
                            let rows = app.tokens.by_model.len();
                            if rows > 0 {
                                let i = app.token_state.selected().unwrap_or(0);
                                app.token_state.select(Some((i + 1) % rows));
                            }
                        }
                        TokenFocus::Projects => {
                            let rows = app.tokens.by_project.len();
                            if rows > 0 {
                                let i = app.project_state.selected().unwrap_or(0);
                                app.project_state.select(Some((i + 1) % rows));
                            }
                        }
                    },
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
                    Tab::Tokens => match app.token_focus {
                        TokenFocus::Models => {
                            let rows = app.tokens.by_model.len();
                            if rows > 0 {
                                let i = app.token_state.selected().unwrap_or(0);
                                app.token_state
                                    .select(Some(if i == 0 { rows - 1 } else { i - 1 }));
                            }
                        }
                        TokenFocus::Projects => {
                            let rows = app.tokens.by_project.len();
                            if rows > 0 {
                                let i = app.project_state.selected().unwrap_or(0);
                                app.project_state.select(Some(if i == 0 {
                                    rows - 1
                                } else {
                                    i - 1
                                }));
                            }
                        }
                    },
                },
                _ => {}
            }
        }

        if app.last_refresh.elapsed() >= app.interval {
            let (snaps, etas, sparks) = fetch_with_history(&app.settings, app.settings.cache_ttl);
            app.snaps = snaps;
            app.etas = etas;
            app.sparks = sparks;
            app.clamp_quota_selection();
            if app.tab == Tab::Tokens {
                app.raw_events = load_raw_events(&app.settings);
                app.rebuild_tokens();
            }
            app.last_refresh = Instant::now();
            app.message = format!("auto refresh · every {}s", app.interval.as_secs());
        }
    }
    Ok(())
}

impl App {
    fn clamp_quota_selection(&mut self) {
        if self.snaps.is_empty() {
            self.selected = 0;
            self.list_state.select(None);
        } else {
            self.selected = self.selected.min(self.snaps.len() - 1);
            self.list_state.select(Some(self.selected));
        }
    }

    fn clamp_token_selection(&mut self) {
        let model_rows = self.tokens.by_model.len();
        let m_sel = self.token_state.selected().unwrap_or(0);
        self.token_state.select(if model_rows == 0 {
            None
        } else {
            Some(m_sel.min(model_rows - 1))
        });

        let proj_rows = self.tokens.by_project.len();
        let p_sel = self.project_state.selected().unwrap_or(0);
        self.project_state.select(if proj_rows == 0 {
            None
        } else {
            Some(p_sel.min(proj_rows - 1))
        });
    }

    fn rebuild_tokens(&mut self) {
        self.tokens = build_token_view(
            &self.raw_events,
            &self.settings,
            self.cost_mode,
            self.model_filter.as_deref(),
            self.project_filter.as_deref(),
        );
        self.clamp_token_selection();
    }
}

fn help_for(tab: Tab) -> &'static str {
    match tab {
        Tab::Quota => {
            "q quit · r live refresh · j/k select · u left/used · p privacy · 2/tab tokens"
        }
        Tab::Tokens => {
            "q quit · r rescan · j/k select · h/l table · enter filter · bksp clear · c cost · p privacy · 1/tab quota"
        }
    }
}

fn fetch(settings: &Settings, cache_ttl: u64) -> Vec<ProviderSnapshot> {
    let mut snaps = providers::fetch_all_cached_with_profile(
        &settings.providers,
        cache_ttl,
        settings.history,
        settings.selection,
        settings.profile.as_deref(),
        None,
    );
    if settings.selection == providers::AgentSelection::Ready {
        snaps.retain(|snap| snap.status != SnapshotStatus::Auth);
    }
    if settings.providers.is_empty() && !settings.config.provider_order.is_empty() {
        let mut ordered = Vec::new();
        for name in &settings.config.provider_order {
            if let Some(pos) = snaps.iter().position(|s| s.id == name.as_str()) {
                ordered.push(snaps.remove(pos));
            }
        }
        ordered.append(&mut snaps);
        snaps = ordered;
    }
    snaps
}

fn fetch_with_history(
    settings: &Settings,
    cache_ttl: u64,
) -> (
    Vec<ProviderSnapshot>,
    HashMap<String, String>,
    HashMap<String, String>,
) {
    let snaps = fetch(settings, cache_ttl);
    let mut etas = HashMap::new();
    let mut sparks = HashMap::new();
    if let Ok(history) = crate::history::HistoryStore::open() {
        etas = crate::eta::eta_map_from_history(&history, &snaps);
        for snap in &snaps {
            if let Ok(points) = history.recent_meter_points(&snap.id, 24) {
                let mut by_meter: HashMap<String, Vec<f64>> = HashMap::new();
                for p in points {
                    by_meter.entry(p.meter_id).or_default().push(p.used_percent);
                }
                for (mid, vals) in by_meter {
                    if vals.len() >= 2 {
                        sparks.insert(
                            format!("{}/{mid}", snap.id),
                            crate::history::sparkline(&vals, 10),
                        );
                    }
                }
            }
        }
    }
    (snaps, etas, sparks)
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
        .map(|s| {
            format_detail(
                s,
                app.settings.display,
                app.settings.privacy,
                &app.etas,
                &app.sparks,
            )
        })
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
        render_daily_chart(f, &app.tokens.daily, app.cost_mode, chunks[2]);
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
    render_project_table(f, app, tables[1]);

    render_daily_chart(f, &app.tokens.daily, app.cost_mode, chunks[2]);
}

fn render_totals_strip(f: &mut Frame, view: &TokenView, area: Rect) {
    let row = |label: &str, t: &TokenTotals| {
        Line::from(format!(
            "{label:<6} in {:>7}   out {:>7}   cache {:>7}   ·   total {:>8}   ·   ${:>6.2}",
            fmt_tokens(t.input),
            fmt_tokens(t.output),
            fmt_tokens(t.cache()),
            fmt_tokens(t.total()),
            t.cost_usd,
        ))
    };
    let strip = Paragraph::new(vec![
        row("today", &view.today),
        row("7d", &view.week),
        row("30d", &view.month),
    ])
    .style(Style::default().fg(Color::Gray))
    .block(bordered("tokens (local days)"));
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
            let cost = if t.cost_usd > 0.0 {
                format!("${:.2}", t.cost_usd)
            } else {
                "—".into()
            };
            Row::new(vec![name.clone(), fmt_tokens(t.total()), cost, share])
                .style(Style::default().fg(Color::Gray))
        })
        .collect()
}

fn breakdown_table<'a>(
    title: impl Into<Line<'static>>,
    header: &'static str,
    rows: Vec<Row<'a>>,
    active: bool,
) -> Table<'a> {
    let border_color = if active {
        Color::White
    } else {
        Color::DarkGray
    };
    Table::new(
        rows,
        [
            Constraint::Min(8),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(5),
        ],
    )
    .header(
        Row::new(vec![header, "tokens", "$", "share"]).style(Style::default().fg(Color::DarkGray)),
    )
    .block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    )
    .row_highlight_style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("› ")
}

fn render_model_table(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = breakdown_rows(&app.tokens.by_model, app.tokens.month.total());
    let title = match &app.project_filter {
        Some(p) => format!("by model · {p}"),
        None => "by model · 30d".into(),
    };
    let table = breakdown_table(title, "model", rows, app.token_focus == TokenFocus::Models);
    f.render_stateful_widget(table, area, &mut app.token_state);
}

fn render_project_table(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = breakdown_rows(&app.tokens.by_project, app.tokens.month.total());
    let title = match &app.model_filter {
        Some(m) => format!("by project · {m}"),
        None => "by project · 30d".into(),
    };
    let table = breakdown_table(
        title,
        "project",
        rows,
        app.token_focus == TokenFocus::Projects,
    );
    f.render_stateful_widget(table, area, &mut app.project_state);
}

fn render_daily_chart(f: &mut Frame, daily: &[(String, u64)], cost_mode: bool, area: Rect) {
    let data: Vec<(&str, u64)> = daily.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    let total: u64 = daily.iter().map(|(_, v)| v).sum();
    let title = if cost_mode {
        format!("last {CHART_DAYS} days · ${:.2} (¢)", total as f64 / 100.0)
    } else {
        format!("last {CHART_DAYS} days · {}", fmt_tokens(total))
    };
    let chart = BarChart::default()
        .block(bordered(title))
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
    sparks: &HashMap<String, String>,
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
    if let Some(ms) = snap.latency_ms {
        header.push_str(&format!("  ·  {ms}ms"));
    }
    lines.push(header);
    match snap.status {
        SnapshotStatus::Ok if !snap.meters.is_empty() => {
            for m in &snap.meters {
                let fraction = match display {
                    DisplayMode::Left => m.left_percent.or_else(|| m.percent.map(|p| 1.0 - p)),
                    DisplayMode::Used => m.percent.or_else(|| m.left_percent.map(|p| 1.0 - p)),
                };
                let key = format!("{}/{}", snap.id, m.id);
                let eta = etas
                    .get(&key)
                    .map(|value| format!("  ·  eta {value}"))
                    .unwrap_or_default();
                let spark = sparks
                    .get(&key)
                    .map(|s| format!("  {s}"))
                    .unwrap_or_default();
                let reset = crate::ui::format_reset(m);
                if let Some(f) = fraction {
                    let bar = crate::ui::meter_bar(f, 14);
                    let pct = format!("{:>3.0}%", f * 100.0);
                    let unit = if m.unit.eq_ignore_ascii_case("usd") {
                        let used = m
                            .used
                            .map(|u| format!("${u:.2}"))
                            .unwrap_or_else(|| "?".into());
                        let lim = m
                            .limit
                            .map(|l| format!("${l:.2}"))
                            .unwrap_or_else(|| "∞".into());
                        format!("  {used}/{lim}")
                    } else {
                        String::new()
                    };
                    lines.push(format!(
                        "  {:<18} {bar} {pct}{unit}{reset}{eta}{spark}",
                        m.title
                    ));
                } else if let Some(value) = crate::ui::money_value(m, display) {
                    lines.push(format!("  {:<18} ${value:.2}{reset}{eta}{spark}", m.title));
                } else {
                    lines.push(format!("  {:<18} —{reset}{eta}{spark}", m.title));
                }
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

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct TokenTotals {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost_usd: f64,
}

impl TokenTotals {
    fn add_with_pricing(
        &mut self,
        event: &TokenEvent,
        overrides: &HashMap<String, ModelPriceOverride>,
    ) {
        self.input += event.input_tokens;
        self.output += event.output_tokens;
        self.cache_read += event.cache_read_tokens;
        self.cache_write += event.cache_write_tokens;
        if let Some(c) = crate::pricing::event_cost_usd_merged(event, overrides) {
            self.cost_usd += c;
        }
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
    #[cfg(test)]
    fn build(events: &[TokenEvent], now_unix: f64) -> Self {
        Self::build_with_options(
            events,
            now_unix,
            &HashMap::new(),
            false,
            None,
            None,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_options(
        events: &[TokenEvent],
        now_unix: f64,
        overrides: &HashMap<String, ModelPriceOverride>,
        cost_mode: bool,
        model_filter: Option<&str>,
        project_filter: Option<&str>,
        privacy: bool,
        utc_midnight: bool,
    ) -> Self {
        let day0 = if utc_midnight {
            day_start(now_unix)
        } else {
            crate::tokens::day_start_unix(now_unix, crate::tokens::local_offset())
        };
        let filtered: Vec<TokenEvent> = events
            .iter()
            .filter(|e| {
                let model_ok = model_filter.is_none_or(|m| {
                    e.model
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("unknown")
                        == m
                });
                let proj_name = e
                    .project
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|p| {
                        if privacy {
                            crate::privacy::redact_account(p)
                        } else {
                            p.to_string()
                        }
                    })
                    .unwrap_or_else(|| "unknown".into());
                let proj_ok = project_filter.is_none_or(|p| proj_name == p);
                model_ok && proj_ok
            })
            .cloned()
            .map(|mut e| {
                if privacy && let Some(p) = e.project.as_mut() {
                    *p = crate::privacy::redact_account(p);
                }
                e
            })
            .collect();
        let mut by_model = breakdown_by(&filtered, |e| e.model.as_deref(), overrides);
        let mut by_project = breakdown_by(&filtered, |e| e.project.as_deref(), overrides);
        if cost_mode {
            by_model.sort_by(|a, b| {
                b.1.cost_usd
                    .partial_cmp(&a.1.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.1.total().cmp(&a.1.total()))
            });
            by_project.sort_by(|a, b| {
                b.1.cost_usd
                    .partial_cmp(&a.1.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.1.total().cmp(&a.1.total()))
            });
        }
        Self {
            events: filtered.len(),
            today: totals_in_range(&filtered, day0, overrides),
            week: totals_in_range(&filtered, day0 - 6.0 * DAY_SECS, overrides),
            month: totals_in_range(
                &filtered,
                day0 - (TOKENS_WINDOW_DAYS - 1.0) * DAY_SECS,
                overrides,
            ),
            by_model,
            by_project,
            daily: daily_totals(&filtered, CHART_DAYS, now_unix, overrides, cost_mode),
        }
    }
}

fn load_raw_events(settings: &Settings) -> Vec<TokenEvent> {
    let now = now_unix();
    let from = crate::tokens::day_start_unix(now, crate::tokens::local_offset())
        - (TOKENS_WINDOW_DAYS - 1.0) * DAY_SECS;
    let mut events = TokenStore::open()
        .ok()
        .and_then(|store| store.events_since(Some(from), None).ok())
        .unwrap_or_default();
    let ids = providers::selected_ledger_ids(&settings.providers, settings.selection);
    events.retain(|event| ids.contains(&event.provider.as_str()));
    events
}

fn build_token_view(
    events: &[TokenEvent],
    settings: &Settings,
    cost_mode: bool,
    model_filter: Option<&str>,
    project_filter: Option<&str>,
) -> TokenView {
    TokenView::build_with_options(
        events,
        now_unix(),
        &settings.config.pricing,
        cost_mode,
        model_filter,
        project_filter,
        settings.privacy,
        false,
    )
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

fn totals_in_range(
    events: &[TokenEvent],
    from_unix: f64,
    overrides: &HashMap<String, ModelPriceOverride>,
) -> TokenTotals {
    let mut totals = TokenTotals::default();
    for event in events.iter().filter(|e| e.ts_unix >= from_unix) {
        totals.add_with_pricing(event, overrides);
    }
    totals
}

fn breakdown_by<'a>(
    events: &'a [TokenEvent],
    key: impl Fn(&'a TokenEvent) -> Option<&'a str>,
    overrides: &HashMap<String, ModelPriceOverride>,
) -> Vec<(String, TokenTotals)> {
    let mut map: HashMap<String, TokenTotals> = HashMap::new();
    for event in events {
        let name = key(event).filter(|s| !s.is_empty()).unwrap_or("unknown");
        map.entry(name.to_string())
            .or_default()
            .add_with_pricing(event, overrides);
    }
    let mut rows: Vec<(String, TokenTotals)> = map.into_iter().collect();
    rows.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Per-day totals for the last `days` UTC days, oldest first.
fn daily_totals(
    events: &[TokenEvent],
    days: usize,
    now_unix: f64,
    overrides: &HashMap<String, ModelPriceOverride>,
    cost_mode: bool,
) -> Vec<(String, u64)> {
    let last = (now_unix / DAY_SECS).floor() as i64;
    let first = last - days as i64 + 1;
    let mut sums = vec![0u64; days.max(1)];
    for event in events {
        let idx = (event.ts_unix / DAY_SECS).floor() as i64;
        if idx >= first && idx <= last {
            let val = if cost_mode {
                (crate::pricing::event_cost_usd_merged(event, overrides).unwrap_or(0.0) * 100.0)
                    .round() as u64
            } else {
                event_total(event)
            };
            sums[(idx - first) as usize] += val;
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
        let overrides = HashMap::new();
        let t = totals_in_range(&events, 200.0, &overrides);
        assert_eq!(t.input, 20);
        assert_eq!(t.output, 10);
        assert_eq!(t.cache(), 10);
        assert_eq!(t.total(), 40);
        assert_eq!(totals_in_range(&events, 0.0, &overrides).total(), 120);
    }

    #[test]
    fn breakdown_sorts_desc_and_labels_missing() {
        let events = vec![
            event("codex", Some("small"), Some("a"), 0.0, 10),
            event("codex", Some("big"), Some("a"), 0.0, 90),
            event("codex", Some("big"), None, 0.0, 20),
            event("codex", None, None, 0.0, 5),
        ];
        let overrides = HashMap::new();
        let models = breakdown_by(&events, |e| e.model.as_deref(), &overrides);
        assert_eq!(models[0].0, "big");
        assert_eq!(models[0].1.total(), 110);
        assert_eq!(models[1].0, "small");
        assert_eq!(models[2].0, "unknown");
        let projects = breakdown_by(&events, |e| e.project.as_deref(), &overrides);
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
        let overrides = HashMap::new();
        let daily = daily_totals(&events, 14, now, &overrides, false);
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
            event("codex", Some("m"), None, 86_400.0 * -20.0, 8),
        ];
        let view = TokenView::build(&events, now);
        assert_eq!(view.today.total(), 16);
        assert_eq!(view.week.total(), 24);
        assert_eq!(view.month.total(), 32);
        // breakdowns cover every loaded event
        assert_eq!(view.by_model[0].1.total(), 40);
    }

    #[test]
    fn token_view_privacy_and_filter() {
        let now = 86_400.0 * 20.0 + 100.0;
        let events = vec![
            event(
                "claude",
                Some("claude-sonnet-4-5"),
                Some("secret-repo"),
                now - 10.0,
                1_000_000,
            ),
            event("codex", Some("gpt-5"), Some("other-repo"), now - 10.0, 100),
        ];
        let view = TokenView::build_with_options(
            &events,
            now,
            &HashMap::new(),
            true,
            Some("claude-sonnet-4-5"),
            None,
            true,
            true,
        );
        assert_eq!(view.events, 1);
        assert!(view.today.cost_usd > 1.0);
        assert_ne!(view.by_project[0].0, "secret-repo");
    }

    // ---- headless rendering ----

    fn test_settings() -> Settings {
        Settings {
            providers: vec![],
            selection: providers::AgentSelection::Ready,
            display: DisplayMode::Left,
            quiet: true,
            compact: false,
            compact_cost: false,
            template: None,
            privacy: false,
            json: false,
            pretty: false,
            format: None,
            alert: None,
            alert_eta: None,
            alert_balance_usd: None,
            notify: false,
            cache_ttl: 0,
            history: false,
            watch_interval: 60,
            profile: None,
            active_project_config: None,
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
            sparks: HashMap::new(),
            raw_events: events.to_vec(),
            tokens: TokenView::build(events, now),
            token_state: TableState::default().with_selected(Some(0)),
            project_state: TableState::default().with_selected(Some(0)),
            token_focus: TokenFocus::Models,
            cost_mode: false,
            model_filter: None,
            project_filter: None,
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
