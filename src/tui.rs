//! Interactive terminal workspace for browsing and deleting caches.

use std::collections::HashSet;
use std::env;
use std::io::{IsTerminal, Stdout};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use cacheferret::{
    CacheCandidate, CacheScope, CleanReport, DiscoveryOptions, Error, ScanReport, ScopeFilter,
    clean_candidates, discover, format_bytes, refresh_candidate,
};
use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{Frame, Terminal};

const RECENT_DAYS: u64 = 7;
const LARGE_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Palette {
    bg: Color,
    surface: Color,
    surface_active: Color,
    text: Color,
    muted: Color,
    accent: Color,
    info: Color,
    success: Color,
    danger: Color,
}

impl Palette {
    const TRUECOLOR: Self = Self {
        bg: Color::Rgb(7, 18, 30),
        surface: Color::Rgb(13, 31, 49),
        surface_active: Color::Rgb(24, 50, 72),
        text: Color::Rgb(236, 240, 239),
        muted: Color::Rgb(151, 169, 178),
        accent: Color::Rgb(211, 139, 86),
        info: Color::Rgb(65, 196, 201),
        success: Color::Rgb(104, 211, 145),
        danger: Color::Rgb(243, 113, 116),
    };

    const ANSI256: Self = Self {
        bg: Color::Indexed(233),
        surface: Color::Indexed(234),
        surface_active: Color::Indexed(24),
        text: Color::Indexed(255),
        muted: Color::Indexed(109),
        accent: Color::Indexed(173),
        info: Color::Indexed(44),
        success: Color::Indexed(114),
        danger: Color::Indexed(210),
    };

    const ANSI16: Self = Self {
        bg: Color::Black,
        surface: Color::Black,
        surface_active: Color::DarkGray,
        text: Color::White,
        muted: Color::Gray,
        accent: Color::Yellow,
        info: Color::Cyan,
        success: Color::Green,
        danger: Color::Red,
    };

