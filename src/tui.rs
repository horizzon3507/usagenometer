//! Interactive TUI (ratatui) — B&W live meters.

use std::io::{self,Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::cli::DisplayMode;
use crate::config::Settings;
use crate::providers::{self, types::ProviderSnapshot};
use crate::providers::types::SnapshotStatus;

struct App {
    snaps: Vec<ProviderSnapshot>,
    selected: usize,
    list_state: ListState,
    last_refresh: Instant,
    interval: Duration,
    message: String,
    settings: Settings,
}

pub fn run(settings: &Settings) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, settings);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, settings: &Settings) -> Result<()> {
    let snaps = fetch(settings);
    let mut app = App {
        snaps,
        selected: 0,
        list_state: ListState::default().with_selected(Some(0)),
        last_refresh: Instant::now(),
        interval: Duration::from_secs(settings.watch_interval.max(5)),
        message: "q quit · r refresh · j/k or ↑/↓ select".into(),
        settings: settings.clone(),
    };

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = app
            .interval
            .checked_sub(app.last_refresh.elapsed())
            .unwrap_or(Duration::from_millis(50));
        if event::poll(timeout.min(Duration::from_millis(250)))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') => {
                        app.snaps = fetch(&app.settings);
                        app.last_refresh = Instant::now();
                        app.message = "refreshed".into();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !app.snaps.is_empty() {
                            app.selected = (app.selected + 1) % app.snaps.len();
                            app.list_state.select(Some(app.selected));
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if !app.snaps.is_empty() {
                            app.selected = if app.selected == 0 {
                                app.snaps.len() - 1
                            } else {
                                app.selected - 1
                            };
                            app.list_state.select(Some(app.selected));
                        }
                    }
                    _ => {}
                }
            }
        }

        if app.last_refresh.elapsed() >= app.interval {
            app.snaps = fetch(&app.settings);
            app.last_refresh = Instant::now();
            app.message = format!("auto refresh · every {}s", app.interval.as_secs());
        }
    }
    Ok(())
}

fn fetch(settings: &Settings) -> Vec<ProviderSnapshot> {
    providers::fetch_all_cached(&settings.providers, settings.cache_ttl, settings.history)
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Percentage(40),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(f.area());

    let title = Paragraph::new("◈ usagenometer tui")
        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(title, chunks[0]);

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
        .block(
            Block::default()
                .title("providers")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
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
        .map(|s| format_detail(s, app.settings.display, app.settings.privacy))
        .unwrap_or_else(|| "no providers".into());
    let detail_w = Paragraph::new(detail)
        .style(Style::default().fg(Color::Gray))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title("detail")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    f.render_widget(detail_w, chunks[2]);

    let help = Paragraph::new(app.message.as_str())
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(help, chunks[3]);
}

fn format_detail(snap: &ProviderSnapshot, display: DisplayMode, privacy: bool) -> String {
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
                lines.push(format!("  {:<20} {pct}", m.title));
            }
        }
        _ => {
            let note = snap.error.as_deref().unwrap_or(snap.status.as_str());
            lines.push(format!("  {} {note}", snap.status.as_str()));
        }
    }
    lines.join("\n")
}
