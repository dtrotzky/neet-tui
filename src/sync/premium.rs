//! Statements and starter code for LeetCode Premium problems, written once by Claude from
//! NeetCode's write-up.

use std::sync::Mutex;

use anyhow::{Context, Result, bail};

use crate::coach::claude;
use crate::config::Config;
use crate::judge::proc::Cancel;
use crate::model::lang::LANGS;
use crate::model::store::{Problem, Store, write_json};
use crate::render::sanitize;

pub(super) const PREMIUM_MARKER: &str = "This is a LeetCode Premium problem";

/// Premium statements aren't public; have Claude write an equivalent one, once.
pub(super) fn write_premium_statements(
    store: &Store,
    cfg: &Config,
    problems: &[Problem],
) -> Result<()> {
    let todo: Vec<&Problem> = problems
        .iter()
        .filter(|p| {
            store
                .load_details(&p.slug)
                .is_some_and(|d| d.premium && d.description.starts_with(PREMIUM_MARKER))
        })
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    println!(
        "==> Asking Claude to write statements for {} LeetCode Premium problems",
        todo.len()
    );
    let queue = Mutex::new(todo);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(p) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    match premium_statement(store, cfg, p) {
                        Ok(()) => println!("    wrote {}", p.title),
                        Err(e) => eprintln!("    {}: {e:#} (will retry on next sync)", p.title),
                    }
                }
            });
        }
    });
    Ok(())
}

fn premium_statement(store: &Store, cfg: &Config, p: &Problem) -> Result<()> {
    let article = store.load_article(&p.slug).unwrap_or_default();
    let prompt = format!(
        "Write the problem statement for the LeetCode problem \"{}\" (slug `{}`), \
as NeetCode presents it. Below is NeetCode's solution write-up so you match its function \
signature and conventions. Output ONLY the statement in plain Markdown: description, the \
function to implement (as `name(params) -> return`), 2-3 examples formatted as \
`**Example 1:**` / `**Input:** ...` / `**Output:** ...` / optional `**Explanation:** ...`, \
and a `**Constraints:**` bullet list. No headings, no hints, no solution.\n\n\
<writeup>\n{}\n</writeup>",
        p.title,
        p.slug,
        article.chars().take(6000).collect::<String>()
    );
    let text = sanitize(&claude::run(
        cfg,
        claude::Kind::Sync,
        &prompt,
        &Cancel::default(),
        |_| {},
    )?);
    if text.trim().is_empty() {
        bail!("empty response");
    }
    let mut details = store.load_details(&p.slug).unwrap_or_default();
    details.description = text.trim().to_string();
    write_json(&store.details_path(&p.slug), &details)?;
    let old = store.load_description(&p.slug).unwrap_or_default();
    let header: String = old
        .split_inclusive('\n')
        .take_while(|l| !l.starts_with(PREMIUM_MARKER))
        .collect();
    let md = format!(
        "{header}{}\n\n> LeetCode Premium problem: statement written by Claude from NeetCode's write-up.\n",
        details.description
    );
    std::fs::write(store.description_path(&p.slug), md)?;
    Ok(())
}

/// LeetCode has no starter code for premium problems; have Claude write it, once.
pub(super) fn write_premium_starters(
    store: &Store,
    cfg: &Config,
    problems: &[Problem],
) -> Result<()> {
    let todo: Vec<&Problem> = problems
        .iter()
        .filter(|p| {
            store.load_details(&p.slug).is_some_and(|d| {
                d.premium && d.snippets.is_empty() && !d.description.starts_with(PREMIUM_MARKER)
            })
        })
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    println!(
        "==> Asking Claude for starter code for {} LeetCode Premium problems",
        todo.len()
    );
    let queue = Mutex::new(todo);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(p) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    match premium_starter(store, cfg, p) {
                        Ok(n) => println!("    {}: {n} languages", p.title),
                        Err(e) => eprintln!("    {}: {e:#} (will retry on next sync)", p.title),
                    }
                }
            });
        }
    });
    Ok(())
}

fn premium_starter(store: &Store, cfg: &Config, p: &Problem) -> Result<usize> {
    let mut details = store.load_details(&p.slug).context("no details")?;
    let slugs: Vec<&str> = LANGS.iter().map(|l| l.lc_slug).collect();
    let prompt = format!(
        "Write LeetCode-style starter code (signature only, empty body) for this problem in \
each of these languages: {}.\n\n<statement>\n{}\n</statement>\n\n\
Match the function name and types in the statement. Use LeetCode conventions: a `Solution` \
class (`impl Solution` in Rust, plain functions in Go/JavaScript/TypeScript as LeetCode does). \
If the problem uses a helper type such as `Interval`, include its definition above the \
Solution. Reply with one fenced code block per language, the fence labelled with exactly the \
language name from the list (e.g. ```python3). No other text.",
        slugs.join(", "),
        details.description
    );
    let text = claude::run(cfg, claude::Kind::Sync, &prompt, &Cancel::default(), |_| {})?;
    for (label, code) in fenced_blocks(&text) {
        if slugs.contains(&label.as_str()) {
            details.snippets.insert(label, sanitize(code.trim_end()));
        }
    }
    if details.snippets.is_empty() {
        bail!("no usable code blocks in the reply");
    }
    write_json(&store.details_path(&p.slug), &details)?;
    Ok(details.snippets.len())
}

/// All fenced code blocks in `text` as (label, code).
fn fenced_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        let fence = line.trim_start().strip_prefix("```");
        match (&mut current, fence) {
            (None, Some(label)) => current = Some((label.trim().to_lowercase(), String::new())),
            (Some(_), Some(_)) => out.extend(current.take()),
            (Some((_, code)), None) => {
                code.push_str(line);
                code.push('\n');
            }
            (None, None) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_fenced_blocks() {
        let text = "```python3\nclass Solution:\n    pass\n```\ntext\n```Go\nfunc f() {}\n```\n";
        let blocks = fenced_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "python3");
        assert_eq!(blocks[1], ("go".to_string(), "func f() {}\n".to_string()));
    }
}
