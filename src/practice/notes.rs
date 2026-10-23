//! Per-problem notes and the cheat-sheet export.

use anyhow::{Context, Result};

use super::editor::{editor_base, run_editor};
use crate::config::Config;
use crate::model::store::{Problem, Store};

const NOTES_HINT: &str = "<!-- Your notes: the key insight in a line or two, complexity, gotchas. \
Shown in neet and collected by `neet notes --export`. -->";

/// Opens the problem's notes file in the editor (no split).
pub fn edit_notes(store: &Store, cfg: &Config, p: &Problem) -> Result<()> {
    let path = store.notes_path(p);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().context("bad notes path")?)?;
        std::fs::write(&path, format!("# {}\n\n{NOTES_HINT}\n\n", p.title))?;
    }
    let (mut cmd, is_vim) = editor_base(cfg);
    if is_vim {
        cmd.arg("--");
    }
    cmd.arg(&path);
    run_editor(cfg, cmd)?;
    Ok(())
}

/// Notes without the template's title line and hint comment.
pub fn notes_body(notes: &str) -> String {
    notes
        .lines()
        .enumerate()
        .filter(|(i, l)| !(*i == 0 && l.starts_with("# ")) && l.trim() != NOTES_HINT)
        .map(|(_, l)| l)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// All notes as one Markdown cheat sheet, grouped by pattern.
pub fn export_notes(store: &Store, problems: &[Problem]) -> String {
    let mut out = String::from("# NeetCode cheat sheet\n");
    let mut last = "";
    for p in problems {
        let Some(body) = store
            .load_notes(p)
            .map(|n| notes_body(&n))
            .filter(|b| !b.is_empty())
        else {
            continue;
        };
        if p.pattern != last {
            out.push_str(&format!("\n## {}\n", p.pattern));
            last = &p.pattern;
        }
        out.push_str(&format!("\n### {} ({})\n\n{body}\n", p.title, p.difficulty));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_body_drops_template() {
        let notes = format!("# Two Sum\n\n{NOTES_HINT}\n\nHash map, O(n).\n");
        assert_eq!(notes_body(&notes), "Hash map, O(n).");
    }
}
