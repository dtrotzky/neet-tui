//! Prompts for Claude's coaching and the per-problem coaching log.

use anyhow::{Result, bail};
use chrono::Local;

use super::progress::{Solve, set_solved};
use crate::coach::claude::{self, Kind};
use crate::config::Config;
use crate::judge::harness;
use crate::model::store::{AiEntry, Problem, Store};

/// Builds the prompt for a coaching request.
pub fn prompt_for(
    store: &Store,
    cfg: &Config,
    p: &Problem,
    kind: Kind,
    question: &str,
) -> Result<String> {
    let ctx = claude::Coaching::gather(store, cfg, p);
    Ok(match kind {
        Kind::Hint => {
            let level = (store.load_ai(&p.slug)?.hint_count() + 1).min(claude::MAX_HINT_LEVEL);
            claude::hint_prompt(&ctx, level)
        }
        Kind::Review => claude::review_prompt(&ctx),
        Kind::Ask => claude::ask_prompt(&ctx, question),
        Kind::Tests => harness::harness_prompt(store, cfg, p),
        other => bail!("{} prompts are built elsewhere", other.label()),
    })
}

/// Stores a finished coaching response. For reviews returns Claude's verdict and,
/// on PASS, what marking the problem solved did.
pub fn record_ai(
    store: &Store,
    p: &Problem,
    kind: Kind,
    question: &str,
    text: &str,
) -> Result<(Option<bool>, Option<Solve>)> {
    let mut log = store.load_ai(&p.slug)?;
    let label = match kind {
        Kind::Ask => format!("ask: {question}"),
        k => k.label().to_string(),
    };
    log.entries.push(AiEntry {
        kind: label,
        at: Local::now(),
        text: text.to_string(),
    });
    store.save_ai(&p.slug, &log)?;
    match kind {
        Kind::Hint => {
            store.update_progress(|progress| {
                progress
                    .entries
                    .entry(p.slug.clone())
                    .or_default()
                    .hints_used += 1;
            })?;
            Ok((None, None))
        }
        Kind::Review => {
            let verdict = claude::verdict(text);
            let solve = match verdict {
                Some(true) => Some(set_solved(store, p, true)?.1),
                _ => None,
            };
            Ok((verdict, solve))
        }
        _ => Ok((None, None)),
    }
}