    const PLAIN: Self = Self {
        bg: Color::Reset,
        surface: Color::Reset,
        surface_active: Color::Reset,
        text: Color::Reset,
        muted: Color::Reset,
        accent: Color::Reset,
        info: Color::Reset,
        success: Color::Reset,
        danger: Color::Reset,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiPreferences {
    palette: Palette,
    unicode: bool,
    animate: bool,
}

impl UiPreferences {
    fn detect() -> Self {
        Self::from_values(
            env::var_os("NO_COLOR").is_some(),
            env::var("TERM").ok().as_deref(),
            env::var("COLORTERM").ok().as_deref(),
            env::var("LC_ALL")
                .or_else(|_| env::var("LC_CTYPE"))
                .or_else(|_| env::var("LANG"))
                .ok()
                .as_deref(),
            env::var_os("CACHEFERRET_ASCII").is_some(),
            env::var_os("CACHEFERRET_REDUCE_MOTION").is_some(),
        )
    }

    fn from_values(
        no_color: bool,
        term: Option<&str>,
        colorterm: Option<&str>,
        locale: Option<&str>,
        force_ascii: bool,
        reduce_motion: bool,
    ) -> Self {
        let dumb = term.is_some_and(|value| value.eq_ignore_ascii_case("dumb"));
        let truecolor = colorterm.is_some_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) || term.is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("truecolor") || value.contains("24bit")
        });
        let ansi256 = term.is_some_and(|value| value.to_ascii_lowercase().contains("256color"));
        let utf8 = locale.is_none_or(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        });
        Self {
            palette: if no_color || dumb {
                Palette::PLAIN
            } else if truecolor {
                Palette::TRUECOLOR
            } else if ansi256 {
                Palette::ANSI256
            } else {
                Palette::ANSI16
            },
            unicode: !force_ascii && !dumb && utf8,
            animate: !reduce_motion && !dumb,
        }
    }

    #[cfg(test)]
    const fn rich() -> Self {
        Self {
            palette: Palette::TRUECOLOR,
            unicode: true,
            animate: true,
        }
    }

    fn separator(self) -> &'static str {
        if self.unicode { " · " } else { " | " }
    }

    fn border(self) -> BorderType {
        if self.unicode {
            BorderType::Rounded
        } else {
            BorderType::Plain
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub roots: Vec<PathBuf>,
    pub scope: ScopeFilter,
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Scanning,
    Ready,
    Reviewing,
    Confirming,
    Cleaning,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewScope {
    All,
    Project,
    Global,
}

impl ViewScope {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Project,
            Self::Project => Self::Global,
            Self::Global => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Project => "Project",
            Self::Global => "Global",
        }
    }

    fn includes(self, scope: CacheScope) -> bool {
        matches!(self, Self::All)
            || matches!((self, scope), (Self::Project, CacheScope::Project))
            || matches!((self, scope), (Self::Global, CacheScope::Global))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortOrder {
    Size,
    Age,
    Name,
}

impl SortOrder {
    fn next(self) -> Self {
        match self {
            Self::Size => Self::Age,
            Self::Age => Self::Name,
            Self::Name => Self::Size,
        }
    }

    fn label(self, unicode: bool) -> &'static str {
        match (self, unicode) {
            (Self::Size, true) => "Size ↓",
            (Self::Age, true) => "Age ↓",
            (Self::Name, true) => "Name ↑",
            (Self::Size, false) => "Size desc",
            (Self::Age, false) => "Age desc",
            (Self::Name, false) => "Name asc",
        }
    }
}

enum WorkerMessage {
    Scan(Result<ScanReport, Error>),
    Review {
        result: Result<CacheCandidate, Error>,
        confirmed: bool,
    },
    Clean(CleanReport),
}

struct App {
    options: Options,
    phase: Phase,
    candidates: Vec<CacheCandidate>,
    visible: Vec<usize>,
    warnings: Vec<String>,
    cursor: usize,
    query: String,
    editing_filter: bool,
    view_scope: ViewScope,
    sort: SortOrder,
    show_help: bool,
    scan_started: Instant,
    pending_delete: Option<CacheCandidate>,
    deleting: Option<PathBuf>,
    error: Option<String>,
    toast: Option<(String, Instant)>,
    should_quit: bool,
    ui: UiPreferences,
}

impl App {
    fn new(options: Options) -> Self {
        Self::with_ui(options, UiPreferences::detect())
    }

    fn with_ui(options: Options, ui: UiPreferences) -> Self {
        Self {
            options,
            phase: Phase::Scanning,
            candidates: Vec::new(),
            visible: Vec::new(),
            warnings: Vec::new(),
            cursor: 0,
            query: String::new(),
            editing_filter: false,
            view_scope: ViewScope::All,
            sort: SortOrder::Size,
            show_help: false,
            scan_started: Instant::now(),
            pending_delete: None,
            deleting: None,
            error: None,
            toast: None,
            should_quit: false,
            ui,
        }
    }

    fn rebuild_visible(&mut self) {
        let query = self.query.to_ascii_lowercase();
        let mut indices: Vec<usize> = self
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| self.view_scope.includes(candidate.scope))
            .filter(|(_, candidate)| {
                query.is_empty()
                    || candidate.kind.to_ascii_lowercase().contains(&query)
                    || candidate.ecosystem.to_ascii_lowercase().contains(&query)
                    || candidate
                        .path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        match self.sort {
            SortOrder::Size => indices.sort_by(|left, right| {
                self.candidates[*right]
                    .bytes
                    .cmp(&self.candidates[*left].bytes)
            }),
            SortOrder::Age => indices.sort_by(|left, right| {
                self.candidates[*right]
                    .age_days
                    .unwrap_or(0)
                    .cmp(&self.candidates[*left].age_days.unwrap_or(0))
            }),
            SortOrder::Name => indices.sort_by(|left, right| {
                self.candidates[*left]
                    .path
                    .cmp(&self.candidates[*right].path)
            }),
        }
        self.visible = indices;
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
    }

    fn focused_index(&self) -> Option<usize> {
        self.visible.get(self.cursor).copied()
    }

    fn move_cursor(&mut self, amount: isize) {
        let last = self.visible.len().saturating_sub(1);
        self.cursor = if amount.is_negative() {
            self.cursor.saturating_sub(amount.unsigned_abs())
        } else {
            self.cursor.saturating_add(amount as usize).min(last)
        };
    }

    fn begin_scan(&mut self, tx: &Sender<WorkerMessage>) {
        self.phase = Phase::Scanning;
        self.scan_started = Instant::now();
        self.pending_delete = None;
        self.deleting = None;
        self.error = None;
        let options = self.options.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let result = discover(&DiscoveryOptions {
                roots: options.roots,
                scope: options.scope,
                kinds: options.kinds,
                protect_days: 0,
            });
            let _ = tx.send(WorkerMessage::Scan(result));
        });
    }

    fn handle_worker(&mut self, message: WorkerMessage, tx: &Sender<WorkerMessage>) {
        match message {
            WorkerMessage::Scan(Ok(report)) => {
                self.candidates = report.candidates;
                self.warnings = report.warnings;
                self.phase = Phase::Ready;
                self.rebuild_visible();
                if self.candidates.is_empty() {
                    self.toast = Some((
                        "No caches found — your disk is already tidy".to_owned(),
                        Instant::now(),
                    ));
                }
            }
            WorkerMessage::Scan(Err(error)) => {
                self.error = Some(error.to_string());
                self.phase = Phase::Failed;
            }
            WorkerMessage::Review {
                result: Ok(candidate),
                confirmed,
            } => {
                if let Some(current) = self
                    .candidates
                    .iter_mut()
                    .find(|current| current.path == candidate.path)
                {
                    *current = candidate.clone();
                }
                self.rebuild_visible();
                if confirmed || risk_reasons(&candidate).is_empty() {
                    self.delete_candidate(candidate, tx);
                } else {
                    self.pending_delete = Some(candidate);
                    self.phase = Phase::Confirming;
                }
            }
            WorkerMessage::Review {
                result: Err(error), ..
            } => {
                self.phase = Phase::Ready;
                self.toast = Some((
                    format!("Cache changed; scan again · {error}"),
                    Instant::now(),
                ));
            }
            WorkerMessage::Clean(report) => {
                let cleaned: HashSet<&PathBuf> = report.cleaned_paths.iter().collect();
                self.candidates
                    .retain(|candidate| !cleaned.contains(&candidate.path));
                self.deleting = None;
                self.phase = Phase::Ready;
                self.toast = Some((cleanup_message(&report), Instant::now()));
                self.rebuild_visible();
            }
        }
    }

    fn begin_delete(&mut self, tx: &Sender<WorkerMessage>) {
        let Some(index) = self.focused_index() else {
            return;
        };
        let candidate = self.candidates[index].clone();
        if !candidate.cleanable {
            self.toast = Some(("This cache is scan-only".to_owned(), Instant::now()));
            return;
        }
        self.review_candidate(candidate, false, tx);
    }

    fn confirm_delete(&mut self, tx: &Sender<WorkerMessage>) {
        let Some(candidate) = self.pending_delete.take() else {
            self.phase = Phase::Ready;
            return;
        };
        self.review_candidate(candidate, true, tx);
    }

    fn cancel_delete(&mut self) {
        self.pending_delete = None;
        self.phase = Phase::Ready;
    }

    fn review_candidate(
        &mut self,
        candidate: CacheCandidate,
        confirmed: bool,
        tx: &Sender<WorkerMessage>,
    ) {
        self.phase = Phase::Reviewing;
        self.scan_started = Instant::now();
        self.deleting = Some(candidate.path.clone());
        let tx = tx.clone();
        std::thread::spawn(move || {
            let result = refresh_candidate(&candidate, RECENT_DAYS);
            let _ = tx.send(WorkerMessage::Review { result, confirmed });
        });
    }

    fn delete_candidate(&mut self, candidate: CacheCandidate, tx: &Sender<WorkerMessage>) {
        self.phase = Phase::Cleaning;
        self.scan_started = Instant::now();
        self.deleting = Some(candidate.path.clone());
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(WorkerMessage::Clean(clean_candidates(&[candidate], false)));
        });
    }
}

