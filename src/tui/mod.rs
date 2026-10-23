use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::ListState;

use crate::coach::claude::{self, Kind};
use crate::coach::mock::Transcript;
use crate::config::Config;
use crate::judge::proc::Cancel;
use crate::model::srs;
use crate::model::store::{Problem, Progress, Status, Store, UiState};
use crate::practice;

mod draw;
mod input;
mod mock_interview;
mod settings_popup;
mod tasks;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);
const FLASH_FOR: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Nc150,
    Nc250,
    Blind75,
    All,
    Due,
}

const LISTS: [ListKind; 5] = [
    ListKind::Nc150,
    ListKind::Nc250,
    ListKind::Blind75,
    ListKind::All,
    ListKind::Due,
];

impl ListKind {
    fn name(self) -> &'static str {
        match self {
            ListKind::Nc150 => "NeetCode 150",
            ListKind::Nc250 => "NeetCode 250",
            ListKind::Blind75 => "Blind 75",
            ListKind::All => "All",
            ListKind::Due => "Due for review",
        }
    }

    /// Stable id for the saved UI state.
    fn id(self) -> &'static str {
        match self {
            ListKind::Nc150 => "150",
            ListKind::Nc250 => "250",
            ListKind::Blind75 => "blind75",
            ListKind::All => "all",
            ListKind::Due => "due",
        }
    }

    fn from_id(id: &str) -> Self {
        LISTS
            .into_iter()
            .find(|l| l.id() == id)
            .unwrap_or(ListKind::Nc150)
    }

    fn next(self) -> Self {
        let i = LISTS.iter().position(|l| *l == self).unwrap_or(0);
        LISTS[(i + 1) % LISTS.len()]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Problem,
    Solution,
    Claude,
    Output,
    Stats,
}

const PANES: [Pane; 5] = [
    Pane::Problem,
    Pane::Solution,
    Pane::Claude,
    Pane::Output,
    Pane::Stats,
];

impl Pane {
    fn title(self) -> &'static str {
        match self {
            Pane::Problem => "1 Problem",
            Pane::Solution => "2 Solution",
            Pane::Claude => "3 Claude",
            Pane::Output => "4 Output",
            Pane::Stats => "5 Stats",
        }
    }
}

/// Questions answered with y/n (or another letter) in the footer.
enum Confirm {
    StartMock(usize),
    FinishMock,
    Fresh(usize),
    /// Quitting would throw away a mock in progress.
    Quit,
}

enum Mode {
    Normal,
    Search,
    Ask(String),
    /// Answering the interviewer in a mock debrief.
    Answer(String),
    Confirm(Confirm),
    Help,
    /// The settings popup: selected setting and, while typing, the input.
    Settings {
        cursor: usize,
        input: Option<String>,
    },
}

enum Row {
    Header {
        pattern: String,
        solved: usize,
        total: usize,
    },
    Item(usize),
}

struct AiTask {
    /// Empty for requests not tied to a problem (weak-spot analysis).
    slug: String,
    kind: Kind,
    question: String,
    text: String,
    started: Instant,
    rx: Receiver<claude::Event>,
    cancel: Cancel,
}

/// Dropping a task (cancel, quit) kills its process.
impl Drop for AiTask {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Raw,
    Tests,
}

struct RunTask {
    slug: String,
    kind: RunKind,
    rx: Receiver<String>,
    cancel: Cancel,
}

