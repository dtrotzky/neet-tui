//! The interactive Claude chat (`c`), full screen or next to the code in Neovim.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::Local;

use super::editor::{INTERVIEW_LUA, STATEMENT_WINDOW, editor_base, is_neovim, run_editor};
use crate::coach::claude::{self, Kind};
use crate::config::Config;
use crate::judge::run::shell_quote;
use crate::model::store::{ChatSession, Problem, Store};

/// Opens the interactive Claude tutor for a problem, continuing the problem's last session
/// unless `fresh`. With `chat_in_editor` (and Neovim), the chat opens in a split next to the
/// solution, and the time counts as editing.
pub fn chat(store: &Store, cfg: &Config, p: &Problem, fresh: bool, lsp: bool) -> Result<()> {
    let sol = store.ensure_solution(p, cfg.lang())?;
    let dir = sol
        .parent()
        .context("solution path has no parent")?
        .to_path_buf();
    let mut log = store.load_ai(&p.slug)?;
    let resume = log
        .chat
        .as_ref()
        .filter(|c| !fresh && claude::session_exists(&dir, &c.id))
        .map(|c| c.id.clone());

    // The prompt goes in a file: it can be large, and a resumed session reuses the
    // first one anyway (Claude Code records it), which is why it says to re-read the file.
    let build = store.build_dir();
    std::fs::create_dir_all(&build)?;
    let prompt_file = build.join(format!("{}.chat-prompt.md", p.file_stem()));
    let ctx = claude::Coaching::gather(store, cfg, p);
    std::fs::write(
        &prompt_file,
        claude::tutor_system_prompt(&ctx, &sol.display().to_string()),
    )?;

    let mut args: Vec<String> = vec![
        "--append-system-prompt-file".into(),
        prompt_file.display().to_string(),
    ];
    if let Some(m) = cfg.model_for(Kind::Chat) {
        args.extend(["--model".into(), m.into()]);
    }
    match resume {
        Some(id) => args.extend(["--resume".into(), id]),
        None => {
            let id = claude::new_session_id()?;
            args.extend([
                "--session-id".into(),
                id.clone(),
                format!(
                    "I'm working on \"{}\". Briefly ask me how I'm thinking about approaching it.",
                    p.title
                ),
            ]);
            // Saved first, so the session is found again even if the chat is killed.
            log.chat = Some(ChatSession {
                id,
                started: Local::now(),
            });
            store.save_ai(&p.slug, &log)?;
        }
    }

    if cfg.chat_in_editor && is_neovim(cfg) {
        let script = build.join(format!("{}.chat.sh", p.file_stem()));
        let mut line = format!(
            "cd {} && exec {}",
            shell_quote(&dir),
            shell_quote(Path::new(&cfg.claude_cmd))
        );
        for a in &args {
            line.push(' ');
            line.push_str(&shell_quote(Path::new(a)));
        }
        std::fs::write(&script, format!("#!/bin/sh\n{line}\n"))?;
        let description = store.description_path(&p.slug);
        let spent = run_editor(
            cfg,
            chat_editor_command(cfg, &sol, &description, &script, lsp),
        )?;
        store.update_progress(|progress| {
            progress
                .entries
                .entry(p.slug.clone())
                .or_default()
                .time_spent_secs += spent.as_secs();
        })?;
    } else {
        Command::new(&cfg.claude_cmd)
            .current_dir(&dir)
            .args(&args)
            .status()
            .with_context(|| format!("failed to start `{}`", cfg.claude_cmd))?;
    }
    Ok(())
}

/// Neovim with the statement (if `split_description`), the solution, and a terminal
/// running `script` on the right, in equal columns.
fn chat_editor_command(
    cfg: &Config,
    solution: &Path,
    description: &Path,
    script: &Path,
    lsp: bool,
) -> Command {
    let (mut cmd, _) = editor_base(cfg);
    if !lsp {
        cmd.arg("--cmd")
            .arg(format!("lua {}", INTERVIEW_LUA.join(" ")));
    }
    let statement = cfg.split_description && description.exists();
    if statement {
        // Runs first, in the statement's window, then moves to the solution.
        cmd.arg("-c").arg(format!("{STATEMENT_WINDOW} | wincmd l"));
    }
    let script = lua_quote(&script.display().to_string());
    // The chat job is stopped on exit, so `:wqa` works without `!` (the transcript is
    // already saved, and the next chat continues it).
    cmd.args([
        "-c",
        &format!(
            "lua vim.cmd('botright vnew'); local job; \
             if vim.fn.has('nvim-0.11') == 1 then job = vim.fn.jobstart({{'sh', {script}}}, {{term = true}}) \
             else job = vim.fn.termopen({{'sh', {script}}}) end; \
             vim.api.nvim_create_autocmd('ExitPre', {{callback = function() pcall(vim.fn.jobstop, job) end}}); \
             vim.cmd('wincmd ='); vim.cmd('startinsert')"
        ),
    ]);
    if statement {
        cmd.args(["-O", "--"]).arg(description);
    } else {
        cmd.arg("--");
    }
    cmd.arg(solution);
    cmd
}

/// A Lua string literal.
fn lua_quote(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\{}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_split_includes_the_statement() {
        let dir = std::env::temp_dir().join(format!("neet-{}-chat-split", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let description = dir.join("description.md");
        std::fs::write(&description, "# Two Sum").unwrap();
        let (sol, script) = (dir.join("0001-two-sum.py"), dir.join("chat.sh"));
        let mut cfg = Config {
            editor: Some("nvim".into()),
            ..Config::default()
        };
        let args = |cfg: &Config| -> Vec<String> {
            chat_editor_command(cfg, &sol, &description, &script, true)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let with = args(&cfg);
        let tail: Vec<&str> = with
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(String::as_str)
            .collect();
        let (d, s) = (description.display().to_string(), sol.display().to_string());
        assert_eq!(tail, ["-O", "--", d.as_str(), s.as_str()]);
        assert!(with[1].starts_with(STATEMENT_WINDOW), "{with:?}");

        cfg.split_description = false;
        let without = args(&cfg);
        assert!(!without.contains(&d), "{without:?}");
        assert_eq!(without.last(), Some(&s));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lua_quote_escapes() {
        assert_eq!(lua_quote(r#"/a/"b"\c"#), r#""/a/\"b\"\\c""#);
        assert_eq!(lua_quote("x\ny"), r#""x\10y""#);
    }
}