fn risk_reasons(candidate: &CacheCandidate) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    match candidate.age_days {
        Some(days) if days < RECENT_DAYS => reasons.push("recent"),
        None => reasons.push("unknown age"),
        _ => {}
    }
    if candidate.bytes >= LARGE_CACHE_BYTES {
        reasons.push("large");
    }
    if candidate.scope == CacheScope::Global {
        reasons.push("shared");
    }
    if candidate.network_restore {
        reasons.push("download restore");
    }
    reasons
}

fn cleanup_message(report: &CleanReport) -> String {
    if report.cleaned == 1 {
        format!(
            "Deleted cache · {} reclaimed",
            format_bytes(report.bytes_reclaimed_estimate)
        )
    } else if let Some(skipped) = report.skipped_paths.first() {
        format!("Could not delete cache · {}", skipped.reason)
    } else {
        "Nothing was deleted".to_owned()
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, cursor::Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let mut stdout = std::io::stdout();
                let _ = execute!(stdout, LeaveAlternateScreen, cursor::Show);
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            cursor::Show
        );
        let _ = self.terminal.show_cursor();
    }
}

pub fn run(options: Options) -> Result<(), Error> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(Error::Usage {
            message: "the TUI needs an interactive terminal; use `cacheferret scan` for pipelines"
                .to_owned(),
        });
    }

    let mut session = TerminalSession::enter().map_err(terminal_error)?;
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(options);
    app.begin_scan(&tx);
    let mut dirty = true;

    while !app.should_quit {
        dirty |= drain_worker(&rx, &mut app, &tx);
        if app
            .toast
            .as_ref()
            .is_some_and(|(_, shown_at)| shown_at.elapsed() >= Duration::from_secs(4))
        {
            app.toast = None;
            dirty = true;
        }
        let animating = app.ui.animate
            && matches!(
                app.phase,
                Phase::Scanning | Phase::Reviewing | Phase::Cleaning
            );
        if dirty || animating {
            session
                .terminal
                .draw(|frame| render(frame, &mut app))
                .map_err(terminal_error)?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(80)).map_err(terminal_error)? {
            match event::read().map_err(terminal_error)? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, key, &tx);
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
    }
    Ok(())
}

fn terminal_error(source: std::io::Error) -> Error {
    Error::Io {
        path: PathBuf::from("<terminal>"),
        source,
    }
}

fn drain_worker(rx: &Receiver<WorkerMessage>, app: &mut App, tx: &Sender<WorkerMessage>) -> bool {
    let mut changed = false;
    while let Ok(message) = rx.try_recv() {
        app.handle_worker(message, tx);
        changed = true;
    }
    changed
}

