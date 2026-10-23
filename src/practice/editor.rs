//! Opening solutions in the editor, with the statement split and Neovim's interview mode.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::progress::mark_opened;
use crate::config::Config;
use crate::model::store::{Problem, Progress, Store};

/// Editor sessions longer than this were probably left open; count only this much.
const MAX_SESSION: Duration = Duration::from_secs(3 * 3600);

/// Neovim "interview mode": no LSP clients (so no completion, diagnostics,
/// Copilot or other AI suggestions), completion plugins switched off, syntax
/// highlighting untouched. Runs via `--cmd`, before the user's config loads.
/// Lua is whitespace-insensitive, so the lines are joined into one Ex command.
pub(super) const INTERVIEW_LUA: &[&str] = &[
    "vim.g.neet_interview = true",
    "local lsp = vim.lsp",
    // Servers never start; LspAttach below catches clients started another way.
    "lsp.start = function() return nil end",
    // Plugins (copilot.lua) report the refused start; keep that out of the way.
    "local wrapped",
    "local function quiet()",
    "  if vim.notify == wrapped then return end",
    "  local notify = vim.notify",
    "  wrapped = function(msg, ...)",
    "    if type(msg) == 'string' and msg:find('Copilot', 1, true) then return end",
    "    return notify(msg, ...)",
    "  end",
    "  vim.notify = wrapped",
    "end",
    "quiet()",
    "local g = vim.api.nvim_create_augroup('neet_interview', {})",
    "vim.api.nvim_create_autocmd({'VimEnter', 'User'}, {group = g, callback = quiet})",
    "vim.api.nvim_create_autocmd('LspAttach', {group = g, callback = function(a)",
    "  vim.schedule(function()",
    "    local c = lsp.get_client_by_id(a.data.client_id)",
    "    if c then c:stop(true) end",
    "  end)",
    "end})",
    "vim.api.nvim_create_autocmd({'BufEnter', 'InsertEnter'}, {group = g, callback = function()",
    "  vim.b.completion = false",
    "  vim.b.copilot_enabled = false",
    "  pcall(function() require('cmp').setup.buffer({enabled = false}) end)",
    "  pcall(vim.diagnostic.enable, false)",
    "end})",
    "vim.api.nvim_create_autocmd('VimEnter', {group = g, callback = function()",
    "  vim.api.nvim_echo({{'neet: interview mode (syntax highlighting only, no LSP or completion)', 'WarningMsg'}}, false, {})",
    "end})",
];

/// The configured editor, and whether it is vim/nvim.
pub(super) fn editor_base(cfg: &Config) -> (Command, bool) {
    let editor = cfg.editor();
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("nvim").to_string();
    let mut cmd = Command::new(&bin);
    cmd.args(parts);
    let is_vim = Path::new(&bin)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("vim"));
    (cmd, is_vim)
}

pub(super) fn is_neovim(cfg: &Config) -> bool {
    let editor = cfg.editor();
    let bin = editor.split_whitespace().next().unwrap_or_default();
    Path::new(bin)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("nvim") || n == "lvim")
}

/// Builds the editor invocation; vim/nvim also get the statement in a read-only
/// split, and Neovim gets interview mode when `lsp` is false.
/// Window options for the read-only problem statement next to the solution.
pub(super) const STATEMENT_WINDOW: &str =
    "setlocal readonly nomodifiable wrap linebreak nonumber norelativenumber filetype=markdown";

pub fn editor_command(cfg: &Config, solution: &Path, description: &Path, lsp: bool) -> Command {
    let (mut cmd, is_vim) = editor_base(cfg);
    if !lsp && is_neovim(cfg) {
        cmd.arg("--cmd")
            .arg(format!("lua {}", INTERVIEW_LUA.join(" ")));
    }
    if is_vim && cfg.split_description && description.exists() {
        // Both files are passed as arguments (not spliced into an Ex command),
        // so paths need no escaping. `-c` runs in the first window: the statement.
        cmd.arg("-c")
            .arg(format!(
                "{STATEMENT_WINDOW} | vertical resize 80 | wincmd l"
            ))
            .args(["-O", "--"]);
        cmd.arg(description);
    } else if is_vim {
        cmd.arg("--");
    }
    cmd.arg(solution);
    cmd
}

pub(super) fn run_editor(cfg: &Config, mut cmd: Command) -> Result<Duration> {
    let start = Instant::now();
    let status = cmd
        .status()
        .with_context(|| format!("failed to launch editor `{}`", cfg.editor()))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(start.elapsed().min(MAX_SESSION))
}

/// Opens the solution in the editor and tracks the time spent; returns fresh progress.
/// `lsp: false` opens Neovim in interview mode.
pub fn edit(store: &Store, cfg: &Config, p: &Problem, lsp: bool) -> Result<Progress> {
    let lang = cfg.lang();
    let sol = store.ensure_solution(p, lang)?;
    mark_opened(store, p)?;
    let spent = run_editor(
        cfg,
        editor_command(cfg, &sol, &store.description_path(&p.slug), lsp),
    )?;
    store.update_progress(|progress| {
        progress
            .entries
            .entry(p.slug.clone())
            .or_default()
            .time_spent_secs += spent.as_secs();
    })
}
