use std::collections::HashMap;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};
use tokio::sync::mpsc;

use crate::events::{AppEvent, LogEntry, LogLevel};

const MAX_LOG_LINES: usize = 200;

/// A single active (or recently stopped) forward shown in the table.
#[derive(Debug, Clone)]
pub struct ForwardRow {
    pub remote_port: u16,
    pub local_port: u16,
    pub label: String,
    pub status: ForwardStatus,
    pub started_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardStatus {
    Active,
    Stopped,
}

/// Complete TUI application state.
pub struct AppState {
    /// Ordered list of forwards (active first, then recently stopped).
    forwards: Vec<ForwardRow>,
    /// Index map remote_port → forwards index for O(1) updates.
    index: HashMap<u16, usize>,
    /// Circular log buffer.
    logs: Vec<LogEntry>,
    /// Table scroll state.
    table_state: TableState,
    /// Log scroll offset (lines from bottom).
    log_scroll: u16,
    /// SSH host shown in the title.
    pub host: String,
    /// Last poll status string.
    last_poll: String,
    /// Whether the app should exit.
    pub should_quit: bool,
}

impl AppState {
    pub fn new(host: String) -> Self {
        Self {
            forwards: Vec::new(),
            index: HashMap::new(),
            logs: Vec::new(),
            table_state: TableState::default(),
            log_scroll: 0,
            host,
            last_poll: "waiting for first poll…".into(),
            should_quit: false,
        }
    }

    // ── Event application ────────────────────────────────────────────────────

    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::ForwardStarted {
                remote_port,
                local_port,
                label,
            } => {
                let row = ForwardRow {
                    remote_port,
                    local_port,
                    label: label.unwrap_or_default(),
                    status: ForwardStatus::Active,
                    started_at: SystemTime::now(),
                };
                if let Some(&idx) = self.index.get(&remote_port) {
                    self.forwards[idx] = row;
                } else {
                    let idx = self.forwards.len();
                    self.index.insert(remote_port, idx);
                    self.forwards.push(row);
                }
                self.push_log(
                    LogLevel::Info,
                    format!("forward started  :{remote_port} → localhost:{local_port}"),
                );
            }
            AppEvent::ForwardStopped {
                remote_port,
                reason,
            } => {
                if let Some(&idx) = self.index.get(&remote_port) {
                    self.forwards[idx].status = ForwardStatus::Stopped;
                }
                self.push_log(
                    LogLevel::Warn,
                    format!("forward stopped  :{remote_port}  ({reason})"),
                );
            }
            AppEvent::ForwardDied { remote_port } => {
                if let Some(&idx) = self.index.get(&remote_port) {
                    self.forwards[idx].status = ForwardStatus::Stopped;
                }
                self.push_log(
                    LogLevel::Error,
                    format!("forward died unexpectedly  :{remote_port}"),
                );
            }
            AppEvent::PollOk { discovered } => {
                self.last_poll = format!("last poll ok — {discovered} remote port(s) listening");
            }
            AppEvent::PollError { message } => {
                self.last_poll = format!("poll error: {message}");
                self.push_log(LogLevel::Error, format!("poll error: {message}"));
            }
            AppEvent::Log { level, message } => {
                self.push_log(level, message);
            }
            AppEvent::Shutdown => {
                self.should_quit = true;
            }
        }
    }

    fn push_log(&mut self, level: LogLevel, message: String) {
        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.remove(0);
        }
        self.logs.push(LogEntry::new(level, message));
    }

    // ── Keyboard handling ────────────────────────────────────────────────────

    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match (code, modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.next_row();
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.prev_row();
            }
            (KeyCode::PageDown, _) => {
                self.log_scroll = self.log_scroll.saturating_sub(5);
            }
            (KeyCode::PageUp, _) => {
                self.log_scroll = self.log_scroll.saturating_add(5);
            }
            (KeyCode::Char('G'), _) => {
                self.log_scroll = 0; // jump to newest logs
            }
            _ => {}
        }
    }

    fn next_row(&mut self) {
        let len = self.forwards.len();
        if len == 0 {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map(|i| (i + 1).min(len - 1))
            .unwrap_or(0);
        self.table_state.select(Some(i));
    }

    fn prev_row(&mut self) {
        let i = self
            .table_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.table_state.select(Some(i));
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();

    // Outer vertical split: table | logs | status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),     // port table (expands)
            Constraint::Length(10), // log panel (fixed height)
            Constraint::Length(1),  // status bar
        ])
        .split(area);

    render_forwards_table(frame, state, chunks[0]);
    render_log_panel(frame, state, chunks[1]);
    render_status_bar(frame, state, chunks[2]);
}