fn handle_key(app: &mut App, key: KeyEvent, tx: &Sender<WorkerMessage>) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if app.phase == Phase::Cleaning {
            app.toast = Some((
                "Deletion is in progress — wait for it to finish".to_owned(),
                Instant::now(),
            ));
        } else {
            app.should_quit = true;
        }
        return;
    }
    if app.editing_filter {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => app.editing_filter = false,
            KeyCode::Backspace => {
                app.query.pop();
                app.cursor = 0;
                app.rebuild_visible();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.query.clear();
                app.cursor = 0;
                app.rebuild_visible();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.query.push(character);
                app.cursor = 0;
                app.rebuild_visible();
            }
            _ => {}
        }
        return;
    }
    if app.show_help {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
        ) {
            app.show_help = false;
        }
        return;
    }
    match app.phase {
        Phase::Scanning => {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                app.should_quit = true;
            }
        }
        Phase::Confirming => match key.code {
            KeyCode::Char('y') | KeyCode::Char('d') | KeyCode::Enter => app.confirm_delete(tx),
            KeyCode::Char('n') | KeyCode::Esc => app.cancel_delete(),
            _ => {}
        },
        Phase::Reviewing | Phase::Cleaning => {}
        Phase::Failed => match key.code {
            KeyCode::Char('r') => app.begin_scan(tx),
            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
            _ => {}
        },
        Phase::Ready => match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Char('?') => app.show_help = true,
            KeyCode::Char('/') => app.editing_filter = true,
            KeyCode::Esc if !app.query.is_empty() => {
                app.query.clear();
                app.cursor = 0;
                app.rebuild_visible();
            }
            KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1),
            KeyCode::PageDown => app.move_cursor(10),
            KeyCode::PageUp => app.move_cursor(-10),
            KeyCode::Home | KeyCode::Char('g') => app.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => app.cursor = app.visible.len().saturating_sub(1),
            KeyCode::Tab => {
                app.view_scope = app.view_scope.next();
                app.cursor = 0;
                app.rebuild_visible();
            }
            KeyCode::Char('s') => {
                app.sort = app.sort.next();
                app.cursor = 0;
                app.rebuild_visible();
            }
            KeyCode::Char('d') => app.begin_delete(tx),
            KeyCode::Char('r') => app.begin_scan(tx),
            _ => {}
        },
    }
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let palette = app.ui.palette;
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.text)),
        area,
    );
    if area.width < 58 || area.height < 16 {
        render_too_small(frame, area, app.ui);
        return;
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, sections[0], app);
    match app.phase {
        Phase::Scanning => render_busy(frame, sections[1], app, "Sniffing out rebuildable caches"),
        Phase::Reviewing => render_workspace(frame, sections[1], app),
        Phase::Failed => render_error(frame, sections[1], app),
        _ => render_workspace(frame, sections[1], app),
    }
    render_footer(frame, sections[2], app);

    if app.show_help {
        render_help(frame, area, app);
    } else if app.phase == Phase::Confirming {
        render_confirmation(frame, area, app);
    } else if app.phase == Phase::Reviewing {
        render_reviewing(frame, area, app);
    } else if app.phase == Phase::Cleaning {
        render_cleaning(frame, area, app);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.ui.palette;
    let (status, status_color) = match app.phase {
        Phase::Scanning => ("SCANNING", palette.info),
        Phase::Reviewing => ("CHECKING", palette.info),
        Phase::Confirming => ("CONFIRM", palette.accent),
        Phase::Cleaning => ("CLEANING", palette.accent),
        Phase::Failed => ("SCAN FAILED", palette.danger),
        _ => ("READY", palette.success),
    };
    let total_bytes: u64 = app.candidates.iter().map(|candidate| candidate.bytes).sum();
    let summary = if app.phase == Phase::Scanning {
        if app.ui.unicode {
            "Scanning…".to_owned()
        } else {
            "Scanning...".to_owned()
        }
    } else {
        let warnings = if app.warnings.is_empty() {
            String::new()
        } else {
            format!(
                "  {}  {} paths skipped",
                app.ui.separator().trim(),
                app.warnings.len()
            )
        };
        format!(
            "{} caches  {}  {} discovered{}",
            app.candidates.len(),
            app.ui.separator().trim(),
            format_bytes(total_bytes),
            warnings
        )
    };
    let brand_marker = if app.ui.unicode { "  ◉ " } else { "  o " };
    let status_marker = if app.ui.unicode { "●" } else { "*" };
    let line = Line::from(vec![
        Span::styled(brand_marker, Style::default().fg(palette.info)),
        Span::styled(
            "CACHE",
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "FERRET",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("{status_marker} {status}"),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(if area.width >= 76 { "   " } else { "" }),
        Span::styled(
            if area.width >= 76 {
                summary
            } else {
                String::new()
            },
            Style::default().fg(palette.muted),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(palette.surface_active))
                .style(Style::default().bg(palette.bg)),
        ),
        area,
    );
}

fn render_workspace(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.width >= 104 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(area);
        render_table(frame, columns[0], app);
        render_details(frame, columns[1], app);
    } else if area.height >= 18 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(8)])
            .split(area);
        render_table(frame, rows[0], app);
        render_details(frame, rows[1], app);
    } else {
        render_table(frame, area, app);
    }
}

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = app.ui.palette;
    let indices = &app.visible;
    let compact = area.width < 76;
    let medium = !compact && area.width < 96;
    let rows: Vec<Row> = indices
        .iter()
        .map(|index| {
            let candidate = &app.candidates[*index];
            let marker = if candidate.cleanable {
                Span::raw(" ")
            } else {
                Span::styled(
                    if app.ui.unicode { "×" } else { "x" },
                    Style::default().fg(palette.muted),
                )
            };
            let age = candidate
                .age_days
                .map_or_else(|| "unknown".to_owned(), |days| format!("{days}d"));
            let scope = match candidate.scope {
                CacheScope::Project => "project",
                CacheScope::Global => "global",
            };
            let row_style = if !candidate.cleanable {
                Style::default().fg(palette.muted)
            } else {
                Style::default().fg(palette.text)
            };
            let size = Cell::from(format_bytes(candidate.bytes))
                .style(Style::default().fg(palette.accent));
            let path = Cell::from(candidate.path.display().to_string());
            let cells = if compact {
                vec![
                    Cell::from(marker),
                    size,
                    Cell::from(candidate.kind.clone()),
                    path,
                ]
            } else if medium {
                vec![
                    Cell::from(marker),
                    size,
                    Cell::from(candidate.kind.clone()),
                    Cell::from(age).style(Style::default().fg(palette.muted)),
                    path,
                ]
            } else {
                vec![
                    Cell::from(marker),
                    size,
                    Cell::from(candidate.kind.clone()),
                    Cell::from(age).style(Style::default().fg(palette.muted)),
                    Cell::from(scope).style(Style::default().fg(palette.muted)),
                    path,
                ]
            };
            Row::new(cells).style(row_style)
        })
        .collect();
    let title = format!(
        " Caches{}{}{}{}{} ",
        app.ui.separator(),
        app.view_scope.label(),
        app.ui.separator(),
        app.sort.label(app.ui.unicode),
        if app.query.is_empty() {
            String::new()
        } else {
            format!("{}filter: {}", app.ui.separator(), app.query)
        }
    );
    let (headers, widths) = if compact {
        (
            vec!["", "SIZE", "KIND", "PATH"],
            vec![
                Constraint::Length(2),
                Constraint::Length(9),
                Constraint::Length(14),
                Constraint::Min(10),
            ],
        )
    } else if medium {
        (
            vec!["", "SIZE", "KIND", "AGE", "PATH"],
            vec![
                Constraint::Length(2),
                Constraint::Length(10),
                Constraint::Length(19),
                Constraint::Length(8),
                Constraint::Min(16),
            ],
        )
    } else {
        (
            vec!["", "SIZE", "KIND", "AGE", "SCOPE", "PATH"],
            vec![
                Constraint::Length(2),
                Constraint::Length(10),
                Constraint::Length(19),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Min(16),
            ],
        )
    };
    let header = Row::new(headers)
        .style(
            Style::default()
                .fg(palette.muted)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(app.ui.border())
                .border_style(Style::default().fg(palette.surface_active))
                .title_style(
                    Style::default()
                        .fg(palette.text)
                        .add_modifier(Modifier::BOLD),
                )
                .style(Style::default().bg(palette.bg)),
        )
        .row_highlight_style(
            Style::default()
                .bg(palette.surface_active)
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(if app.ui.unicode { "› " } else { "> " });
    let mut state = TableState::default();
    if !indices.is_empty() {
        state.select(Some(app.cursor));
    }
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_details(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.ui.palette;
    let Some(index) = app.focused_index() else {
        frame.render_widget(
            Paragraph::new(if app.candidates.is_empty() {
                "No caches found. Enjoy the breathing room."
            } else {
                "No caches match this view. Press Esc to clear the filter."
            })
            .style(Style::default().fg(palette.muted))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(detail_block(" Details ", app.ui)),
            area,
        );
        return;
    };
    let candidate = &app.candidates[index];
    let total: u64 = app.candidates.iter().map(|item| item.bytes).sum();
    let percent = if total == 0 {
        0
    } else {
        ((candidate.bytes as f64 / total as f64) * 100.0).round() as u64
    };
    let restore = if candidate.network_restore {
        "download required"
    } else {
        "local rebuild"
    };
    let action = if !candidate.cleanable {
        Span::styled("scan-only", Style::default().fg(palette.muted))
    } else {
        Span::styled(
            "press d to delete",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let mut lines = vec![Line::from(Span::styled(
        candidate.path.display().to_string(),
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    ))];
    if area.height >= 10 {
        lines.push(Line::from(""));
    }
    lines.extend([
        labeled(
            "Size",
            format!(
                "{}  {}  {percent}% of total",
                format_bytes(candidate.bytes),
                app.ui.separator().trim()
            ),
            palette,
        ),
        labeled(
            "Age",
            candidate
                .age_days
                .map_or_else(|| "unknown".to_owned(), |days| format!("{days} days")),
            palette,
        ),
        labeled(
            "Type",
            format!(
                "{}{}{}",
                candidate.ecosystem,
                app.ui.separator(),
                candidate.kind
            ),
            palette,
        ),
        labeled("Restore", restore.to_owned(), palette),
        Line::from(vec![
            Span::styled("Action   ", Style::default().fg(palette.muted)),
            action,
        ]),
    ]);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(detail_block(" Inspect ", app.ui)),
        area,
    );
}

fn detail_block(title: &'static str, ui: UiPreferences) -> Block<'static> {
    let palette = ui.palette;
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(ui.border())
        .border_style(Style::default().fg(palette.surface_active))
        .title_style(
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(palette.surface))
}

fn labeled(label: &'static str, value: String, palette: Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<9}"), Style::default().fg(palette.muted)),
        Span::styled(value, Style::default().fg(palette.text)),
    ])
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.ui.palette;
    let content = if app.editing_filter {
        Line::from(vec![
            Span::styled(
                " FILTER  ",
                Style::default()
                    .bg(palette.info)
                    .fg(palette.bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}{}", app.query, if app.ui.unicode { "█" } else { "_" }),
                Style::default().fg(palette.text),
            ),
            Span::styled(
                if app.ui.unicode {
                    "   Enter apply  ·  Esc close  ·  Ctrl+U clear"
                } else {
                    "   Enter apply  |  Esc close  |  Ctrl+U clear"
                },
                Style::default().fg(palette.muted),
            ),
        ])
    } else if let Some((message, shown_at)) = &app.toast {
        if shown_at.elapsed() < Duration::from_secs(4) {
            Line::from(vec![
                Span::styled(
                    if app.ui.unicode { "  ●  " } else { "  *  " },
                    Style::default().fg(palette.info),
                ),
                Span::styled(message.clone(), Style::default().fg(palette.text)),
            ])
        } else {
            footer_shortcuts(app, area.width)
        }
    } else {
        footer_shortcuts(app, area.width)
    };
    frame.render_widget(
        Paragraph::new(content)
            .style(Style::default().bg(palette.surface).fg(palette.text))
            .alignment(Alignment::Left),
        area,
    );
}

fn footer_shortcuts(app: &App, width: u16) -> Line<'static> {
    let palette = app.ui.palette;
    if app.phase == Phase::Scanning {
        return Line::from(vec![
            Span::styled(
                "  q",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" quit", Style::default().fg(palette.muted)),
        ]);
    }
    if app.phase == Phase::Reviewing {
        return Line::from(vec![
            Span::styled(
                if app.ui.unicode { "  ◐ " } else { "  * " },
                Style::default().fg(palette.info),
            ),
            Span::styled(
                "checking current cache state",
                Style::default().fg(palette.muted),
            ),
        ]);
    }
    if app.ui.unicode {
        footer_shortcuts_for_width(app, "↑↓", width)
    } else {
        footer_shortcuts_for_width(app, "jk", width)
    }
}

