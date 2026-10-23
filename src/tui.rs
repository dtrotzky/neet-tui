use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Tabs, Wrap,
};
use ratatui::{DefaultTerminal, Frame};

use crate::actions;
use crate::claude::{self, Kind};
use crate::config::Config;
use crate::mock::{self, Speaker, Transcript};
use crate::proc::Cancel;
use crate::render;
use crate::settings::{self, Control, Setting};
use crate::srs;
use crate::stats;
use crate::store::{MockRecord, Problem, Progress, Status, Store, UiState};
use crate::testing;

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
    Tutor(usize),
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

    // ------------------------------------------------------------------
    // Background work

    /// Returns whether anything changed.
    fn poll_tasks(&mut self) -> bool {
        let mut changed = false;
        if let Some(task) = self.ai.as_mut() {
            loop {
                match task.rx.try_recv() {
                    Ok(claude::Event::Delta(d)) => {
                        task.text.push_str(&d);
                        changed = true;
                    }
                    Ok(claude::Event::Done(res)) => {
                        if let Some(task) = self.ai.take() {
                            self.finish_ai(&task, res);
                        }
                        changed = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.ai = None;
                        changed = true;
                        break;
                    }
                }
            }
        }
        if let Some(out) = self.run.as_ref().and_then(|r| r.rx.try_recv().ok())
            && let Some(run) = self.run.take()
        {
            match run.kind {
                RunKind::Tests => {
                    self.flash(testing::summary(&out));
                    self.reload_progress();
                }
                RunKind::Raw => self.flash("run finished, see Output (4)"),
            }
            self.output = Some((run.slug.clone(), out));
            changed = true;
        }
        if changed {
            self.touch();
        }
        changed
    }

    fn finish_ai(&mut self, task: &AiTask, res: Result<String>) {
        let text = match res {
            Ok(text) => text,
            Err(e) => {
                self.flash(format!("Claude {} failed: {e:#}", task.kind.label()));
                return;
            }
        };
        if task.kind == Kind::Coach {
            match std::fs::write(self.store.coach_path(), &text) {
                Ok(()) => self.flash("weak-spot analysis ready (5)"),
                Err(e) => self.flash(format!("error saving analysis: {e}")),
            }
            return;
        }
        let Some(idx) = self.problems.iter().position(|p| p.slug == task.slug) else {
            return;
        };
        let p = self.problems[idx].clone();
        match task.kind {
            Kind::Tests => match testing::save_harness(&self.store, &self.cfg, &p, &text) {
                Ok(_) => self.spawn_tests(&p),
                Err(e) => self.flash(format!("could not create tests: {e:#}")),
            },
            Kind::Mock => self.interviewer_replied(text),
            _ => {
                match actions::record_ai(&self.store, &p, task.kind, &task.question, &text) {
                    Ok((Some(true), Some(solve))) => {
                        self.flash(format!("Claude: PASS. {}", solve.describe(&p.title)));
                    }
                    Ok((Some(false), _)) => self.flash("Claude: not there yet, see the review (3)"),
                    Ok(_) => self.flash(format!("Claude {} ready (3)", task.kind.label())),
                    Err(e) => self.flash(format!("error saving response: {e:#}")),
                }
                self.reload_progress();
            }
        }
    }

    fn spawn_ai(&mut self, slug: String, kind: Kind, question: String, prompt: String) {
        let (rx, cancel) = claude::spawn(&self.cfg, kind, prompt);
        self.ai = Some(AiTask {
            slug,
            kind,
            question,
            text: String::new(),
            started: Instant::now(),
            rx,
            cancel,
        });
        self.touch();
    }

    fn ai_busy(&mut self) -> bool {
        if self.ai.is_some() {
            self.flash("Claude is still working (esc cancels)");
            return true;
        }
        false
    }

    fn start_ai(&mut self, kind: Kind, question: String) {
        if self.ai_busy() || self.locked() {
            return;
        }
        let Some(p) = self.selected().cloned() else {
            return;
        };
        if kind == Kind::Review && !self.store.solution_path(&p, self.cfg.lang()).exists() {
            self.flash("nothing to review yet, press enter to start coding");
            return;
        }
        match actions::prompt_for(&self.store, &self.cfg, &p, kind, &question) {
            Ok(prompt) => {
                self.spawn_ai(p.slug.clone(), kind, question, prompt);
                self.set_pane(Pane::Claude);
            }
            Err(e) => self.flash(format!("error: {e:#}")),
        }
    }

    fn start_coach(&mut self) {
        if self.ai_busy() {
            return;
        }
        let report = self.stats_report();
        self.spawn_ai(
            String::new(),
            Kind::Coach,
            String::new(),
            stats::coach_prompt(&report),
        );
        self.set_pane(Pane::Stats);
    }

    fn stats_report(&self) -> String {
        let mocks = self.store.load_mocks().unwrap_or_default();
        // The due list isn't a useful base for stats; use the full set then.
        let list = if self.list == ListKind::Due {
            ListKind::All
        } else {
            self.list
        };
        let include = |p: &Problem| match list {
            ListKind::Nc150 => p.nc150,
            ListKind::Nc250 => p.nc250,
            ListKind::Blind75 => p.blind75,
            _ => true,
        };
        stats::report(
            &self.problems,
            &self.progress,
            &mocks,
            list.name(),
            include,
            self.today,
        )
    }

    fn start_run(&mut self) {
        if self.run.is_some() {
            self.flash("still running (esc cancels)");
            return;
        }
        let Some(p) = self.selected().cloned() else {
            return;
        };
        let path = self.store.solution_path(&p, self.cfg.lang());
        if !path.exists() {
            self.flash("no solution file yet, press enter to start coding");
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Cancel::default();
        let job = cancel.clone();
        let cfg = self.cfg.clone();
        let store = self.store.clone();
        std::thread::spawn(move || {
            let out = actions::run_captured(&cfg, &store, &path, cfg.run_timeout(), &job)
                .unwrap_or_else(|e| format!("error: {e:#}"));
            let _ = tx.send(out);
        });
        self.run = Some(RunTask {
            slug: p.slug.clone(),
            kind: RunKind::Raw,
            rx,
            cancel,
        });
        self.set_pane(Pane::Output);
        self.touch();
    }

    /// Tests the selected solution, asking Claude for a harness first if needed.
    fn start_tests(&mut self, regenerate: bool) {
        if self.run.is_some() {
            self.flash("still running (esc cancels)");
            return;
        }
        let Some(p) = self.selected().cloned() else {
            return;
        };
        if let Err(e) = testing::ensure_has_solution(&self.store, &self.cfg, &p) {
            self.flash(format!("{e:#}"));
            return;
        }
        self.set_pane(Pane::Output);
        if regenerate || testing::load_harness(&self.store, &self.cfg, &p).is_none() {
            if self.ai_busy() {
                return;
            }
            let prompt = testing::harness_prompt(&self.store, &self.cfg, &p);
            self.spawn_ai(p.slug.clone(), Kind::Tests, String::new(), prompt);
            return;
        }
        self.spawn_tests(&p);
    }

    fn spawn_tests(&mut self, p: &Problem) {
        let (tx, rx) = mpsc::channel();
        let cancel = Cancel::default();
        let job = cancel.clone();
        let (cfg, store, problem) = (self.cfg.clone(), self.store.clone(), p.clone());
        std::thread::spawn(move || {
            let timeout = cfg.run_timeout() * 2;
            let out = match actions::run_existing_tests(&store, &cfg, &problem, timeout, &job) {
                Ok((out, _)) => out,
                Err(e) => format!("error: {e:#}"),
            };
            let _ = tx.send(out);
        });
        self.run = Some(RunTask {
            slug: p.slug.clone(),
            kind: RunKind::Tests,
            rx,
            cancel,
        });
        self.touch();
    }

    fn cancel_tasks(&mut self) -> bool {
        let any = self.ai.is_some() || self.run.is_some();
        self.ai = None;
        self.run = None;
        if any {
            self.touch();
        }
        any
    }

    // ------------------------------------------------------------------
    // Mock interviews

    fn start_mock(&mut self, idx: usize) {
        let p = self.problems[idx].clone();
        self.cancel_tasks();
        match actions::fresh_attempt(&self.store, &self.cfg, &p) {
            Ok(archived) => {
                let limit = mock::time_limit(&self.cfg, &p);
                self.mock = Some(MockState {
                    idx,
                    started_at: Local::now(),
                    clock: Instant::now(),
                    limit,
                    debrief: None,
                });
                self.select_problem(idx);
                self.set_pane(Pane::Problem);
                let note = if archived.is_some() {
                    " (old attempt archived)"
                } else {
                    ""
                };
                self.flash(format!(
                    "mock started, {} on the clock{note}. enter to code, m when done",
                    mock::fmt_duration(limit)
                ));
                self.touch();
            }
            Err(e) => self.flash(format!("could not start mock: {e:#}")),
        }
    }

    fn begin_debrief(&mut self) {
        let Some(m) = self.mock.as_mut() else {
            return;
        };
        m.debrief = Some(Debrief {
            elapsed: m.clock.elapsed(),
            transcript: Transcript::default(),
        });
        self.ask_interviewer();
    }

    fn ask_interviewer(&mut self) {
        if self.ai_busy() {
            return;
        }
        let Some(m) = self.mock.as_ref() else {
            return;
        };
        let Some(d) = m.debrief.as_ref() else {
            return;
        };
        let p = self.problems[m.idx].clone();
        let ctx = claude::Coaching::gather(&self.store, &self.cfg, &p);
        let prompt = mock::debrief_prompt(&ctx, d.elapsed, m.limit, &d.transcript);
        self.spawn_ai(p.slug.clone(), Kind::Mock, String::new(), prompt);
        self.set_pane(Pane::Claude);
    }

    fn interviewer_replied(&mut self, text: String) {
        let Some(d) = self.mock.as_mut().and_then(|m| m.debrief.as_mut()) else {
            return;
        };
        d.transcript.push(Speaker::Interviewer, text);
        if d.transcript.finished() || d.transcript.gave_up() {
            self.complete_mock(true);
        } else {
            self.mode = Mode::Answer(String::new());
        }
    }

    fn answer_interviewer(&mut self, answer: String) {
        if let Some(d) = self.mock.as_mut().and_then(|m| m.debrief.as_mut()) {
            d.transcript.push(Speaker::Candidate, answer);
            self.ask_interviewer();
        }
    }

    /// Records the mock (with the interviewer's verdict if there was a debrief).
    fn complete_mock(&mut self, debriefed: bool) {
        let Some(m) = self.mock.take() else {
            return;
        };
        let p = self.problems[m.idx].clone();
        let elapsed = m
            .debrief
            .as_ref()
            .map_or_else(|| m.clock.elapsed(), |d| d.elapsed);
        let result = m
            .debrief
            .as_ref()
            .and_then(|d| d.transcript.turns.last())
            .and_then(|(_, t)| mock::result_of(t));
        if debriefed && let Some(d) = &m.debrief {
            let _ =
                actions::record_ai(&self.store, &p, Kind::Mock, "", &d.transcript.to_markdown());
        }
        let record = MockRecord {
            slug: p.slug.clone(),
            title: p.title.clone(),
            difficulty: p.difficulty.clone(),
            started: m.started_at,
            elapsed_secs: elapsed.as_secs(),
            limit_secs: m.limit.as_secs(),
            result: result.clone(),
        };
        match self.store.add_mock(record) {
            Ok(()) => self.flash(match result {
                Some(r) => format!("mock done: {r} (details in 3 and 5)"),
                None => "mock recorded without debrief".into(),
            }),
            Err(e) => self.flash(format!("error saving mock: {e:#}")),
        }
        self.mode = Mode::Normal;
        self.touch();
    }

    /// `m`: start, finish, or continue answering, depending on the mock's phase.
    fn mock_key(&mut self) {
        match &self.mock {
            None => {
                if let Some(i) = self.selected_idx() {
                    self.mode = Mode::Confirm(Confirm::StartMock(i));
                }
            }
            Some(m) => match &m.debrief {
                None => self.mode = Mode::Confirm(Confirm::FinishMock),
                Some(d) => {
                    if self.ai.as_ref().is_some_and(|t| t.kind == Kind::Mock) {
                        self.flash("the interviewer is still talking");
                    } else if d
                        .transcript
                        .turns
                        .last()
                        .is_some_and(|(w, _)| *w == Speaker::Interviewer)
                    {
                        self.mode = Mode::Answer(String::new());
                    } else {
                        self.ask_interviewer();
                    }
                }
            },
        }
    }

    // ------------------------------------------------------------------
    // Settings

    fn settings_key(&mut self, key: KeyEvent, typed: Option<char>) {
        let Mode::Settings { cursor, input } = &mut self.mode else {
            return;
        };
        let all = settings::all();
        let cursor = *cursor;
        let setting = all[cursor];
        // Typing a value.
        if let Some(buf) = input {
            match key.code {
                KeyCode::Esc => *input = None,
                KeyCode::Enter => {
                    let text = std::mem::take(buf);
                    *input = None;
                    let mut cfg = self.cfg.clone();
                    match setting.set(&mut cfg, &text) {
                        Ok(()) => self.apply_settings(cfg),
                        Err(e) => self.flash(format!("{e:#}")),
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                _ => {
                    if let Some(c) = typed {
                        buf.push(c);
                    }
                }
            }
            return;
        }
        let set_cursor = |mode: &mut Mode, c: usize| {
            if let Mode::Settings { cursor, .. } = mode {
                *cursor = c;
            }
        };
        let start_input = |mode: &mut Mode, value: String| {
            if let Mode::Settings { input, .. } = mode {
                *input = Some(value);
            }
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(',') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                set_cursor(&mut self.mode, (cursor + 1).min(all.len() - 1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                set_cursor(&mut self.mode, cursor.saturating_sub(1))
            }
            KeyCode::Char('g') | KeyCode::Home => set_cursor(&mut self.mode, 0),
            KeyCode::Char('G') | KeyCode::End => set_cursor(&mut self.mode, all.len() - 1),
            KeyCode::Char('e') => start_input(&mut self.mode, setting.raw(&self.cfg)),
            KeyCode::Enter
            | KeyCode::Char(' ')
            | KeyCode::Char('l')
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Left => {
                let forward = !matches!(key.code, KeyCode::Char('h') | KeyCode::Left);
                let is_step = !matches!(key.code, KeyCode::Enter | KeyCode::Char(' '));
                match setting.control() {
                    Control::Text => start_input(&mut self.mode, setting.raw(&self.cfg)),
                    Control::Number if !is_step => {
                        start_input(&mut self.mode, setting.raw(&self.cfg));
                    }
                    _ => {
                        let mut cfg = self.cfg.clone();
                        setting.step(&mut cfg, forward);
                        self.apply_settings(cfg);
                    }
                }
            }
            _ => {}
        }
    }

    /// Adopts settings read from disk (no save needed).
    fn apply_loaded(&mut self, cfg: Config) {
        if cfg.workspace != self.cfg.workspace {
            match Store::new(&cfg) {
                Ok(store) => self.store = store,
                Err(e) => {
                    self.flash(format!("error: {e:#}"));
                    return;
                }
            }
        }
        self.cfg = cfg;
        self.touch();
    }

    /// Validates, saves and applies new settings; on error nothing changes.
    fn apply_settings(&mut self, cfg: Config) {
        if cfg == self.cfg {
            return;
        }
        let res = cfg.validate().and_then(|()| {
            let store = if cfg.workspace != self.cfg.workspace {
                Some(Store::new(&cfg)?)
            } else {
                None
            };
            cfg.save()?;
            Ok(store)
        });
        match res {
            Ok(store) => {
                if let Some(store) = store {
                    self.store = store;
                }
                self.cfg = cfg;
                self.touch();
                self.flash(format!("saved to {}", tilde(&Config::path())));
            }
            Err(e) => self.flash(format!("not saved: {e:#}")),
        }
    }

    fn draw_settings(&self, f: &mut Frame, cursor: usize, input: Option<&str>) {
        let area = f.area();
        let w = area.width.min(96);
        let h = area.height.min(34);
        let popup = Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height - h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, popup);
        let block = panel(" settings ".into(), true);
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        let [list_area, help_area] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(4)]).areas(inner);

        let mut items = Vec::new();
        let mut selected_item = 0;
        let mut index = 0;
        for (section, entries) in settings::SECTIONS {
            if !items.is_empty() {
                items.push(ListItem::new(""));
            }
            items.push(ListItem::new(Line::from(Span::styled(
                format!(" {section}"),
                Style::default().fg(Color::Cyan).bold(),
            ))));
            for setting in *entries {
                let editing = index == cursor && input.is_some();
                let value = match (editing, input) {
                    (true, Some(buf)) => format!("{buf}▏"),
                    _ => setting.display(&self.cfg),
                };
                let value_style = match setting {
                    Setting::Lsp if !self.cfg.editor_lsp => Style::default().fg(Color::Yellow),
                    _ if editing => Style::default().fg(Color::Magenta),
                    _ => Style::default().fg(Color::Green),
                };
                if index == cursor {
                    selected_item = items.len();
                }
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(format!("   {:<32}", setting.label())),
                    Span::styled(value, value_style),
                ])));
                index += 1;
            }
        }
        let mut state = ListState::default().with_selected(Some(selected_item));
        f.render_stateful_widget(
            List::new(items).highlight_style(
                Style::default()
                    .bg(Color::Rgb(50, 50, 70))
                    .add_modifier(Modifier::BOLD),
            ),
            list_area,
            &mut state,
        );

        let setting = settings::all()[cursor];
        let keys = match (input.is_some(), setting.control()) {
            (true, _) => "enter save · esc cancel",
            (_, Control::Toggle) => "enter toggle · j/k move · esc close",
            (_, Control::Cycle) => "enter/l next · h previous · e type a value · esc close",
            (_, Control::Number) => "enter type · l/h +5/-5 · esc close",
            (_, Control::Text) => "enter edit (empty = default) · esc close",
        };
        let help = Paragraph::new(vec![
            Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(format!(" {}", setting.help())),
            Line::from(Span::styled(
                format!(" {keys} · changes are saved to {}", tilde(&Config::path())),
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .wrap(Wrap { trim: false });
        f.render_widget(help, help_area);
    }

    fn external(&mut self, ext: External) -> Result<()> {
        match ext {
            External::Edit(i) => {
                let p = self.problems[i].clone();
                let lsp = self.cfg.lsp_enabled(self.mock.is_some());
                actions::edit(&self.store, &self.cfg, &p, lsp)?;
            }
            External::Notes(i) => {
                let p = self.problems[i].clone();
                actions::edit_notes(&self.store, &self.cfg, &p)?;
            }
            External::Tutor(i) => {
                let p = self.problems[i].clone();
                let sol = self.store.ensure_solution(&p, self.cfg.lang())?;
                let ctx = claude::Coaching::gather(&self.store, &self.cfg, &p);
                let sys = claude::tutor_system_prompt(&ctx, &sol.display().to_string());
                let mut cmd = Command::new(&self.cfg.claude_cmd);
                cmd.current_dir(sol.parent().unwrap_or(&self.store.workspace))
                    .args(["--append-system-prompt", &sys]);
                if let Some(m) = self.cfg.model_for(Kind::Chat) {
                    cmd.args(["--model", m]);
                }
                cmd.arg(format!(
                    "I'm working on \"{}\". Briefly ask me how I'm thinking about approaching it.",
                    p.title
                ));
                cmd.status()
                    .with_context(|| format!("failed to start `{}`", self.cfg.claude_cmd))?;
            }
        }
        self.touch();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Input

    fn on_paste(&mut self, text: &str) {
        let text = text.replace(['\r', '\n'], " ");
        match &mut self.mode {
            Mode::Search => {
                self.search.push_str(&text);
                self.rebuild(None);
            }
            Mode::Ask(buf)
            | Mode::Answer(buf)
            | Mode::Settings {
                input: Some(buf), ..
            } => buf.push_str(&text),
            _ => {}
        }
    }

    /// Returns whether anything changed.
    fn on_mouse(&mut self, m: MouseEvent) -> bool {
        let pos = Position::new(m.column, m.row);
        let in_list = self.list_area.contains(pos);
        let in_pane = self.pane_area.contains(pos);
        match m.kind {
            MouseEventKind::ScrollDown if in_list => self.move_by(1),
            MouseEventKind::ScrollUp if in_list => self.move_by(-1),
            MouseEventKind::ScrollDown if in_pane => self.scroll = self.scroll.saturating_add(3),
            MouseEventKind::ScrollUp if in_pane => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::Down(MouseButton::Left) if in_list => {
                // Row 0 is the border; the list may be scrolled.
                let offset = m.row.saturating_sub(self.list_area.y + 1) as usize;
                let row = self.list_state.offset() + offset;
                if matches!(self.rows.get(row), Some(Row::Item(_))) {
                    self.list_state.select(Some(row));
                    self.scroll = 0;
                }
            }
            _ => return false,
        }
        true
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<External> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Text input only takes plain (or shifted) characters.
        let typed = |key: &KeyEvent| match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(c)
            }
            _ => None,
        };
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        let answering = matches!(self.mode, Mode::Answer(_));
        if let Mode::Settings { .. } = self.mode {
            self.settings_key(key, typed(&key));
            return None;
        }
        match &mut self.mode {
            Mode::Help => {
                self.mode = Mode::Normal;
                return None;
            }
            Mode::Confirm(_) => {
                let Mode::Confirm(c) = std::mem::replace(&mut self.mode, Mode::Normal) else {
                    return None;
                };
                match (c, key.code) {
                    (Confirm::StartMock(i), KeyCode::Char('y')) => self.start_mock(i),
                    (Confirm::FinishMock, KeyCode::Char('y')) => self.begin_debrief(),
                    (Confirm::FinishMock, KeyCode::Char('s')) => self.complete_mock(false),
                    (Confirm::Quit, KeyCode::Char('y')) => self.quit = true,
                    (Confirm::Fresh(i), KeyCode::Char('y')) => {
                        let p = self.problems[i].clone();
                        match actions::fresh_attempt(&self.store, &self.cfg, &p) {
                            Ok(Some(old)) => {
                                self.flash(format!("fresh start; old attempt in {}", tilde(&old)))
                            }
                            Ok(None) => self.flash("fresh starter file created"),
                            Err(e) => self.flash(format!("error: {e:#}")),
                        }
                        self.touch();
                    }
                    _ => {}
                }
                return None;
            }
            Mode::Search => {
                match key.code {
                    KeyCode::Esc => {
                        self.search.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        self.search.pop();
                    }
                    KeyCode::Down => self.move_by(1),
                    KeyCode::Up => self.move_by(-1),
                    _ => {
                        if let Some(c) = typed(&key) {
                            self.search.push(c);
                        }
                    }
                }
                self.rebuild(None);
                return None;
            }
            Mode::Ask(buf) | Mode::Answer(buf) => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Normal,
                    KeyCode::Enter => {
                        let (Mode::Ask(text) | Mode::Answer(text)) =
                            std::mem::replace(&mut self.mode, Mode::Normal)
                        else {
                            return None;
                        };
                        if text.trim().is_empty() {
                            if answering {
                                self.mode = Mode::Answer(text);
                            }
                        } else if answering {
                            self.answer_interviewer(text);
                        } else {
                            self.start_ai(Kind::Ask, text);
                        }
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    _ => {
                        if let Some(c) = typed(&key) {
                            buf.push(c);
                        }
                    }
                }
                return None;
            }
            // Handled above.
            Mode::Settings { .. } | Mode::Normal => {}
        }

        match key.code {
            KeyCode::Char('q') => {
                if self.mock.is_some() {
                    self.mode = Mode::Confirm(Confirm::Quit);
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Esc => {
                if self.cancel_tasks() {
                    self.flash("cancelled");
                } else if !self.search.is_empty() {
                    self.search.clear();
                    self.rebuild(None);
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char(',') => {
                // Start from the file on disk, in case it was edited by hand meanwhile.
                match Config::load() {
                    Ok(cfg) if cfg != self.cfg => {
                        self.apply_loaded(cfg);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.flash(format!("config file has an error, fix it first: {e:#}"));
                        return None;
                    }
                }
                self.mode = Mode::Settings {
                    cursor: 0,
                    input: None,
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll = self.scroll.saturating_add(15),
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(15),
            KeyCode::Char('g') if ctrl => self.start_tests(true),
            KeyCode::Char('J') | KeyCode::PageDown => self.scroll = self.scroll.saturating_add(3),
            KeyCode::Char('K') | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(3),
            KeyCode::Char('g') | KeyCode::Home => self.select_edge(false),
            KeyCode::Char('G') | KeyCode::End => self.select_edge(true),
            KeyCode::Char(c @ '1'..='5') => self.set_pane(PANES[(c as u8 - b'1') as usize]),
            KeyCode::Tab | KeyCode::BackTab => {
                let i = PANES.iter().position(|p| *p == self.pane).unwrap_or(0);
                let n = PANES.len();
                let next = if key.code == KeyCode::Tab {
                    i + 1
                } else {
                    i + n - 1
                };
                self.set_pane(PANES[next % n]);
            }
            KeyCode::Char('l') => {
                self.list = self.list.next();
                self.rebuild(None);
                self.flash(format!("list: {}", self.list.name()));
            }
            KeyCode::Char('D') => {
                self.list = if self.list == ListKind::Due {
                    ListKind::Nc150
                } else {
                    ListKind::Due
                };
                self.rebuild(None);
                self.flash(format!("list: {}", self.list.name()));
            }
            KeyCode::Char('/') => {
                self.search.clear();
                self.mode = Mode::Search;
            }
            KeyCode::Char('u') => {
                self.hide_solved = !self.hide_solved;
                self.rebuild(None);
                self.flash(if self.hide_solved {
                    "hiding solved"
                } else {
                    "showing solved"
                });
            }
            KeyCode::Char('n') | KeyCode::Char('R') => {
                let random = key.code == KeyCode::Char('R');
                // Picking from the due list makes no sense (it is all solved).
                let base = if self.list == ListKind::Due {
                    ListKind::Nc150
                } else {
                    self.list
                };
                let include = |p: &Problem| match base {
                    ListKind::Nc150 => p.nc150,
                    ListKind::Nc250 => p.nc250,
                    ListKind::Blind75 => p.blind75,
                    _ => true,
                };
                let pick = if random {
                    actions::random_unsolved(&self.problems, &self.progress, include)
                } else {
                    actions::next_unsolved(
                        &self.problems,
                        &self.progress,
                        include,
                        self.selected_idx(),
                    )
                };
                match pick {
                    Some(i) => {
                        self.select_problem(i);
                        self.set_pane(Pane::Problem);
                    }
                    None => self.flash("everything here is solved"),
                }
            }
            KeyCode::Enter | KeyCode::Char('e') => return self.selected_idx().map(External::Edit),
            KeyCode::Char('N') => return self.selected_idx().map(External::Notes),
            KeyCode::Char('c') => {
                if !self.locked() {
                    return self.selected_idx().map(External::Tutor);
                }
            }
            KeyCode::Char('t') => self.start_tests(false),
            KeyCode::Char('T') => self.start_run(),
            KeyCode::Char('h') => self.start_ai(Kind::Hint, String::new()),
            KeyCode::Char('r') => self.start_ai(Kind::Review, String::new()),
            KeyCode::Char('a') => {
                if !self.locked() {
                    self.mode = Mode::Ask(String::new());
                }
            }
            KeyCode::Char('m') => self.mock_key(),
            KeyCode::Char('W') => self.start_coach(),
            KeyCode::Char('F') => {
                if !self.locked()
                    && let Some(i) = self.selected_idx()
                {
                    self.mode = Mode::Confirm(Confirm::Fresh(i));
                }
            }
            KeyCode::Char('s') => self.set_pane(Pane::Solution),
            KeyCode::Char('y') if self.pane == Pane::Solution => {
                if self.locked() {
                    return None;
                }
                if let Some(p) = self.selected().cloned() {
                    let was_due = self.is_due(&p);
                    let res = actions::mark_revealed(&self.store, &p);
                    self.set_progress(res);
                    if was_due {
                        self.flash("solution viewed during a review: review restarts tomorrow");
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(p) = self.selected().cloned() {
                    // Solved and not due: toggle off. Otherwise solve / complete the review.
                    let solve = self.status_of(&p) != Status::Solved || self.is_due(&p);
                    match actions::set_solved(&self.store, &p, solve) {
                        Ok((progress, outcome)) => {
                            self.set_progress(Ok(progress));
                            self.flash(outcome.describe(&p.title));
                        }
                        Err(e) => self.flash(format!("error: {e:#}")),
                    }
                }
            }
            KeyCode::Char('o') => {
                if let Some(p) = self.selected() {
                    let url = match &p.video {
                        Some(v) if !v.is_empty() => format!("https://www.youtube.com/watch?v={v}"),
                        _ => p.neetcode_url(),
                    };
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let res = Command::new(opener)
                        .arg(&url)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                    match res {
                        Ok(_) => self.flash(format!("opening {url}")),
                        Err(e) => self.flash(format!("{opener} failed: {e}")),
                    }
                }
            }
            _ => {}
        }
        None
    }

    // ------------------------------------------------------------------
    // Drawing

    fn draw(&mut self, f: &mut Frame) {
        if self
            .status
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= FLASH_FOR)
        {
            self.status = None;
        }
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .areas(f.area());
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)])
                .areas(body);
        self.list_area = left;
        self.pane_area = right;

        self.draw_header(f, header);
        self.draw_list(f, left);
        self.draw_pane(f, right);
        self.draw_footer(f, footer);
        match &self.mode {
            Mode::Help => draw_help(f),
            Mode::Settings { cursor, input } => self.draw_settings(f, *cursor, input.as_deref()),
            _ => {}
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let in_list: Vec<&Problem> = self.problems.iter().filter(|p| self.in_list(p)).collect();
        let solved = |d: Option<&str>| {
            in_list
                .iter()
                .filter(|p| d.is_none_or(|d| p.difficulty == d))
                .filter(|p| self.status_of(p) == Status::Solved)
                .count()
        };
        let total = |d: &str| in_list.iter().filter(|p| p.difficulty == d).count();
        let mut spans = vec![
            Span::styled(
                " neet ",
                Style::default().bg(Color::Magenta).fg(Color::Black).bold(),
            ),
            Span::raw("  "),
            Span::styled(self.list.name(), Style::default().bold()),
            Span::raw(format!("  ✓ {}/{}", solved(None), in_list.len())),
            Span::raw("   "),
        ];
        for (d, short, color) in [
            ("Easy", "E", Color::Green),
            ("Medium", "M", Color::Yellow),
            ("Hard", "H", Color::Red),
        ] {
            spans.push(Span::styled(
                format!("{short} {}/{}  ", solved(Some(d)), total(d)),
                Style::default().fg(color),
            ));
        }
        let due = self.due_count();
        if due > 0 {
            spans.push(Span::styled(
                format!(" ⟳ {due} due "),
                Style::default().fg(Color::Cyan).bold(),
            ));
        }
        if let Some(m) = &self.mock {
            let elapsed = m
                .debrief
                .as_ref()
                .map_or_else(|| m.clock.elapsed(), |d| d.elapsed);
            let over = elapsed > m.limit;
            let phase = if m.debrief.is_some() {
                "debrief"
            } else {
                "coding"
            };
            spans.push(Span::styled(
                format!(
                    " MOCK {phase} {} / {} ",
                    mock::fmt_duration(elapsed),
                    mock::fmt_duration(m.limit)
                ),
                Style::default()
                    .bg(if over { Color::Red } else { Color::Blue })
                    .fg(Color::White)
                    .bold(),
            ));
        }
        spans.push(Span::styled(
            format!(" {} ", self.cfg.language),
            Style::default().fg(Color::DarkGray),
        ));
        if self.hide_solved {
            spans.push(Span::styled(
                " [hiding solved]",
                Style::default().fg(Color::DarkGray),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| match row {
                Row::Header {
                    pattern,
                    solved,
                    total,
                } => ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{pattern} "),
                        Style::default().fg(Color::Cyan).bold(),
                    ),
                    Span::styled(
                        format!("{solved}/{total}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])),
                Row::Item(i) => {
                    let p = &self.problems[*i];
                    let entry = self.progress.entries.get(&p.slug);
                    let (icon, icolor) = match entry.map(|e| e.status).unwrap_or_default() {
                        Status::Solved if self.is_due(p) => ("⟳", Color::Cyan),
                        Status::Solved => ("✓", Color::Green),
                        Status::Attempted => ("●", Color::Yellow),
                        Status::Todo => ("·", Color::DarkGray),
                    };
                    let (short, dcolor) = match p.difficulty.as_str() {
                        "Easy" => ("E", Color::Green),
                        "Hard" => ("H", Color::Red),
                        _ => ("M", Color::Yellow),
                    };
                    let mut spans = vec![
                        Span::styled(format!("  {icon} "), Style::default().fg(icolor)),
                        Span::styled(short, Style::default().fg(dcolor)),
                        Span::raw(format!(" {}", p.title)),
                    ];
                    if entry.is_some_and(|e| e.revealed) {
                        spans.push(Span::styled(" ◌", Style::default().fg(Color::DarkGray)));
                    }
                    ListItem::new(Line::from(spans))
                }
            })
            .collect();
        let title = if matches!(self.mode, Mode::Search) || !self.search.is_empty() {
            format!(" /{} ", self.search)
        } else {
            format!(" {} ", self.list.name())
        };
        let list = List::new(items).block(panel(title, true)).highlight_style(
            Style::default()
                .bg(Color::Rgb(50, 50, 70))
                .add_modifier(Modifier::BOLD),
        );
        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    /// Which pane shows the given Claude task.
    fn pane_of(kind: Kind) -> Pane {
        match kind {
            Kind::Tests => Pane::Output,
            Kind::Coach => Pane::Stats,
            _ => Pane::Claude,
        }
    }

    fn draw_pane(&mut self, f: &mut Frame, area: Rect) {
        let [tabs_area, content] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        let spinner = spinner();
        let titles: Vec<Line> = PANES
            .iter()
            .map(|p| {
                let busy = self
                    .ai
                    .as_ref()
                    .is_some_and(|t| Self::pane_of(t.kind) == *p)
                    || (*p == Pane::Output && self.run.is_some());
                if busy {
                    Line::from(format!("{} {spinner}", p.title()))
                } else {
                    Line::from(p.title())
                }
            })
            .collect();
        let sel = PANES.iter().position(|p| *p == self.pane).unwrap_or(0);
        f.render_widget(
            Tabs::new(titles)
                .select(sel)
                .highlight_style(Style::default().fg(Color::Magenta).bold())
                .divider("│"),
            tabs_area,
        );

        let Some(idx) = self.selected_idx() else {
            let msg = if self.list == ListKind::Due {
                "Nothing due for review today."
            } else {
                "No problems match."
            };
            f.render_widget(
                Paragraph::new(msg).block(panel(String::new(), false)),
                content,
            );
            return;
        };
        let key = (idx, self.pane, self.version);
        if self.view.as_ref().is_none_or(|v| v.key != key) {
            let p = &self.problems[idx];
            let text = match self.pane {
                Pane::Problem => self.problem_text(p),
                Pane::Solution => self.solution_text(p),
                Pane::Claude => self.claude_text(p),
                Pane::Output => self.output_text(p),
                Pane::Stats => self.stats_text(),
            };
            self.view = Some(View {
                key,
                text,
                lines: None,
            });
        }

        let mut title = if self.pane == Pane::Stats {
            " Progress ".to_string()
        } else {
            format!(" {} ", self.problems[idx].title)
        };
        if let Some(task) = self
            .ai
            .as_ref()
            .filter(|t| Self::pane_of(t.kind) == self.pane)
        {
            title.push_str(&format!(
                "· {spinner} {} {}s · esc cancels ",
                task.kind.label(),
                task.started.elapsed().as_secs()
            ));
        }
        let Some(view) = self.view.as_mut() else {
            return;
        };
        let para = Paragraph::new(view.text.clone()).wrap(Wrap { trim: false });
        // Clamp scrolling to the wrapped content height.
        let inner = content.inner(Margin::new(1, 1));
        let lines = match view.lines {
            Some((w, n)) if w == inner.width => n,
            _ => {
                let n = para.line_count(inner.width);
                view.lines = Some((inner.width, n));
                n
            }
        };
        let max_scroll = lines.saturating_sub(inner.height as usize);
        self.scroll = self
            .scroll
            .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
        f.render_widget(
            para.block(panel(title, false)).scroll((self.scroll, 0)),
            content,
        );
        if max_scroll > 0 {
            let mut state = ScrollbarState::new(max_scroll).position(self.scroll as usize);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                content.inner(Margin::new(0, 1)),
                &mut state,
            );
        }
    }

    fn problem_text(&self, p: &Problem) -> Text<'static> {
        let mut lines = match self.store.load_description(&p.slug) {
            Some(d) => render::markdown(&d),
            None => vec![Line::from("No statement cached, run `neet sync`.")],
        };
        let dim = Style::default().fg(Color::DarkGray);
        if let Some(notes) = self
            .store
            .load_notes(p)
            .map(|n| actions::notes_body(&n))
            .filter(|n| !n.is_empty())
        {
            lines.push(Line::raw(""));
            lines.extend(render::markdown(&format!("## Your notes\n\n{notes}")));
        }
        let path = self.store.solution_path(p, self.cfg.lang());
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("─".repeat(48), dim)));
        let file_note = if path.exists() {
            "your solution: "
        } else {
            "will be created at: "
        };
        lines.push(Line::from(vec![
            Span::styled(file_note, dim),
            Span::raw(tilde(&path)),
        ]));
        if let Some(e) = self.progress.entries.get(&p.slug) {
            let mut bits = Vec::new();
            if let Some(t) = e.last_test {
                bits.push(format!(
                    "tests {}/{} ({})",
                    t.passed,
                    t.total,
                    t.at.format("%b %d")
                ));
            }
            if e.hints_used > 0 {
                bits.push(format!("{} nudge(s)", e.hints_used));
            }
            if e.revealed {
                bits.push("solution viewed".into());
            }
            if e.time_spent_secs > 0 {
                bits.push(format!("{} spent", stats::fmt_time(e.time_spent_secs)));
            }
            if let Some(at) = e.solved_at {
                bits.push(format!("solved {}", at.format("%Y-%m-%d")));
            }
            if let Some(l) = srs::label(e, self.today) {
                bits.push(l);
            }
            if !bits.is_empty() {
                lines.push(Line::from(Span::styled(bits.join(" · "), dim)));
            }
        }
        Text::from(lines)
    }

    fn solution_text(&self, p: &Problem) -> Text<'static> {
        if self.mock.is_some() {
            return Text::from("\n  Solutions are hidden during a mock interview.");
        }
        let revealed = self
            .progress
            .entries
            .get(&p.slug)
            .is_some_and(|e| e.revealed);
        if !revealed {
            let mut lines = vec![
                Line::raw(""),
                Line::from("  The NeetCode solution is hidden.".bold()),
                Line::raw(""),
                Line::from("  Press y to reveal it (it will be recorded as viewed)."),
                Line::from("  Stuck? Press h for a nudge from Claude instead."),
            ];
            if self.is_due(p) {
                lines.push(Line::from(
                    "  This problem is due for review: revealing restarts its schedule.",
                ));
            }
            return Text::from(lines);
        }
        let lang = self.cfg.lang();
        if let Some(article) = self.store.load_article(&p.slug) {
            return Text::from(render::markdown(&render::filter_article(&article, lang)));
        }
        if let Some(code) = self.store.load_ref_solution(&p.slug, lang) {
            return Text::from(render::markdown(&format!("```{}\n{code}\n```", lang.key)));
        }
        Text::from("No NeetCode solution available offline for this problem in your language.")
    }

    fn claude_text(&self, p: &Problem) -> Text<'static> {
        let mut md = String::new();
        let debrief = self
            .mock
            .as_ref()
            .filter(|m| {
                m.idx
                    == self
                        .problems
                        .iter()
                        .position(|q| q.slug == p.slug)
                        .unwrap_or(usize::MAX)
            })
            .and_then(|m| m.debrief.as_ref());
        if let Some(d) = debrief {
            md.push_str("# Mock interview debrief\n\n");
            md.push_str(&d.transcript.to_markdown());
            md.push_str("\n\n");
            match &self.ai {
                Some(t) if t.kind == Kind::Mock => {
                    md.push_str(&format!("**Interviewer:** {}\n\n", t.text));
                }
                _ if matches!(self.mode, Mode::Answer(_)) => {
                    md.push_str(&format!(
                        "*Type your answer below, enter to send ({} to wrap up).*\n\n",
                        mock::DONE
                    ));
                }
                _ => md.push_str("*Press m to continue the debrief.*\n\n"),
            }
            md.push_str("---\n\n");
        } else if let Some(task) = self
            .ai
            .as_ref()
            .filter(|t| t.slug == p.slug && Self::pane_of(t.kind) == Pane::Claude)
        {
            md.push_str(&format!(
                "# Claude is working on a {}...\n\n{}\n\n---\n\n",
                task.kind.label(),
                task.text
            ));
        }
        let log = match self.store.load_ai(&p.slug) {
            Ok(log) => log,
            Err(e) => {
                md.push_str(&format!("Could not read coaching history: {e:#}\n"));
                Default::default()
            }
        };
        if log.entries.is_empty() && md.is_empty() {
            md.push_str(
                "No coaching yet for this problem.\n\n\
                 - `h` ask for a nudge (they escalate, never the full answer)\n\
                 - `r` get your code reviewed like an interviewer would\n\
                 - `a` ask Claude a question\n\
                 - `c` open an interactive tutoring session\n\
                 - `m` do it as a timed mock interview instead\n",
            );
        }
        for e in log.entries.iter().rev() {
            md.push_str(&format!(
                "# {} · {}\n\n{}\n\n---\n\n",
                e.kind,
                e.at.format("%b %d %H:%M"),
                e.text.trim()
            ));
        }
        Text::from(render::markdown(&md))
    }

    fn output_text(&self, p: &Problem) -> Text<'static> {
        if self
            .ai
            .as_ref()
            .is_some_and(|t| t.slug == p.slug && t.kind == Kind::Tests)
        {
            return Text::from(
                "Claude is writing tests for this problem (only needed once)... (esc cancels)",
            );
        }
        if let Some(run) = self.run.as_ref().filter(|r| r.slug == p.slug) {
            return Text::from(match run.kind {
                RunKind::Tests => "running tests... (esc cancels)",
                RunKind::Raw => "running... (esc cancels)",
            });
        }
        match &self.output {
            Some((slug, out)) if *slug == p.slug => colorize_output(out),
            _ => Text::from(format!(
                "t        test against the examples + edge cases (Claude writes the tests once)\n\
                 ctrl-g   regenerate the tests\n\
                 T        run the file as-is: {}\n\n\
                 Tests are cached in {}",
                self.cfg.run_cmd(),
                tilde(&self.store.harness_path(p, self.cfg.lang()))
            )),
        }
    }

    fn stats_text(&self) -> Text<'static> {
        let mut md = format!("```\n{}```\n\n", self.stats_report());
        if let Some(task) = self.ai.as_ref().filter(|t| t.kind == Kind::Coach) {
            md.push_str(&format!("## Weak-spot analysis\n\n{}\n", task.text));
        } else if let Ok(coach) = std::fs::read_to_string(self.store.coach_path()) {
            let when = std::fs::metadata(self.store.coach_path())
                .and_then(|m| m.modified())
                .map(|t| DateTime::<Local>::from(t).format(" (%b %d)").to_string())
                .unwrap_or_default();
            md.push_str(&format!(
                "## Weak-spot analysis{when}\n\n{}\n\n*W to refresh.*\n",
                coach.trim()
            ));
        } else {
            md.push_str("Press W for a weak-spot analysis and study plan from Claude.\n");
        }
        Text::from(render::markdown(&md))
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let prompt = |label: &str, buf: &str, hint: &str| {
            Line::from(vec![
                Span::styled(format!(" {label}: "), Style::default().fg(Color::Magenta)),
                Span::raw(format!("{buf}▏")),
                Span::styled(format!("  {hint}"), Style::default().fg(Color::DarkGray)),
            ])
        };
        let question = |q: String| {
            Line::from(Span::styled(
                format!(" {q}"),
                Style::default().fg(Color::Yellow).bold(),
            ))
        };
        let line = match &self.mode {
            Mode::Search => prompt("search", &self.search, "enter keep · esc clear"),
            Mode::Ask(buf) => prompt("ask Claude", buf, "enter send · esc cancel"),
            Mode::Answer(buf) => prompt(
                "your answer",
                buf,
                &format!("enter send · {} wrap up · esc later (m)", mock::DONE),
            ),
            Mode::Confirm(Confirm::StartMock(i)) => {
                let p = &self.problems[*i];
                question(format!(
                    "Start a {} mock on {}? Hints and solutions lock, your current file is archived. y/n",
                    mock::fmt_duration(mock::time_limit(&self.cfg, p)),
                    p.title
                ))
            }
            Mode::Confirm(Confirm::FinishMock) => question(
                "Done coding? y = debrief with the interviewer · s = skip debrief · n = keep going"
                    .into(),
            ),
            Mode::Confirm(Confirm::Quit) => question(
                "A mock interview is in progress and won't be recorded. Quit anyway? y/n".into(),
            ),
            Mode::Confirm(Confirm::Fresh(i)) => question(format!(
                "Archive your current {} solution and start over? y/n",
                self.problems[*i].title
            )),
            _ => match &self.status {
                Some((msg, _)) => Line::from(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(Color::Yellow),
                )),
                None => {
                    let keys = [
                        ("⏎", "code"),
                        ("t", "test"),
                        ("h", "nudge"),
                        ("r", "review"),
                        ("m", "mock"),
                        ("n", "next"),
                        ("D", "due"),
                        ("s", "solution"),
                        ("x", "solved"),
                        ("N", "notes"),
                        (",", "settings"),
                        ("?", "help"),
                        ("q", "quit"),
                    ];
                    let mut spans = vec![Span::raw(" ")];
                    for (k, d) in keys {
                        spans.push(Span::styled(k, Style::default().fg(Color::Magenta).bold()));
                        spans.push(Span::styled(
                            format!(" {d}  "),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    Line::from(spans)
                }
            },
        };
        f.render_widget(Paragraph::new(line), area);
    }
}

/// Colors test results: PASS green, FAIL red, RESULT bold.
fn colorize_output(out: &str) -> Text<'static> {
    Text::from(
        out.lines()
            .map(|l| {
                let style = if l.starts_with("PASS") {
                    Style::default().fg(Color::Green)
                } else if l.starts_with("FAIL") {
                    Style::default().fg(Color::Red)
                } else if l.starts_with("RESULT") {
                    Style::default().bold()
                } else if l.starts_with('$') || l.starts_with('[') {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(l.to_string(), style))
            })
            .collect::<Vec<_>>(),
    )
}

fn spinner() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[(ms / 90) as usize % SPINNER.len()]
}

fn panel(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            Color::Magenta
        } else {
            Color::DarkGray
        }))
        .title(title)
}

fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

fn draw_help(f: &mut Frame) {
    let rows = [
        (
            "j/k ↓/↑ g/G, mouse",
            "move through problems (wheel / click)",
        ),
        ("J/K, ctrl-d/u", "scroll the right pane"),
        ("1-5, tab", "pane: problem, solution, claude, output, stats"),
        ("enter / e", "open your solution in your editor"),
        (
            "t / ctrl-g",
            "test against examples + edge cases / regenerate tests",
        ),
        ("T", "run your file as-is"),
        ("h", "nudge: question → insight → outline → pseudocode"),
        ("r", "Claude reviews your code; PASS marks it solved"),
        ("a / c", "ask Claude a question / interactive tutor"),
        ("m", "mock interview: timed, no help, then a debrief"),
        ("n / R", "next unsolved / random unsolved problem"),
        ("D", "toggle the due-for-review list"),
        ("x", "mark solved, or complete a due review"),
        ("s, then y", "reveal the NeetCode solution"),
        ("N", "edit your notes for this problem"),
        ("F", "archive your solution and start fresh"),
        ("W", "weak-spot analysis from Claude (stats pane)"),
        ("l / u / /", "cycle list / hide solved / search"),
        ("o", "open the NeetCode video in the browser"),
        (
            ",",
            "settings (LSP / interview mode, language, models, ...)",
        ),
        ("esc / q", "cancel Claude or a run, clear search / quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(
                    format!(" {k:<22}"),
                    Style::default().fg(Color::Magenta).bold(),
                ),
                Span::raw(*d),
            ])
        })
        .collect();
    let area = f.area();
    let w = area.width.min(90);
    let h = (rows.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(panel(" keys (any key to close) ".into(), true)),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
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
        std::fs::write(store.problems_path(), include_str!("../data/problems.json")).unwrap();
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