fn render_forwards_table(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let header_cells = ["Remote Port", "Local Port", "Label", "Status", "Uptime"]
        .iter()
        .map(|h| {
            Cell::from(*h).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        });
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let now = SystemTime::now();
    let rows: Vec<Row> = state
        .forwards
        .iter()
        .map(|fwd| {
            let (status_text, status_color) = match fwd.status {
                ForwardStatus::Active => ("active", Color::Green),
                ForwardStatus::Stopped => ("stopped", Color::Red),
            };
            let uptime = format_uptime(now.duration_since(fwd.started_at).unwrap_or_default());
            Row::new(vec![
                Cell::from(format!(":{}", fwd.remote_port)),
                Cell::from(format!(":{}", fwd.local_port)),
                Cell::from(fwd.label.clone()),
                Cell::from(status_text).style(Style::default().fg(status_color)),
                Cell::from(uptime),
            ])
        })
        .collect();

    let active_count = state
        .forwards
        .iter()
        .filter(|f| f.status == ForwardStatus::Active)
        .count();

    let title = format!(
        " port-shadow — {} — {} active forward(s) ",
        state.host, active_count
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(13), // remote
            Constraint::Length(12), // local
            Constraint::Min(20),    // label
            Constraint::Length(9),  // status
            Constraint::Length(10), // uptime
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut state.table_state);
}

fn render_log_panel(frame: &mut Frame, state: &AppState, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // minus borders

    let total = state.logs.len();
    let scroll = state.log_scroll as usize;

    // We display the last `inner_height` lines, offset by scroll
    let start = total.saturating_sub(inner_height).saturating_sub(scroll);
    let end = total.saturating_sub(scroll);
    let visible = &state.logs[start..end];

    let lines: Vec<Line> = visible
        .iter()
        .map(|entry| {
            let time_str = format_time(entry.time);
            let (level_str, level_color) = match entry.level {
                LogLevel::Info => ("INFO ", Color::Cyan),
                LogLevel::Warn => ("WARN ", Color::Yellow),
                LogLevel::Error => ("ERROR", Color::Red),
            };
            Line::from(vec![
                Span::styled(format!("{time_str} "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{level_str} "),
                    Style::default()
                        .fg(level_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(entry.message.clone()),
            ])
        })
        .collect();

    let scroll_hint = if state.log_scroll > 0 {
        format!(" logs [↑{} lines] ", state.log_scroll)
    } else {
        " logs ".into()
    };

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(scroll_hint)
            .title_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(paragraph, area);
}

fn render_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let left = Span::styled(
        format!(" {} ", state.last_poll),
        Style::default().fg(Color::White).bg(Color::DarkGray),
    );
    let right = Span::styled(
        " q/^C quit  ↑↓/jk select  PgUp/PgDn scroll logs  G newest ",
        Style::default().fg(Color::DarkGray).bg(Color::Black),
    );

    let bar_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(62)])
        .split(area);

    frame.render_widget(Paragraph::new(Line::from(left)), bar_chunks[0]);
    frame.render_widget(Paragraph::new(Line::from(right)), bar_chunks[1]);
}

// ── Terminal lifecycle ───────────────────────────────────────────────────────

/// Set up the terminal for TUI use.
pub fn init_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its normal state.
pub fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// ── TUI run loop ─────────────────────────────────────────────────────────────

/// Runs the TUI in the current task.
/// Receives `AppEvent`s from the polling loop via `rx`.
/// Returns when the user quits or a `Shutdown` event is received.
pub async fn run_tui(
    mut rx: mpsc::UnboundedReceiver<AppEvent>,
    host: String,
) -> anyhow::Result<()> {
    let mut terminal = init_terminal()?;

    // Install a panic hook that restores the terminal before printing the panic
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        default_hook(info);
    }));

    let mut state = AppState::new(host);
    let tick = Duration::from_millis(100);

    loop {
        terminal.draw(|f| render(f, &mut state))?;

        // Poll crossterm input events with a short timeout so we keep
        // re-drawing even if no key is pressed (the poll loop may send events).
        if crossterm::event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                state.on_key(key.code, key.modifiers);
            }
        }

        // Drain all pending app events
        loop {
            match rx.try_recv() {
                Ok(ev) => state.apply(ev),
                Err(_) => break,
            }
        }

        if state.should_quit {
            break;
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn format_uptime(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

fn format_time(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}