impl Drop for RunTask {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct MockState {
    idx: usize,
    started_at: DateTime<Local>,
    clock: Instant,
    limit: Duration,
    /// Set once coding is finished.
    debrief: Option<Debrief>,
}

struct Debrief {
    elapsed: Duration,
    transcript: Transcript,
}

/// Rendered right-pane content, rebuilt only when its inputs change.
struct View {
    key: (usize, Pane, u64),
    text: Text<'static>,
    /// Wrapped height for a given width; counting is O(text), so cache it.
    lines: Option<(u16, usize)>,
}

/// Something that needs the real terminal, handled outside of drawing.
enum External {
    Edit(usize),
    Notes(usize),
    /// The Claude chat; `true` starts a new session instead of continuing the last one.
    Tutor(usize, bool),
}

pub struct App {
    cfg: Config,
    store: Store,
    problems: Vec<Problem>,
    progress: Progress,
    today: NaiveDate,
    list: ListKind,
    search: String,
    hide_solved: bool,
    rows: Vec<Row>,
    list_state: ListState,
    pane: Pane,
    scroll: u16,
    mode: Mode,
    ai: Option<AiTask>,
    run: Option<RunTask>,
    output: Option<(String, String)>,
    mock: Option<MockState>,
    status: Option<(String, Instant)>,
    /// Bumped whenever anything shown in the right pane may have changed.
    version: u64,
    view: Option<View>,
    /// Last drawn areas, for mouse handling.
    list_area: Rect,
    pane_area: Rect,
    quit: bool,
}

pub fn run(cfg: Config, store: Store) -> Result<()> {
    let mut app = App::new(cfg, store)?;

    // ratatui::init installs a panic hook restoring the terminal; chain one that
    // also turns mouse capture and bracketed paste off again.
    let mut terminal = ratatui::try_init().context("neet's TUI needs an interactive terminal")?;
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        prev(info);
    }));
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let res = app.event_loop(&mut terminal);
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    app.save_ui();
    res
}

/// Hands the terminal to a child program (editor, interactive claude).
fn suspend() -> Result<()> {
    execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    )?;
    disable_raw_mode()?;
    Ok(())
}

fn resume(terminal: &mut DefaultTerminal) -> Result<()> {
    enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    terminal.clear()?;
    Ok(())
}