fn footer_shortcuts_for_width(app: &App, movement: &'static str, width: u16) -> Line<'static> {
    let palette = app.ui.palette;
    if width < 76 {
        return Line::from(vec![
            key(movement, palette),
            hint("move", palette),
            key("d", palette),
            hint("delete", palette),
            key("/", palette),
            hint("filter", palette),
            key("?", palette),
            hint("help", palette),
        ]);
    }
    Line::from(vec![
        key(movement, palette),
        hint("move", palette),
        key("d", palette),
        hint("delete", palette),
        key("/", palette),
        hint("filter", palette),
        key("tab", palette),
        hint("scope", palette),
        key("s", palette),
        hint("sort", palette),
        key("r", palette),
        hint("rescan", palette),
        key("?", palette),
        hint("help", palette),
    ])
}

fn key(value: &'static str, palette: Palette) -> Span<'static> {
    Span::styled(
        format!("  {value}"),
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )
}

fn hint(value: &'static str, palette: Palette) -> Span<'static> {
    Span::styled(format!(" {value}"), Style::default().fg(palette.muted))
}

fn render_busy(frame: &mut Frame, area: Rect, app: &App, message: &str) {
    let palette = app.ui.palette;
    let content = vec![
        Line::from(Span::styled(
            progress_glyph(app),
            Style::default()
                .fg(palette.info)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            message,
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Sizing directories in parallel.",
            Style::default().fg(palette.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(content)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::default().style(Style::default().bg(palette.bg))),
        centered_rect(70, 8, area),
    );
}

fn render_error(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.ui.palette;
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "The scan could not finish",
                Style::default()
                    .fg(palette.danger)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                app.error.as_deref().unwrap_or("Unknown error"),
                Style::default().fg(palette.text),
            )),
            Line::from(""),
            Line::from(Span::styled(
                if app.ui.unicode {
                    "r retry  ·  q quit"
                } else {
                    "r retry  |  q quit"
                },
                Style::default().fg(palette.muted),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(detail_block(" Scan failed ", app.ui)),
        centered_rect(72, 11, area),
    );
}

fn render_reviewing(frame: &mut Frame, area: Rect, app: &App) {
    render_progress_modal(
        frame,
        area,
        app,
        "Checking current cache state",
        "Remeasuring size and recent activity before delete",
        " Check ",
    );
}

fn render_confirmation(frame: &mut Frame, area: Rect, app: &App) {
    let Some(candidate) = &app.pending_delete else {
        return;
    };
    let palette = app.ui.palette;
    let modal = centered_box(76, 11, area);
    let reasons = risk_reasons(candidate).join(app.ui.separator());
    let path = truncate_middle(
        &candidate.path.display().to_string(),
        modal.width.saturating_sub(6) as usize,
        app.ui.unicode,
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("Delete {}?", candidate.kind),
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(path, Style::default().fg(palette.muted))),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    format_bytes(candidate.bytes),
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}  {reasons}", app.ui.separator().trim()),
                    Style::default().fg(palette.muted),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " d / y ",
                    Style::default()
                        .bg(palette.accent)
                        .fg(palette.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" delete    ", Style::default().fg(palette.text)),
                Span::styled(
                    " n / Esc ",
                    Style::default().bg(palette.surface_active).fg(palette.text),
                ),
                Span::styled(" cancel", Style::default().fg(palette.text)),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Confirm delete ")
                .borders(Borders::ALL)
                .border_type(app.ui.border())
                .border_style(Style::default().fg(palette.accent))
                .style(Style::default().bg(palette.surface).fg(palette.text)),
        ),
        modal,
    );
}

fn render_cleaning(frame: &mut Frame, area: Rect, app: &App) {
    let fallback = if app.ui.unicode {
        "Working…"
    } else {
        "Working..."
    };
    let detail = app
        .deleting
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(|| fallback.into(), |name| name.to_string_lossy());
    render_progress_modal(frame, area, app, "Deleting cache", &detail, " Delete ");
}

fn render_progress_modal(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    title: &str,
    detail: &str,
    block_title: &'static str,
) {
    let palette = app.ui.palette;
    let modal = centered_box(68, 9, area);
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                progress_glyph(app),
                Style::default().fg(palette.accent),
            )),
            Line::from(""),
            Line::from(Span::styled(
                title.to_owned(),
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                truncate_middle(
                    detail,
                    modal.width.saturating_sub(6) as usize,
                    app.ui.unicode,
                ),
                Style::default().fg(palette.muted),
            )),
        ])
        .alignment(Alignment::Center)
        .block(detail_block(block_title, app.ui)),
        modal,
    );
}

