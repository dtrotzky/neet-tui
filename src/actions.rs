//! Operations shared by the TUI and the plain CLI.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};

use crate::claude::{self, Kind};
use crate::config::Config;
use crate::proc::{self, Cancel};
use crate::srs;
use crate::store::{AiEntry, ChatSession, Problem, Progress, Status, Store};
use crate::testing;

/// Editor sessions longer than this were probably left open; count only this much.
const MAX_SESSION: Duration = Duration::from_secs(3 * 3600);

/// Cap on captured output per stream, so a runaway print loop can't eat memory.
const MAX_OUTPUT: u64 = 256 * 1024;

/// Finds a problem by LeetCode/NeetCode slug, file stem, LeetCode number,
/// title, or a solution file path.
pub fn resolve<'a>(problems: &'a [Problem], store: &Store, query: &str) -> Result<&'a Problem> {
    let path = Path::new(query);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(query);
    let q = stem.trim().to_lowercase();
    let found = problems.iter().find(|p| {
        p.slug == q || p.nc_slug == q || p.file_stem() == q || p.title.to_lowercase() == q
    });
    if let Some(p) = found {
        return Ok(p);
    }
    if !q.is_empty() && q.bytes().all(|b| b.is_ascii_digit()) {
        let num = q.trim_start_matches('0');
        if let Some(p) = problems.iter().find(|p| {
            store
                .load_details(&p.slug)
                .and_then(|d| d.id)
                .is_some_and(|id| id == num)
        }) {
            return Ok(p);
        }
    }
    let fuzzy: Vec<&Problem> = problems
        .iter()
        .filter(|p| p.title.to_lowercase().contains(&q) || p.slug.contains(&q))
        .collect();
    match fuzzy.as_slice() {
        [one] => Ok(one),
        [] => bail!("no problem matches '{query}'"),
        many => bail!(
            "'{query}' is ambiguous: {}",
            many.iter()
                .take(8)
                .map(|p| p.slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

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
        Kind::Tests => testing::harness_prompt(store, cfg, p),
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

/// Neovim "interview mode": no LSP clients (so no completion, diagnostics,
/// Copilot or other AI suggestions), completion plugins switched off, syntax
/// highlighting untouched. Runs via `--cmd`, before the user's config loads.
/// Lua is whitespace-insensitive, so the lines are joined into one Ex command.
const INTERVIEW_LUA: &[&str] = &[
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
fn editor_base(cfg: &Config) -> (Command, bool) {
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

fn is_neovim(cfg: &Config) -> bool {
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
const STATEMENT_WINDOW: &str =
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

fn run_editor(cfg: &Config, mut cmd: Command) -> Result<Duration> {
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

fn unsolved(progress: &Progress, p: &Problem) -> bool {
    progress
        .entries
        .get(&p.slug)
        .is_none_or(|e| e.status != Status::Solved)
}

/// The next unsolved problem in roadmap order after `after`, wrapping around.
pub fn next_unsolved(
    problems: &[Problem],
    progress: &Progress,
    include: impl Fn(&Problem) -> bool,
    after: Option<usize>,
) -> Option<usize> {
    let n = problems.len();
    let start = after.map_or(0, |a| a + 1);
    (0..n)
        .map(|k| (start + k) % n)
        .find(|&i| include(&problems[i]) && unsolved(progress, &problems[i]))
}

/// A random unsolved problem.
pub fn random_unsolved(
    problems: &[Problem],
    progress: &Progress,
    include: impl Fn(&Problem) -> bool,
) -> Option<usize> {
    let candidates: Vec<usize> = (0..problems.len())
        .filter(|&i| include(&problems[i]) && unsolved(progress, &problems[i]))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    Some(candidates[random_below(candidates.len())])
}

/// Good-enough randomness for picking a problem, without a dependency.
fn random_below(n: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = (nanos ^ (u64::from(std::process::id()) << 32)) | 1;
    // xorshift64*
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64) as usize
}

/// Generates the harness if needed (blocking), builds the test file and runs it.
pub fn run_tests(
    store: &Store,
    cfg: &Config,
    p: &Problem,
    regenerate: bool,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<(String, Option<(u32, u32)>)> {
    testing::ensure_has_solution(store, cfg, p)?;
    let source = if testing::needs_claude(store, cfg, p, regenerate) {
        testing::generate(store, cfg, p)?;
        testing::Source::Claude
    } else {
        testing::Source::Auto
    };
    run_existing_tests(store, cfg, p, source, timeout, cancel)
}

/// Runs tests with an existing harness (built-in or generated), recording the result.
pub fn run_existing_tests(
    store: &Store,
    cfg: &Config,
    p: &Problem,
    source: testing::Source,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<(String, Option<(u32, u32)>)> {
    let file = testing::build_test_file(store, cfg, p, source)?;
    let output = run_captured(cfg, store, &file, timeout, cancel)?;
    let result = testing::parse_result(&output);
    if let Some((passed, total)) = result {
        testing::record(store, p, passed, total)?;
    }
    Ok((output, result))
}

fn run_command_line(cfg: &Config, store: &Store, file: &Path) -> Result<String> {
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .context("bad solution path")?;
    let build = store.build_dir();
    std::fs::create_dir_all(&build)?;
    Ok(fill_placeholders(
        &cfg.run_cmd(),
        &shell_quote(file),
        &shell_quote(&build.join(stem)),
    ))
}

/// Replaces `{file}` and `{bin}` in one pass, so text inside a substituted
/// path (say a folder literally named `{bin}`) is never substituted again.
fn fill_placeholders(template: &str, file: &str, bin: &str) -> String {
    let mut out = String::with_capacity(template.len() + file.len() + bin.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(after) = tail.strip_prefix("{file}") {
            out.push_str(file);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("{bin}") {
            out.push_str(bin);
            rest = after;
        } else {
            out.push('{');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn solution_command(cmdline: &str, file: &Path) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(cmdline)
        .current_dir(file.parent().unwrap_or(Path::new(".")));
    cmd
}

/// Runs the solution in the foreground with the terminal attached (CLI).
pub fn run_interactive(cfg: &Config, store: &Store, file: &Path) -> Result<ExitStatus> {
    if !file.exists() {
        bail!("no solution yet at {}", file.display());
    }
    let cmdline = run_command_line(cfg, store, file)?;
    eprintln!("$ {cmdline}");
    Ok(solution_command(&cmdline, file).status()?)
}

/// Runs the solution with captured output (TUI). The whole process group is
/// killed on timeout or cancellation, so builds and runaway loops can't linger.
pub fn run_captured(
    cfg: &Config,
    store: &Store,
    file: &Path,
    timeout: Duration,
    cancel: &Cancel,
) -> Result<String> {
    let cmdline = run_command_line(cfg, store, file)?;
    let mut child = solution_command(&cmdline, file)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start run command")?;
    let out_t = capture(child.stdout.take().context("no stdout")?);
    let err_t = capture(child.stderr.take().context("no stderr")?);

    let start = Instant::now();
    let outcome = loop {
        if let Some(st) = child.try_wait()? {
            break Ok(st);
        }
        let reason = if cancel.is_cancelled() {
            Some("cancelled".to_string())
        } else if start.elapsed() > timeout {
            Some(format!("killed after {}s timeout", timeout.as_secs()))
        } else {
            None
        };
        if let Some(reason) = reason {
            // Kill before reaping: the group id can't be reused yet.
            proc::kill_group(child.id());
            let _ = child.wait();
            break Err(reason);
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let elapsed = start.elapsed();
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();

    let mut text = format!("$ {cmdline}\n\n{stdout}");
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let footer = match outcome {
        Ok(st) if st.success() => format!("ok in {:.2}s", elapsed.as_secs_f64()),
        Ok(st) => format!("failed: {st} after {:.2}s", elapsed.as_secs_f64()),
        Err(reason) => reason,
    };
    text.push_str(&format!("\n[{footer}]"));
    Ok(text)
}

/// Reads a pipe to the end on a thread, keeping at most `MAX_OUTPUT` bytes.
fn capture(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut pipe).take(MAX_OUTPUT).read_to_end(&mut buf);
        let rest = std::io::copy(&mut pipe, &mut std::io::sink()).unwrap_or(0);
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if rest > 0 {
            s.push_str(&format!("\n[... {rest} more bytes not shown]\n"));
        }
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_filled_once() {
        assert_eq!(
            fill_placeholders("g++ -o {bin} {file} && {bin}", "'/a/{bin}/x.cpp'", "'/b/x'"),
            "g++ -o '/b/x' '/a/{bin}/x.cpp' && '/b/x'"
        );
        assert_eq!(
            fill_placeholders("echo {x} {file", "F", "B"),
            "echo {x} {file"
        );
    }

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

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote(Path::new("/a/it's")), "'/a/it'\\''s'");
    }

    /// A store in a fresh temp folder, and a config whose Python run command is `cmd`.
    fn run_sandbox(name: &str, cmd: &str) -> (Store, Config, PathBuf) {
        let dir = std::env::temp_dir().join(format!("neet-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store {
            root: dir.join("data"),
            workspace: dir.join("workspace"),
        };
        let mut cfg = Config {
            language: "python".into(),
            ..Config::default()
        };
        cfg.run
            .insert("python".into(), cmd.replace("{dir}", &shell_quote(&dir)));
        let file = dir.join("solution.py");
        std::fs::write(&file, "").unwrap();
        (store, cfg, file)
    }

    #[test]
    fn timeout_kills_the_whole_process_group() {
        // A background child that would outlive a plain kill of the shell.
        let (store, cfg, file) = run_sandbox("timeout", "(sleep 1; touch {dir}/leaked) & sleep 30");
        let start = Instant::now();
        let out = run_captured(
            &cfg,
            &store,
            &file,
            Duration::from_millis(300),
            &Cancel::default(),
        )
        .unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{out}");
        assert!(out.contains("timeout"), "{out}");
        std::thread::sleep(Duration::from_millis(1500));
        let dir = file.parent().unwrap();
        assert!(!dir.join("leaked").exists(), "background child survived");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn output_is_capped() {
        let (store, cfg, file) = run_sandbox("cap", "head -c 1000000 /dev/zero | tr '\\0' a");
        let out = run_captured(
            &cfg,
            &store,
            &file,
            Duration::from_secs(20),
            &Cancel::default(),
        )
        .unwrap();
        assert!(
            out.len() < MAX_OUTPUT as usize + 1024,
            "{} bytes",
            out.len()
        );
        assert!(out.contains("more bytes not shown"));
        assert!(out.contains("[ok in"));
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn notes_body_drops_template() {
        let notes = format!("# Two Sum\n\n{NOTES_HINT}\n\nHash map, O(n).\n");
        assert_eq!(notes_body(&notes), "Hash map, O(n).");
    }
}