impl App {
    fn new(cfg: Config, store: Store) -> Result<Self> {
        let problems = store.load_problems()?;
        let progress = store.load_progress()?;
        let ui = store.load_ui();
        let mut app = App {
            cfg,
            store,
            problems,
            progress,
            today: srs::today(),
            list: ListKind::from_id(&ui.list),
            search: String::new(),
            hide_solved: ui.hide_solved,
            rows: Vec::new(),
            list_state: ListState::default(),
            pane: Pane::Problem,
            scroll: 0,
            mode: Mode::Normal,
            ai: None,
            run: None,
            output: None,
            mock: None,
            status: None,
            version: 0,
            view: None,
            list_area: Rect::default(),
            pane_area: Rect::default(),
            quit: false,
        };
        let keep = ui
            .slug
            .and_then(|s| app.problems.iter().position(|p| p.slug == s));
        app.rebuild(keep);
        Ok(app)
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut dirty = true;
        while !self.quit {
            // Redraw on change, and every tick while a spinner, timer or flash is visible.
            let animating = self.ai.is_some()
                || self.run.is_some()
                || self.mock.is_some()
                || self.status.is_some();
            if dirty || animating {
                terminal.draw(|f| self.draw(f))?;
                dirty = false;
            }
            dirty |= self.poll_tasks();
            if !event::poll(TICK)? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    dirty = true;
                    if let Some(ext) = self.on_key(key) {
                        suspend()?;
                        let res = self.external(ext);
                        resume(terminal)?;
                        // Other `neet` processes (e.g. from nvim) may have changed progress.
                        self.reload_progress();
                        if let Err(e) = res {
                            self.flash(format!("error: {e:#}"));
                        }
                    }
                }
                Event::Mouse(m) => dirty |= self.on_mouse(m),
                Event::Paste(text) => {
                    dirty = true;
                    self.on_paste(&text);
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn save_ui(&self) {
        let _ = self.store.save_ui(&UiState {
            list: self.list.id().to_string(),
            slug: self.selected().map(|p| p.slug.clone()),
            hide_solved: self.hide_solved,
        });
    }

    // ------------------------------------------------------------------
    // State helpers

    fn selected_idx(&self) -> Option<usize> {
        match self.rows.get(self.list_state.selected()?)? {
            Row::Item(i) => Some(*i),
            Row::Header { .. } => None,
        }
    }

    fn selected(&self) -> Option<&Problem> {
        self.selected_idx().map(|i| &self.problems[i])
    }

    fn status_of(&self, p: &Problem) -> Status {
        self.progress
            .entries
            .get(&p.slug)
            .map(|e| e.status)
            .unwrap_or_default()
    }

    fn is_due(&self, p: &Problem) -> bool {
        self.progress
            .entries
            .get(&p.slug)
            .is_some_and(|e| srs::is_due(e, self.today))
    }

    fn in_list(&self, p: &Problem) -> bool {
        match self.list {
            ListKind::Nc150 => p.nc150,
            ListKind::Nc250 => p.nc250,
            ListKind::Blind75 => p.blind75,
            ListKind::All => true,
            ListKind::Due => self.is_due(p),
        }
    }

    fn due_count(&self) -> usize {
        self.problems.iter().filter(|p| self.is_due(p)).count()
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    fn touch(&mut self) {
        self.version += 1;
    }

    fn set_progress(&mut self, res: Result<Progress>) {
        match res {
            Ok(p) => self.progress = p,
            Err(e) => self.flash(format!("error: {e:#}")),
        }
        self.touch();
        self.rebuild(None);
    }

    fn reload_progress(&mut self) {
        let res = self.store.load_progress();
        self.set_progress(res);
    }

    fn set_pane(&mut self, pane: Pane) {
        self.pane = pane;
        self.scroll = 0;
    }

    /// Help is off-limits during a mock interview.
    fn locked(&mut self) -> bool {
        if self.mock.is_some() {
            self.flash("not available during a mock interview (m to finish)");
            return true;
        }
        false
    }

    /// Recomputes visible rows, keeping `keep` (a problem index) selected if possible.
    fn rebuild(&mut self, keep: Option<usize>) {
        self.today = srs::today();
        let keep = keep.or_else(|| self.selected_idx());
        let q = self.search.to_lowercase();
        let mut rows = Vec::new();
        let mut header_at = None;
        let mut last_pattern: Option<&str> = None;
        for (i, p) in self.problems.iter().enumerate() {
            if !self.in_list(p) {
                continue;
            }
            if last_pattern != Some(p.pattern.as_str()) {
                last_pattern = Some(&p.pattern);
                header_at = Some(rows.len());
                rows.push(Row::Header {
                    pattern: p.pattern.clone(),
                    solved: 0,
                    total: 0,
                });
            }
            let solved = self.status_of(p) == Status::Solved;
            if let Some(Row::Header {
                solved: s, total, ..
            }) = header_at.map(|h| &mut rows[h])
            {
                *total += 1;
                *s += usize::from(solved);
            }
            // The due list shows solved problems by definition.
            let hidden = self.hide_solved && solved && self.list != ListKind::Due;
            let visible = !hidden
                && (q.is_empty() || p.title.to_lowercase().contains(&q) || p.slug.contains(&q));
            if visible {
                rows.push(Row::Item(i));
            }
        }
        // Drop headers whose items are all filtered out.
        let mut kept = Vec::with_capacity(rows.len());
        let mut iter = rows.into_iter().peekable();
        while let Some(row) = iter.next() {
            if matches!(row, Row::Header { .. }) && !matches!(iter.peek(), Some(Row::Item(_))) {
                continue;
            }
            kept.push(row);
        }
        self.rows = kept;

        let pos = keep
            .and_then(|k| self.row_of(k))
            .or_else(|| self.rows.iter().position(|r| matches!(r, Row::Item(_))));
        if pos != self.list_state.selected() {
            self.scroll = 0;
        }
        self.list_state.select(pos);
    }

    fn row_of(&self, idx: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, Row::Item(i) if *i == idx))
    }

    /// Selects a problem, clearing filters or switching list if it is hidden.
    fn select_problem(&mut self, idx: usize) {
        if self.row_of(idx).is_none() {
            self.search.clear();
            self.hide_solved = false;
            if !self.in_list(&self.problems[idx]) {
                self.list = ListKind::All;
            }
        }
        self.rebuild(Some(idx));
        self.scroll = 0;
    }

    fn move_by(&mut self, delta: isize) {
        let Some(cur) = self.list_state.selected() else {
            return;
        };
        let step = delta.signum();
        let mut remaining = delta.unsigned_abs();
        let mut i = cur as isize;
        let mut last_item = cur;
        while remaining > 0 {
            i += step;
            if i < 0 || i >= self.rows.len() as isize {
                break;
            }
            if matches!(self.rows[i as usize], Row::Item(_)) {
                last_item = i as usize;
                remaining -= 1;
            }
        }
        if last_item != cur {
            self.list_state.select(Some(last_item));
            self.scroll = 0;
        }
    }

    fn select_edge(&mut self, last: bool) {
        let pos = if last {
            self.rows.iter().rposition(|r| matches!(r, Row::Item(_)))
        } else {
            self.rows.iter().position(|r| matches!(r, Row::Item(_)))
        };
        self.list_state.select(pos);
        self.scroll = 0;
    }

    fn external(&mut self, ext: External) -> Result<()> {
        match ext {
            External::Edit(i) => {
                let p = self.problems[i].clone();
                let lsp = self.cfg.lsp_enabled(self.mock.is_some());
                practice::edit(&self.store, &self.cfg, &p, lsp)?;
            }
            External::Notes(i) => {
                let p = self.problems[i].clone();
                practice::edit_notes(&self.store, &self.cfg, &p)?;
            }
            External::Tutor(i, fresh) => {
                let p = self.problems[i].clone();
                let lsp = self.cfg.lsp_enabled(false);
                practice::chat(&self.store, &self.cfg, &p, fresh, lsp)?;
            }
        }
        self.touch();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::widgets::{Paragraph, Wrap};

    use super::*;

    /// An app on a fresh store with the bundled problem list.
    fn app(name: &str) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("neet-tui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store {
            root: dir.join("data"),
            workspace: dir.join("workspace"),
        };
        std::fs::create_dir_all(&store.root).unwrap();
        std::fs::write(
            store.problems_path(),
            include_str!("../../assets/problems.json"),
        )
        .unwrap();
        let cfg = Config {
            language: "python".into(),
            ..Config::default()
        };
        (App::new(cfg, store).unwrap(), dir)
    }

    fn press(app: &mut App, code: KeyCode) {
        assert!(
            app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
                .is_none()
        );
    }

    fn typing(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Renders the app and returns the screen as text.
    fn screen(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn starts_on_the_150_and_navigates() {
        let (mut app, dir) = app("nav");
        let s = screen(&mut app);
        assert!(s.contains("NeetCode 150"), "{s}");
        assert!(s.contains("Contains Duplicate"), "{s}");
        assert!(s.contains("c chat"), "{s}");
        assert_eq!(app.selected().unwrap().slug, "contains-duplicate");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected().unwrap().slug, "valid-anagram");
        press(&mut app, KeyCode::Char('l'));
        assert!(screen(&mut app).contains("NeetCode 250"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn marks_solved_and_hides_solved() {
        let (mut app, dir) = app("solve");
        press(&mut app, KeyCode::Char('x'));
        let progress = app.store.load_progress().unwrap();
        assert_eq!(
            progress.entries["contains-duplicate"].status,
            Status::Solved
        );
        assert!(screen(&mut app).contains("✓"));
        press(&mut app, KeyCode::Char('u'));
        assert!(!screen(&mut app).contains("Contains Duplicate"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_filters_the_list() {
        let (mut app, dir) = app("search");
        press(&mut app, KeyCode::Char('/'));
        typing(&mut app, "anagram");
        press(&mut app, KeyCode::Enter);
        let s = screen(&mut app);
        assert!(s.contains("Valid Anagram"), "{s}");
        assert!(!s.contains("Contains Duplicate"), "{s}");
        press(&mut app, KeyCode::Esc);
        assert!(screen(&mut app).contains("Contains Duplicate"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn panes_help_and_quit() {
        let (mut app, dir) = app("panes");
        for key in ['2', '3', '4', '5', '1'] {
            press(&mut app, KeyCode::Char(key));
            screen(&mut app);
        }
        press(&mut app, KeyCode::Char('?'));
        assert!(matches!(app.mode, Mode::Help));
        screen(&mut app);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn line_count_wraps_long_lines() {
        let text = format!("{}\n", "x".repeat(1000)).repeat(3);
        let para = Paragraph::new(text).wrap(Wrap { trim: false });
        assert_eq!(para.line_count(100), 30);
    }
}
