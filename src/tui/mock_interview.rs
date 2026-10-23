//! Mock interviews: the timer, the interviewer's debrief and the result.

use std::time::Instant;

use chrono::Local;

use super::{App, Confirm, Debrief, MockState, Mode, Pane};
use crate::coach::claude::{self, Kind};
use crate::coach::mock::{self, Speaker, Transcript};
use crate::model::store::MockRecord;
use crate::practice;

impl App {
    pub(super) fn start_mock(&mut self, idx: usize) {
        let p = self.problems[idx].clone();
        self.cancel_tasks();
        match practice::fresh_attempt(&self.store, &self.cfg, &p) {
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

    pub(super) fn begin_debrief(&mut self) {
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

    pub(super) fn interviewer_replied(&mut self, text: String) {
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

    pub(super) fn answer_interviewer(&mut self, answer: String) {
        if let Some(d) = self.mock.as_mut().and_then(|m| m.debrief.as_mut()) {
            d.transcript.push(Speaker::Candidate, answer);
            self.ask_interviewer();
        }
    }

    /// Records the mock (with the interviewer's verdict if there was a debrief).
    pub(super) fn complete_mock(&mut self, debriefed: bool) {
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
                practice::record_ai(&self.store, &p, Kind::Mock, "", &d.transcript.to_markdown());
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
    pub(super) fn mock_key(&mut self) {
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
}