fn render_help(frame: &mut Frame, area: Rect, app: &App) {
    let palette = app.ui.palette;
    let modal = centered_box(78, 14, area);
    frame.render_widget(Clear, modal);
    let rows = if modal.width < 72 {
        [
            ("Up/Down j/k", "Move"),
            ("d", "Delete; confirms risky caches"),
            ("/", "Filter"),
            ("Tab", "Cycle scope"),
            ("s", "Cycle sort"),
            ("r", "Scan again"),
            ("q / Ctrl+C", "Quit"),
        ]
    } else {
        [
            ("↑ / ↓  j / k", "Move through caches"),
            ("d", "Delete the focused cache; confirm when risky"),
            ("/", "Filter by path, kind, or ecosystem"),
            ("Tab", "Cycle all, project, and global caches"),
            ("s", "Cycle size, age, and name sorting"),
            ("r", "Scan again"),
            ("q / Ctrl+C", "Quit"),
        ]
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "One key does one thing.",
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    lines.extend(rows.into_iter().map(|(shortcut, description)| {
        Line::from(vec![
            Span::styled(
                format!("{shortcut:<15}"),
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(description, Style::default().fg(palette.text)),
        ])
    }));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            if app.ui.unicode {
                "× marks catalog entries that are scan-only."
            } else {
                "x marks catalog entries that are scan-only."
            },
            Style::default().fg(palette.muted),
        )),
        Line::from(Span::styled(
            "Press ? or Esc to close",
            Style::default().fg(palette.info),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" Keyboard ")
                .borders(Borders::ALL)
                .border_type(app.ui.border())
                .border_style(Style::default().fg(palette.info))
                .style(Style::default().bg(palette.surface).fg(palette.text)),
        ),
        modal,
    );
}

