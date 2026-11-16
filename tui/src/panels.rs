//! Pure widget constructors: given app state, build the ratatui
//! widgets for each screen. No I/O, no facade access — rendering
//! can never change gate state.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, RunMode, RunOutcome, Screen};
use crate::facade::StatusView;
use crate::{DEVICES_SHOWN_MAX, TRIGGER_EMPTY_REASON, TRIGGER_RULES};

/// Maximum lines of log rendered at once.
pub const LOG_RENDER_LINES_MAX: usize = 200;
/// Maximum characters in the overview note line.
pub const NOTE_CHARS_MAX: usize = 160;

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('~');
        out
    }
}

/// The arm banner: the one line that must never be misread.
/// `None` (unknown) renders as UNKNOWN, never as disarmed.
pub fn arm_banner(status: &StatusView) -> Line<'static> {
    match status {
        StatusView::Known(s) if s.armed => Line::from(Span::styled(
            "*** MASTER: ARMED *** output may fire through the server decide() gate",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        StatusView::Known(_) => Line::from(Span::styled(
            "MASTER: DISARMED (default) — no output can fire",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        StatusView::Unknown(reason) => Line::from(Span::styled(
            format!("MASTER: UNKNOWN — server unreachable ({reason}); last-known state discarded"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
    }
}

/// Overview screen body lines.
pub fn overview_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![arm_banner(&app.status)];
    match &app.status {
        StatusView::Known(s) => {
            lines.push(Line::from(format!("server: {}", s.server_label)));
            lines.push(Line::from(format!(
                "ceiling: {} / {} {}",
                s.ceiling_display,
                crate::CEILING_MAX,
                if s.ceiling_anomaly {
                    format!(
                        "!! ANOMALY: server reported {} — display clamped, do not trust server",
                        s.ceiling_reported
                    )
                } else {
                    String::new()
                }
            )));
            lines.push(Line::from(format!("output active: {}", s.output_active)));
            lines.push(Line::from(format!(
                "watchdog: {}s window, {} trip(s), session health: {}",
                s.watchdog_secs, s.watchdog_trips, s.session_health
            )));
            lines.push(Line::from(format!(
                "server API (opt-in): {}",
                if s.server_api_enabled {
                    "ENABLED"
                } else {
                    "disabled (default)"
                }
            )));
            lines.push(Line::from("providers:"));
            for (id, state, available, note) in &s.providers {
                lines.push(Line::from(format!(
                    "  {id}: state={state} available={available} — {}",
                    truncate(note, NOTE_CHARS_MAX)
                )));
            }
        }
        StatusView::Unknown(_) => {
            lines.push(Line::from(
                "Gate state is UNKNOWN. Runs are blocked until the server answers again.",
            ));
        }
    }
    lines.push(Line::from(format!(
        "mode: {} | STOP: press s from any screen (synchronous lovense_stop)",
        match app.mode {
            RunMode::Plan => "PLAN (default)",
            RunMode::Auto => "AUTO",
        }
    )));
    lines
}

/// Workflows screen body lines.
pub fn workflow_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![arm_banner(&app.status)];
    lines.push(Line::from(format!(
        "mode: {} — plan stages the exact call for approval; auto executes \
         declared operations, still only what the server decide() gate allows",
        match app.mode {
            RunMode::Plan => "PLAN (default)",
            RunMode::Auto => "AUTO",
        }
    )));
    if let Some(devices) = &app.devices {
        let count = devices["count"].as_u64().unwrap_or(0);
        lines.push(Line::from(format!("devices (ache_devices): {count}")));
        if let Some(list) = devices["devices"].as_array() {
            for device in list.iter().take(DEVICES_SHOWN_MAX) {
                lines.push(Line::from(format!(
                    "  {} [{}] {}",
                    device["id"].as_str().unwrap_or("?"),
                    device["provider"].as_str().unwrap_or("?"),
                    device["name"].as_str().unwrap_or("?")
                )));
            }
        }
    } else {
        lines.push(Line::from("devices: not fetched yet (press d)"));
    }
    lines.push(Line::from("declared workflows:"));
    for (idx, run) in crate::app::declared_workflows().iter().enumerate() {
        lines.push(Line::from(format!(
            "  {}. {} {}",
            idx + 1,
            run.tool,
            run.args
        )));
    }
    if let Some(staged) = &app.staged {
        lines.push(Line::from(Span::styled(
            format!(
                "STAGED (not sent): {} {} — y=approve, n=abort",
                staged.tool, staged.args
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }
    match &app.outcome {
        RunOutcome::None => {}
        RunOutcome::Done(text) => lines.push(Line::from(format!("last run: {text}"))),
        RunOutcome::Refused(text) => lines.push(Line::from(Span::styled(
            format!("last run: REFUSED by server gate — {text} (not retried)"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))),
        RunOutcome::Failed(text) => {
            lines.push(Line::from(format!("last run: FAILED — {text}")));
        }
    }
    lines
}

/// Log/Live screen body lines (bounded tail).
pub fn log_lines(app: &App) -> Vec<Line<'static>> {
    let start = app.log.len().saturating_sub(LOG_RENDER_LINES_MAX);
    app.log[start..]
        .iter()
        .map(|line| Line::from(truncate(line, NOTE_CHARS_MAX * 2)))
        .collect()
}

/// Fleet panel body lines: rules empty by design, reason in copy.
pub fn fleet_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("trigger rules declared: {}", TRIGGER_RULES.len())),
        Line::from(TRIGGER_EMPTY_REASON.to_string()),
        Line::from("fleet members: reachability is read from each member's own server; rung 1 lists ache only".to_string()),
        arm_banner(&app.status),
    ];
    lines.push(Line::from("audit log tail:"));
    let start = app.log.len().saturating_sub(20);
    for line in &app.log[start..] {
        lines.push(Line::from(truncate(line, NOTE_CHARS_MAX)));
    }
    lines
}

/// Render the full frame for `app` into `area`.
pub fn render(app: &App, frame: &mut ratatui::Frame<'_>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    let tabs: Vec<String> = Screen::ALL
        .iter()
        .map(|s| {
            if *s == app.screen {
                format!("[{}]", s.label())
            } else {
                s.label().to_string()
            }
        })
        .collect();
    let header = Paragraph::new(format!(
        "ache-tui safety console | {} | s=STOP  Tab=screen  m=mode  r=refresh  q=quit",
        tabs.join(" ")
    ))
    .block(Block::default().borders(Borders::ALL).title("ache"));
    frame.render_widget(header, chunks[0]);
    let lines = match app.screen {
        Screen::Overview => overview_lines(app),
        Screen::Workflows => workflow_lines(app),
        Screen::Log => log_lines(app),
        Screen::Fleet => fleet_lines(app),
    };
    let body = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(app.screen.label()),
    );
    frame.render_widget(body, chunks[1]);
}
