//! Solved, opened and revealed status, and starting over.

use std::path::PathBuf;

use anyhow::Result;
use chrono::{Local, NaiveDate};

use crate::config::Config;
use crate::model::srs;
use crate::model::store::{Problem, Progress, Status, Store};

pub fn mark_opened(store: &Store, p: &Problem) -> Result<Progress> {
    store.update_progress(|progress| {
        let e = progress.entries.entry(p.slug.clone()).or_default();
        if e.status == Status::Todo {
            e.status = Status::Attempted;
        }
        e.last_opened = Some(Local::now());
    })
}

/// What marking a problem solved did, for user feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Solve {
    /// Solved for the first time; review scheduled.
    First(Option<NaiveDate>),
    /// A due review was completed; next review date (None = mastered).
    Reviewed(Option<NaiveDate>),
    /// Already solved and not due; nothing changed.
    Already,
    Unsolved,
}

impl Solve {
    pub fn describe(self, title: &str) -> String {
        match self {
            Solve::First(Some(d)) => format!("{title} solved ✓ first review on {d}"),
            Solve::First(None) => format!("{title} solved ✓"),
            Solve::Reviewed(Some(d)) => format!("review of {title} done ✓ next one on {d}"),
            Solve::Reviewed(None) => format!("review of {title} done ✓ mastered!"),
            Solve::Already => format!("{title} is already solved"),
            Solve::Unsolved => format!("{title} marked unsolved"),
        }
    }
}

/// Marks solved (scheduling spaced repetition) or unsolved.
pub fn set_solved(store: &Store, p: &Problem, solved: bool) -> Result<(Progress, Solve)> {
    let today = srs::today();
    let mut outcome = Solve::Already;
    let progress = store.update_progress(|progress| {
        let e = progress.entries.entry(p.slug.clone()).or_default();
        if !solved {
            e.status = Status::Attempted;
            e.solved_at = None;
            srs::unschedule(e);
            outcome = Solve::Unsolved;
        } else if e.status != Status::Solved {
            e.status = Status::Solved;
            e.solved_at = Some(Local::now());
            srs::schedule_first(e, today);
            outcome = Solve::First(e.next_review);
        } else if srs::is_due(e, today) {
            srs::passed(e, today);
            outcome = Solve::Reviewed(e.next_review);
        }
    })?;
    Ok((progress, outcome))
}

/// Records that the solution was viewed; while a review is due that counts as forgetting it.
pub fn mark_revealed(store: &Store, p: &Problem) -> Result<Progress> {
    let today = srs::today();
    store.update_progress(|progress| {
        let e = progress.entries.entry(p.slug.clone()).or_default();
        e.revealed = true;
        if srs::is_due(e, today) {
            srs::lapsed(e, today);
        }
    })
}

/// Moves the current solution to `attempts/` and starts over from the starter code.
/// Returns where the old attempt went, if there was one.
pub fn fresh_attempt(store: &Store, cfg: &Config, p: &Problem) -> Result<Option<PathBuf>> {
    let lang = cfg.lang();
    let sol = store.solution_path(p, lang);
    let archived = if sol.exists() {
        let dir = store.attempts_dir(lang);
        std::fs::create_dir_all(&dir)?;
        let stamp = Local::now().format("%Y%m%d-%H%M%S");
        let mut dest = dir.join(format!("{}.{stamp}.{}", p.file_stem(), lang.ext));
        let mut n = 1;
        while dest.exists() {
            dest = dir.join(format!("{}.{stamp}-{n}.{}", p.file_stem(), lang.ext));
            n += 1;
        }
        std::fs::rename(&sol, &dest)?;
        Some(dest)
    } else {
        None
    };
    store.ensure_solution(p, lang)?;
    Ok(archived)
}