fn render_too_small(frame: &mut Frame, area: Rect, ui: UiPreferences) {
    let palette = ui.palette;
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "CacheFerret needs a little more room",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                if ui.unicode {
                    "Resize to at least 58 × 16 · q to quit"
                } else {
                    "Resize to at least 58 x 16 | q to quit"
                },
                Style::default().fg(palette.muted),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true }),
        centered_rect(90, 4, area),
    );
}

fn progress_glyph(app: &App) -> &'static str {
    if !app.ui.unicode {
        return "*";
    }
    if !app.ui.animate {
        return "●";
    }
    let frames = ["◐", "◓", "◑", "◒"];
    frames[(app.scan_started.elapsed().as_millis() / 120) as usize % frames.len()]
}

fn truncate_middle(value: &str, max_chars: usize, unicode: bool) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    let marker = if unicode { "…" } else { "..." };
    let marker_width = marker.chars().count();
    if max_chars <= marker_width + 1 {
        return value.chars().take(max_chars).collect();
    }
    let left = (max_chars - marker_width) / 2;
    let right = max_chars - marker_width - left;
    let start: String = value.chars().take(left).collect();
    let end: String = value
        .chars()
        .rev()
        .take(right)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}{marker}{end}")
}

fn centered_box(max_width: u16, max_height: u16, area: Rect) -> Rect {
    let width = max_width.min(area.width.saturating_sub(2)).max(1);
    let height = max_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let height = height.min(area.height);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(height) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::SystemTime;

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use tempfile::tempdir;

    use super::*;

    fn options() -> Options {
        Options {
            roots: vec![PathBuf::from(".")],
            scope: ScopeFilter::All,
            kinds: Vec::new(),
        }
    }

    fn test_app(options: Options) -> App {
        App::with_ui(options, UiPreferences::rich())
    }

    fn receive_worker(app: &mut App, tx: &Sender<WorkerMessage>, rx: &Receiver<WorkerMessage>) {
        let message = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        app.handle_worker(message, tx);
    }

    fn render_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
        let buffer = render_buffer(app, width, height);
        let mut output = String::new();
        for y in 0..height {
            for x in 0..width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }

    fn project_app() -> (tempfile::TempDir, PathBuf, App) {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let target = project.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
        fs::write(target.join("artifact"), [7_u8; 64]).unwrap();
        let options = Options {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
        };
        let report = discover(&DiscoveryOptions {
            roots: options.roots.clone(),
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 7,
        })
        .unwrap();
        let mut app = test_app(options);
        let (tx, _rx) = mpsc::channel();
        app.handle_worker(WorkerMessage::Scan(Ok(report)), &tx);
        (temp, target, app)
    }

    #[test]
    fn scope_and_sort_cycles_are_predictable() {
        assert_eq!(ViewScope::All.next(), ViewScope::Project);
        assert_eq!(ViewScope::Project.next(), ViewScope::Global);
        assert_eq!(ViewScope::Global.next(), ViewScope::All);
        assert_eq!(SortOrder::Size.next(), SortOrder::Age);
        assert_eq!(SortOrder::Age.next(), SortOrder::Name);
        assert_eq!(SortOrder::Name.next(), SortOrder::Size);
    }

    #[test]
    fn empty_app_never_moves_cursor_out_of_bounds() {
        let mut app = test_app(options());
        app.move_cursor(10);
        assert_eq!(app.cursor, 0);
        app.move_cursor(-10);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn d_prompts_for_a_recent_cache_then_y_deletes_it() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let target = project.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();

        let mut app = test_app(Options {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
        });
        let report = discover(&DiscoveryOptions {
            roots: app.options.roots.clone(),
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 7,
        })
        .unwrap();
        let (tx, rx) = mpsc::channel();
        app.handle_worker(WorkerMessage::Scan(Ok(report)), &tx);

        let candidate = &app.candidates[0];
        assert!(candidate.protected);
        assert!(candidate.cleanable);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.phase, Phase::Reviewing);
        receive_worker(&mut app, &tx, &rx);
        assert_eq!(app.phase, Phase::Confirming);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.phase, Phase::Ready);
        assert!(target.exists());
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &tx,
        );
        receive_worker(&mut app, &tx, &rx);
        assert_eq!(app.phase, Phase::Confirming);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.phase, Phase::Reviewing);
        receive_worker(&mut app, &tx, &rx);
        assert_eq!(app.phase, Phase::Cleaning);
        receive_worker(&mut app, &tx, &rx);

        assert!(!target.exists());
        assert_eq!(app.phase, Phase::Ready);
    }

    #[test]
    fn d_immediately_deletes_an_ordinary_project_cache() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("demo");
        let target = project.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();

        let old = SystemTime::now() - Duration::from_secs(30 * 86_400);
        fs::File::open(&target).unwrap().set_modified(old).unwrap();

        let mut app = test_app(Options {
            roots: vec![temp.path().to_path_buf()],
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
        });
        let report = discover(&DiscoveryOptions {
            roots: app.options.roots.clone(),
            scope: ScopeFilter::Project,
            kinds: Vec::new(),
            protect_days: 0,
        })
        .unwrap();
        let (tx, rx) = mpsc::channel();
        app.handle_worker(WorkerMessage::Scan(Ok(report)), &tx);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.phase, Phase::Reviewing);
        receive_worker(&mut app, &tx, &rx);
        assert_eq!(app.phase, Phase::Cleaning);
        receive_worker(&mut app, &tx, &rx);

        assert!(!target.exists());
    }

    #[test]
    fn quit_keys_do_not_interrupt_an_active_cleanup() {
        let mut app = test_app(options());
        app.phase = Phase::Cleaning;
        let (tx, _rx) = mpsc::channel();

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            &tx,
        );
        assert!(!app.should_quit);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &tx,
        );
        assert!(!app.should_quit);
    }

    #[test]
    fn stale_safe_candidate_is_reclassified_before_delete() {
        let (_temp, target, mut app) = project_app();
        app.candidates[0].age_days = Some(30);
        app.candidates[0].protected = false;
        let (tx, rx) = mpsc::channel();

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &tx,
        );
        receive_worker(&mut app, &tx, &rx);

        assert_eq!(app.phase, Phase::Confirming);
        assert_eq!(app.pending_delete.as_ref().unwrap().age_days, Some(0));
        assert!(target.exists());
    }

    #[test]
    fn terminal_preferences_cover_truecolor_plain_ascii_and_reduced_motion() {
        let truecolor = UiPreferences::from_values(
            false,
            Some("xterm-256color"),
            Some("truecolor"),
            Some("en_US.UTF-8"),
            false,
            false,
        );
        assert_eq!(truecolor.palette, Palette::TRUECOLOR);
        assert!(truecolor.unicode);
        assert!(truecolor.animate);

        let ansi256 = UiPreferences::from_values(
            false,
            Some("xterm-256color"),
            None,
            Some("en_US.UTF-8"),
            false,
            false,
        );
        assert_eq!(ansi256.palette, Palette::ANSI256);

        let ansi16 = UiPreferences::from_values(
            false,
            Some("vt100"),
            None,
            Some("en_US.UTF-8"),
            false,
            false,
        );
        assert_eq!(ansi16.palette, Palette::ANSI16);

        let plain = UiPreferences::from_values(true, Some("xterm"), None, Some("C"), true, true);
        assert_eq!(plain.palette, Palette::PLAIN);
        assert!(!plain.unicode);
        assert!(!plain.animate);

        let dumb = UiPreferences::from_values(
            false,
            Some("dumb"),
            Some("truecolor"),
            Some("en_US.UTF-8"),
            false,
            false,
        );
        assert_eq!(dumb.palette, Palette::PLAIN);
        assert!(!dumb.unicode);
        assert!(!dumb.animate);
    }

    #[test]
    fn minimum_size_confirmation_keeps_risk_and_controls_visible() {
        let (_temp, _target, mut app) = project_app();
        let mut candidate = app.candidates[0].clone();
        candidate.path = PathBuf::from(
            "/a/very/long/project/path/that/must/not/push/the/destructive/controls/offscreen/target",
        );
        app.pending_delete = Some(candidate);
        app.phase = Phase::Confirming;

        let output = render_text(&mut app, 58, 16);

        assert!(output.contains("Confirm delete"));
        assert!(output.contains("Delete cargo-target?"));
        assert!(output.contains("recent"));
        assert!(output.contains("d / y"));
        assert!(output.contains("n / Esc"));
    }

    #[test]
    fn layouts_render_at_minimum_standard_and_wide_sizes() {
        let (_temp, _target, mut app) = project_app();
        let minimum = render_text(&mut app, 58, 16);
        assert!(minimum.contains("Caches"));
        assert!(minimum.contains("d delete"));
        assert!(!minimum.contains("Inspect"));

        let standard = render_text(&mut app, 80, 24);
        assert!(standard.contains("Caches"));
        assert!(standard.contains("Inspect"));

        let wide = render_text(&mut app, 140, 40);
        assert!(wide.contains("Caches"));
        assert!(wide.contains("Inspect"));
        assert!(wide.contains("press d to delete"));
    }

    #[test]
    fn minimum_size_help_keeps_every_command_visible() {
        let mut app = test_app(options());
        app.phase = Phase::Ready;
        app.show_help = true;

        let output = render_text(&mut app, 58, 16);

        for expected in [
            "Keyboard",
            "Up/Down j/k",
            "Delete; confirms risky caches",
            "Cycle scope",
            "Cycle sort",
            "Press ? or Esc to close",
        ] {
            assert!(output.contains(expected), "missing {expected:?}\n{output}");
        }
    }

    #[test]
    fn plain_ascii_mode_has_no_color_or_unicode_only_glyphs() {
        let (_temp, _target, mut app) = project_app();
        app.ui = UiPreferences::from_values(true, Some("dumb"), None, Some("C"), true, true);
        let buffer = render_buffer(&mut app, 80, 24);
        let mut output = String::new();
        for y in 0..24 {
            for x in 0..80 {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }

        for glyph in ["◉", "●", "›", "×", "◐", "◓", "◑", "◒", "…", "·", "↑", "↓"]
        {
            assert!(!output.contains(glyph), "found unicode glyph {glyph:?}");
        }
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset),
            "plain mode emitted terminal colors"
        );
    }
}
