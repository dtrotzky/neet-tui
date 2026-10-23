//! Background work: Claude requests, runs and tests, polled from the event loop.

use std::sync::mpsc::{self, TryRecvError};
use std::time::Instant;

use anyhow::Result;

use super::{AiTask, App, ListKind, Pane, RunKind, RunTask};
use crate::coach::claude::{self, Kind};
use crate::judge::harness;
use crate::judge::proc::Cancel;
use crate::judge::run;
use crate::model::stats;
use crate::model::store::Problem;
use crate::practice;

impl App {
    /// Returns whether anything changed.
    pub(super) fn poll_tasks(&mut self) -> bool {
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
                    self.flash(harness::summary(&out));
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
            Kind::Tests => match harness::save_harness(&self.store, &self.cfg, &p, &text) {
                Ok(_) => self.spawn_tests(&p, harness::Source::Claude),
                Err(e) => self.flash(format!("could not create tests: {e:#}")),
            },
            Kind::Mock => self.interviewer_replied(text),
            _ => {
                match practice::record_ai(&self.store, &p, task.kind, &task.question, &text) {
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

    pub(super) fn spawn_ai(&mut self, slug: String, kind: Kind, question: String, prompt: String) {
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

    pub(super) fn ai_busy(&mut self) -> bool {
        if self.ai.is_some() {
            self.flash("Claude is still working (esc cancels)");
            return true;
        }
        false
    }

    pub(super) fn start_ai(&mut self, kind: Kind, question: String) {
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
        match practice::prompt_for(&self.store, &self.cfg, &p, kind, &question) {
            Ok(prompt) => {
                self.spawn_ai(p.slug.clone(), kind, question, prompt);
                self.set_pane(Pane::Claude);
            }
            Err(e) => self.flash(format!("error: {e:#}")),
        }
    }

    pub(super) fn start_coach(&mut self) {
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

    pub(super) fn stats_report(&self) -> String {
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

    pub(super) fn start_run(&mut self) {
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
            let out = run::run_captured(&cfg, &store, &path, cfg.run_timeout(), &job)
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
    pub(super) fn start_tests(&mut self, regenerate: bool) {
        if self.run.is_some() {
            self.flash("still running (esc cancels)");
            return;
        }
        let Some(p) = self.selected().cloned() else {
            return;
        };
        if let Err(e) = harness::ensure_has_solution(&self.store, &self.cfg, &p) {
            self.flash(format!("{e:#}"));
            return;
        }
        self.set_pane(Pane::Output);
        if harness::needs_claude(&self.store, &self.cfg, &p, regenerate) {
            if self.ai_busy() {
                return;
            }
            let prompt = harness::harness_prompt(&self.store, &self.cfg, &p);
            self.spawn_ai(p.slug.clone(), Kind::Tests, String::new(), prompt);
            return;
        }
        self.spawn_tests(&p, harness::Source::Auto);
    }

    fn spawn_tests(&mut self, p: &Problem, source: harness::Source) {
        let (tx, rx) = mpsc::channel();
        let cancel = Cancel::default();
        let job = cancel.clone();
        let (cfg, store, problem) = (self.cfg.clone(), self.store.clone(), p.clone());
        std::thread::spawn(move || {
            let timeout = cfg.run_timeout() * 2;
            let out = match run::run_existing_tests(&store, &cfg, &problem, source, timeout, &job) {
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

    pub(super) fn cancel_tasks(&mut self) -> bool {
        let any = self.ai.is_some() || self.run.is_some();
        self.ai = None;
        self.run = None;
        if any {
            self.touch();
        }
        any
    }
}
