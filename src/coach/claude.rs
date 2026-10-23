//! Coaching via the Claude Code CLI (`claude -p`), so no API key is needed.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::Config;
use crate::judge::proc::Cancel;
use crate::model::lang::Lang;
use crate::model::store::{AiLog, Problem, Store};
use crate::render::filter_article;

pub const MAX_HINT_LEVEL: usize = 4;
/// Only the most recent coaching entries are sent back as context.
const HISTORY_ENTRIES: usize = 6;
// Prompt budgets in characters, so huge files can't blow up a request.
const MAX_STATEMENT: usize = 12_000;
const MAX_REFERENCE: usize = 24_000;
const MAX_CODE: usize = 20_000;
const MAX_HISTORY: usize = 12_000;

/// What a Claude request is for; selects the model and how results are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hint,
    Review,
    Ask,
    /// Interactive tutoring session.
    Chat,
    /// Writing a test harness.
    Tests,
    /// Mock interview debrief.
    Mock,
    /// Weak-spot analysis across all problems.
    Coach,
    /// Premium statements and starter code during sync.
    Sync,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Hint => "hint",
            Kind::Review => "review",
            Kind::Ask => "ask",
            Kind::Chat => "chat",
            Kind::Tests => "tests",
            Kind::Mock => "mock interview",
            Kind::Coach => "weak-spot analysis",
            Kind::Sync => "sync",
        }
    }
}

pub enum Event {
    Delta(String),
    Done(Result<String>),
}

const SYSTEM: &str = "You are a senior software engineer and experienced coding-interview coach \
(think Google onsite interviewer). You are helping a candidate practise NeetCode problems from \
a terminal app. Your output is shown in a terminal that renders simple Markdown: headings, \
bullets, **bold**, `inline code` and fenced code blocks. No tables, no HTML. Be concise, concrete \
and encouraging without fluff. You may be given the NeetCode reference solution as background: \
use it to ground your feedback, but never paste or paraphrase it in full unless explicitly told \
you may.";

/// Everything Claude needs to know about the current attempt.
pub struct Coaching {
    pub problem: String,
    pub statement: String,
    pub reference: String,
    pub lc_hints: Vec<String>,
    pub code: String,
    pub lang: &'static str,
    pub history: String,
}

impl Coaching {
    pub fn gather(store: &Store, cfg: &Config, p: &Problem) -> Self {
        let lang: &'static Lang = cfg.lang();
        let details = store.load_details(&p.slug).unwrap_or_default();
        let code = std::fs::read_to_string(store.solution_path(p, lang)).unwrap_or_default();
        let reference = store
            .load_article(&p.slug)
            .map(|a| filter_article(&a, lang))
            .or_else(|| store.load_ref_solution(&p.slug, lang))
            .unwrap_or_default();
        let log = store.load_ai(&p.slug).unwrap_or_default();
        Self {
            problem: format!("{} ({}, pattern: {})", p.title, p.difficulty, p.pattern),
            statement: truncate(
                &store.load_description(&p.slug).unwrap_or_default(),
                MAX_STATEMENT,
            ),
            reference: truncate(&reference, MAX_REFERENCE),
            lc_hints: details.hints,
            code: truncate(&code, MAX_CODE),
            lang: lang.key,
            history: truncate(&history_text(&log), MAX_HISTORY),
        }
    }

    pub fn common(&self) -> String {
        let mut s = format!(
            "# Problem: {}\n\n<statement>\n{}\n</statement>\n\n",
            self.problem, self.statement
        );
        if !self.reference.is_empty() {
            s.push_str(&format!(
                "<reference_solution note=\"NeetCode's official write-up. Background only.\">\n{}\n</reference_solution>\n\n",
                self.reference
            ));
        }
        if !self.lc_hints.is_empty() {
            s.push_str(&format!(
                "<leetcode_hints>\n{}\n</leetcode_hints>\n\n",
                self.lc_hints.join("\n")
            ));
        }
        if !self.history.is_empty() {
            s.push_str(&format!(
                "<previous_coaching>\n{}\n</previous_coaching>\n\n",
                self.history
            ));
        }
        s.push_str(&format!(
            "<candidate_code language=\"{}\">\n{}\n</candidate_code>\n\n",
            self.lang,
            if self.code.trim().is_empty() {
                "(nothing written yet)"
            } else {
                &self.code
            }
        ));
        s
    }
}

