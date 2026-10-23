//! Drawing the TUI: header, problem list, panes, footer and the help popup.

use std::path::Path;

use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Tabs, Wrap,
};

use super::{App, Confirm, FLASH_FOR, ListKind, Mode, PANES, Pane, Row, RunKind, SPINNER, View};
use crate::coach::claude::Kind;
use crate::coach::mock::{self};
use crate::model::srs;
use crate::model::stats;
use crate::model::store::{Problem, Status};
use crate::practice;
use crate::render;

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

pub(super) fn panel(title: String, focused: bool) -> Block<'static> {
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

pub(super) fn tilde(path: &Path) -> String {
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
            "test against examples + edge cases / Claude writes new tests",
        ),
        ("T", "run your file as-is"),
        ("h", "nudge: question → insight → outline → pseudocode"),
        ("r", "Claude reviews your code; PASS marks it solved"),
        ("a", "ask Claude a question"),
        (
            "c / C",
            "chat with Claude: continue this problem's chat / new chat",
        ),
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

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
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
            .map(|n| practice::notes_body(&n))
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
        if log.entries.is_empty() && log.chat.is_none() && md.is_empty() {
            md.push_str(
                "No coaching yet for this problem.\n\n\
                 - `h` ask for a nudge (they escalate, never the full answer)\n\
                 - `r` get your code reviewed like an interviewer would\n\
                 - `a` ask Claude a question\n\
                 - `c` chat with Claude; quit, edit your code, and `c` continues the same chat\n\
                 - `m` do it as a timed mock interview instead\n",
            );
        }
        if let Some(chat) = &log.chat {
            md.push_str(&format!(
                "Chat started {}: `c` continues it, `C` starts a new one.\n\n---\n\n",
                chat.started.format("%b %d %H:%M")
            ));
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
                 ctrl-g   have Claude write new tests\n\
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
                        ("c", "chat"),
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
