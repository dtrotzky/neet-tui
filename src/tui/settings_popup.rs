//! The settings popup (`,`): editing, saving and applying settings.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::draw::{panel, tilde};
use super::{App, Mode};
use crate::config::Config;
use crate::config::settings::{self, Control, Setting};
use crate::model::store::Store;

impl App {
    pub(super) fn settings_key(&mut self, key: KeyEvent, typed: Option<char>) {
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
    pub(super) fn apply_loaded(&mut self, cfg: Config) {
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

    pub(super) fn draw_settings(&self, f: &mut Frame, cursor: usize, input: Option<&str>) {
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
}