fn history_text(log: &AiLog) -> String {
    let skip = log.entries.len().saturating_sub(HISTORY_ENTRIES);
    log.entries
        .iter()
        .skip(skip)
        .map(|e| format!("[{}]\n{}", e.kind, e.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => format!("{}\n[... truncated]", &s[..i]),
        None => s.to_string(),
    }
}

pub fn hint_prompt(ctx: &Coaching, level: usize) -> String {
    format!(
        "{}The candidate is stuck and asked for nudge #{level} (of {MAX_HINT_LEVEL}).\n\n\
Nudge levels, each a little more revealing:\n\
1. A guiding question or observation that points at what matters in the problem.\n\
2. Name the key insight, pattern or data structure, and why it fits.\n\
3. Outline the approach in plain words (steps, no code).\n\
4. Pseudocode for the trickiest part only, plus the edge cases to watch.\n\n\
Rules:\n\
- Give exactly ONE nudge at level {level}. Never write the full solution in code.\n\
- Build on previous nudges, don't repeat them.\n\
- Look at their code first: if it shows a specific bug or misconception, address that instead \
  of (or before) the generic nudge, pointing at the line.\n\
- Under 120 words. Start with a short `## Nudge {level}` heading.",
        ctx.common()
    )
}

pub fn review_prompt(ctx: &Coaching) -> String {
    format!(
        "{}Review the candidate's code like an interviewer at the end of a coding round.\n\n\
Use these sections:\n\
## Verdict\nOne line: Correct & optimal / Correct but suboptimal / Has bugs / Incomplete.\n\
## Correctness\nTrace the tricky cases (empty input, duplicates, negatives, overflow, ...). \
Point to concrete bugs by quoting the offending line. Don't rewrite their solution for them.\n\
## Complexity\nTime and space of THEIR code, and what the optimal complexity is.\n\
## Code quality\nNaming, structure, idioms for {}. Keep it brief.\n\
## Interview tips\nWhat they should say out loud, plus one likely follow-up question.\n\n\
If the solution is not optimal or buggy, nudge toward the fix without giving full code. \
If it IS correct and optimal you may show a short, cleaner variant.\n\
Finish with a final line that is exactly `VERDICT: PASS` (correct and at least \
acceptable complexity) or `VERDICT: FAIL`.",
        ctx.common(),
        ctx.lang
    )
}

pub fn ask_prompt(ctx: &Coaching, question: &str) -> String {
    format!(
        "{}The candidate asks:\n\n{question}\n\n\
Answer directly and briefly. Unless they explicitly ask for the solution, \
help them think rather than handing them the answer.",
        ctx.common()
    )
}

/// Starts `claude -p` in a background thread and streams text back.
/// The returned handle cancels the request (and kills the process).
pub fn spawn(cfg: &Config, kind: Kind, prompt: String) -> (Receiver<Event>, Cancel) {
    let (tx, rx) = mpsc::channel();
    let cancel = Cancel::default();
    let cfg = cfg.clone();
    let job = cancel.clone();
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        let res = run(&cfg, kind, &prompt, &job, |d| {
            let _ = tx2.send(Event::Delta(d.to_string()));
        });
        let _ = tx.send(Event::Done(res));
    });
    (rx, cancel)
}

