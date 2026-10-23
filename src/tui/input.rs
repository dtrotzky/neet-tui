//! Keyboard, mouse and paste input.

use std::process::Command;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

use super::draw::tilde;
use super::{App, Confirm, External, ListKind, Mode, PANES, Pane, Row};
use crate::coach::claude::Kind;
use crate::config::Config;
use crate::model::store::{Problem, Status};
use crate::practice;

impl App {
    pub(super) fn on_paste(&mut self, text: &str) {
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
    pub(super) fn on_mouse(&mut self, m: MouseEvent) -> bool {
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

    pub(super) fn on_key(&mut self, key: KeyEvent) -> Option<External> {
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
                        match practice::fresh_attempt(&self.store, &self.cfg, &p) {
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
                    practice::random_unsolved(&self.problems, &self.progress, include)
                } else {
                    practice::next_unsolved(
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
            KeyCode::Char(c @ ('c' | 'C')) => {
                if !self.locked() {
                    return self.selected_idx().map(|i| External::Tutor(i, c == 'C'));
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
                    let res = practice::mark_revealed(&self.store, &p);
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
                    match practice::set_solved(&self.store, &p, solve) {
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
}