/// Runs `claude -p` with all tools disabled, calling `on_delta` for streamed text.
pub fn run(
    cfg: &Config,
    kind: Kind,
    prompt: &str,
    cancel: &Cancel,
    mut on_delta: impl FnMut(&str),
) -> Result<String> {
    let mut cmd = Command::new(&cfg.claude_cmd);
    cmd.args([
        "-p",
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--system-prompt",
        SYSTEM,
    ]);
    if let Some(model) = cfg.model_for(kind) {
        cmd.args(["--model", model]);
    }
    let mut child = cmd
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to start `{}` (is Claude Code installed?)",
                cfg.claude_cmd
            )
        })?;
    cancel.register(child.id());

    // Feed stdin and drain stderr on their own threads so a full pipe can
    // never deadlock us while we read stdout.
    let mut stdin = child.stdin.take().context("no stdin")?;
    let input = prompt.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stderr = child.stderr.take().context("no stderr")?;
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        String::from_utf8_lossy(&buf).into_owned()
    });

    let stdout = child.stdout.take().context("no stdout")?;
    let mut streamed = String::new();
    let mut result: Option<String> = None;
    let mut error: Option<String> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"] == "stream_event" {
            let ev = &v["event"];
            if ev["type"] == "content_block_delta"
                && ev["delta"]["type"] == "text_delta"
                && let Some(t) = ev["delta"]["text"].as_str()
            {
                streamed.push_str(t);
                on_delta(t);
            }
        } else if let Some(r) = v["result"].as_str() {
            if v["is_error"].as_bool() == Some(true) {
                error = Some(r.to_string());
            } else {
                result = Some(r.to_string());
            }
        }
    }
    // Unregister before reaping so a late cancel can't signal a reused pid.
    cancel.unregister();
    let status = child.wait()?;
    let _ = writer.join();
    let stderr = err_reader.join().unwrap_or_default();

    if cancel.is_cancelled() {
        bail!("cancelled");
    }
    if let Some(e) = error {
        bail!("claude: {e}");
    }
    if !status.success() && result.is_none() {
        bail!("claude exited with {status}: {}", stderr.trim());
    }
    Ok(result.unwrap_or(streamed))
}

pub fn verdict(text: &str) -> Option<bool> {
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let last = last.trim().trim_matches('`');
    match last {
        "VERDICT: PASS" => Some(true),
        "VERDICT: FAIL" => Some(false),
        _ => None,
    }
}

/// Context for an interactive `claude` tutor session (`c`).
pub fn tutor_system_prompt(ctx: &Coaching, solution_path: &str) -> String {
    const MAX_PROMPT: usize = 120 * 1024;
    let mut prompt = format!(
        "{SYSTEM}\n\nYou are in an interactive tutoring session. The candidate's solution file is \
{solution_path}. The session is resumed when the candidate comes back after editing it, and \
the code below is only a snapshot from when the session started, so read the file again \
whenever you discuss their code. Do not edit it unless they ask. Do not reveal the full \
solution unless they explicitly ask for it.\n\n{}",
        ctx.common()
    );
    if prompt.len() > MAX_PROMPT {
        let mut cut = MAX_PROMPT;
        while !prompt.is_char_boundary(cut) {
            cut -= 1;
        }
        prompt.truncate(cut);
    }
    prompt
}

/// A random (version 4) UUID for `claude --session-id`.
pub fn new_session_id() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// Whether Claude Code still has the transcript of a session started in `cwd`, so it can be
/// resumed (old sessions are cleaned up after a while).
pub fn session_exists(cwd: &Path, id: &str) -> bool {
    session_file(&claude_home(), cwd, id).is_some_and(|f| f.is_file())
}

fn claude_home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .unwrap_or_default()
}

/// `<home>/projects/<cwd with every non-alphanumeric character as '-'>/<id>.jsonl`.
fn session_file(home: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    if !crate::model::store::is_safe_ident(id) {
        return None;
    }
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let project: String = cwd
        .to_str()?
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(
        home.join("projects")
            .join(project)
            .join(format!("{id}.jsonl")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_and_files() {
        let id = new_session_id().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(id, new_session_id().unwrap());
        assert_eq!(
            session_file(
                Path::new("/h/.claude"),
                Path::new("/no/such/.local/share/neet/workspace/python"),
                "abc"
            ),
            Some(PathBuf::from(
                "/h/.claude/projects/-no-such--local-share-neet-workspace-python/abc.jsonl"
            ))
        );
        assert_eq!(session_file(Path::new("/h"), Path::new("/w"), "../x"), None);
    }

    #[test]
    fn verdict_reads_last_line() {
        assert_eq!(verdict("ok\n\nVERDICT: PASS\n"), Some(true));
        assert_eq!(verdict("`VERDICT: FAIL`"), Some(false));
        assert_eq!(verdict("VERDICT: PASS\nmore text"), None);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 2), "hé\n[... truncated]");
        assert_eq!(truncate("hi", 5), "hi");
    }
}
