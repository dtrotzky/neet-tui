//! `neet test`: run your solution against the examples (plus a few edge cases).
//!
//! A harness is appended to your code (everything above `MARKER`) to build a test file. In
//! Python, problems with built-in cases use neet's generic harness (see `cases`); otherwise
//! Claude writes a harness once per problem and language, and it is cached.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Local;

use crate::coach::claude::{self, Coaching};
use crate::config::Config;
use crate::judge::cases::{self, Availability};
use crate::model::lang::Lang;
use crate::model::store::{Problem, Progress, Store, TestRun};
use crate::render::sanitize;

/// Separates your solution from scratch checks in solution files.
pub const MARKER: &str =
    "---- neet: scratch checks below; `neet test` replaces everything from here ----";

/// How the harness gets its entry point in each language.
fn entry_point(lang: &Lang) -> &'static str {
    match lang.key {
        "python" => "an `if __name__ == \"__main__\":` block",
        "cpp" => "`int main()` (all of <bits/stdc++.h> and `using namespace std` are available)",
        "java" => {
            "a non-public `class Main` with `public static void main(String[] args)` \
             (java.util.* is imported; use fully qualified names for anything else)"
        }
        "go" => {
            "`func main()`. Do NOT add import statements: only fmt, reflect, sort and strings \
             are imported (at the top of the file)"
        }
        "rust" => "`fn main()` (use fully qualified paths for anything outside std::collections)",
        "javascript" | "typescript" => "top-level statements at the end of the file",
        "kotlin" => "`fun main()`",
        "csharp" => "top-level statements (C# script)",
        _ => "the language's normal program entry point",
    }
}

pub fn harness_prompt(store: &Store, cfg: &Config, p: &Problem) -> String {
    let lang = cfg.lang();
    let ctx = Coaching::gather(store, cfg, p);
    let known = match cases::spec(store, p) {
        Availability::Ready(spec) => format!(
            "<verified_test_cases compare=\"{}\">\n{}\n</verified_test_cases>\n\n\
             Use exactly these test cases, with these names and expected outputs (inputs are given \
             by parameter name; `ops`/`args` is the usual LeetCode design-problem format; lists and \
             trees are in LeetCode's array form). Add no other cases.\n\n",
            serde_json::to_value(spec.compare)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            serde_json::to_string_pretty(&spec.cases).unwrap_or_default()
        ),
        Availability::Unavailable(_) => String::new(),
    };
    let signature = store
        .load_details(&p.slug)
        .and_then(|d| d.snippets.get(lang.lc_slug).cloned())
        .unwrap_or_else(|| solution_part(&ctx.code).to_string());
    format!(
        "# Problem: {}\n\n<statement>\n{}\n</statement>\n\n\
<reference_solution>\n{}\n</reference_solution>\n\n\
<starter_code language=\"{}\">\n{}\n</starter_code>\n\n{}\
Write a test harness in {} that will be appended directly AFTER the candidate's code \
(which contains the starter code above, filled in, plus any ListNode/TreeNode definitions). \
Requirements:\n\
- Entry point: {}.\n\
- Do NOT redefine the Solution class/functions or ListNode/TreeNode. Write any helpers you \
  need (e.g. building a linked list or tree from an array, serializing results) with names \
  prefixed `neet_` to avoid clashes.\n\
- Tests: every example from the statement, with expected outputs copied exactly. Then up to 5 \
  extra edge cases, ONLY ones whose expected output you are certain of (verify against the \
  reference solution). Name them `example 1`, `example 2`, ..., `edge: <what>`.\n\
- If the problem accepts answers in any order, compare order-insensitively. If several answers \
  are valid, check the required properties instead of one exact answer. For in-place problems, \
  check the mutated input.\n\
- Run each test independently with a fresh Solution instance; an exception or panic in one \
  test must count as a failure of that test, not stop the run (where the language allows).\n\
- Output exactly one line per test: `PASS <name>` or `FAIL <name>: input=<...> expected=<...> got=<...>`, \
  then a final line `RESULT <passed>/<total>`.\n\
- No other output, no reading input. Do not access files, the network, environment variables \
  or other processes: the harness is executed automatically on the candidate's machine.\n\n\
Reply with only the harness code in a single fenced code block.",
        ctx.problem,
        ctx.statement,
        ctx.reference,
        lang.key,
        signature,
        known,
        lang.key,
        entry_point(lang),
    )
}

/// The part of a solution file above the scratch marker.
pub fn solution_part(code: &str) -> &str {
    match code.find(MARKER) {
        // Cut at the start of the marker's line (drops the comment prefix too).
        Some(i) => &code[..code[..i].rfind('\n').map_or(0, |n| n + 1)],
        None => code,
    }
}

/// Extracts the first fenced code block from a response.
pub fn extract_code(text: &str) -> Option<String> {
    let mut lines = text.lines();
    lines.find(|l| l.trim_start().starts_with("```"))?;
    let mut code = String::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            return Some(code);
        }
        code.push_str(line);
        code.push('\n');
    }
    None
}

/// Which harness a test run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Built-in tests when available, else Claude's.
    Auto,
    /// Claude's, e.g. right after regenerating them.
    Claude,
}

/// Claude's cached harness for this problem and language.
pub fn load_harness(store: &Store, cfg: &Config, p: &Problem) -> Option<String> {
    std::fs::read_to_string(store.harness_path(p, cfg.lang())).ok()
}

/// neet's own harness, when the language has one and the problem has built-in cases.
pub fn builtin_harness(store: &Store, cfg: &Config, p: &Problem) -> Option<String> {
    if cfg.lang().key != "python" {
        return None;
    }
    match cases::spec(store, p) {
        Availability::Ready(spec) => Some(cases::python_harness(&spec)),
        Availability::Unavailable(_) => None,
    }
}

/// Whether testing needs Claude to write a harness first.
pub fn needs_claude(store: &Store, cfg: &Config, p: &Problem, regenerate: bool) -> bool {
    regenerate
        || (builtin_harness(store, cfg, p).is_none() && load_harness(store, cfg, p).is_none())
}

fn harness(store: &Store, cfg: &Config, p: &Problem, source: Source) -> Option<String> {
    match source {
        Source::Auto => builtin_harness(store, cfg, p).or_else(|| load_harness(store, cfg, p)),
        Source::Claude => load_harness(store, cfg, p),
    }
}

pub fn save_harness(store: &Store, cfg: &Config, p: &Problem, response: &str) -> Result<PathBuf> {
    let code = extract_code(response).context("Claude's reply contained no code block")?;
    let path = store.harness_path(p, cfg.lang());
    std::fs::create_dir_all(path.parent().context("bad harness path")?)?;
    std::fs::write(&path, sanitize(&code))?;
    Ok(path)
}

/// Writes your solution + the harness into the build dir, returning the test file.
pub fn build_test_file(
    store: &Store,
    cfg: &Config,
    p: &Problem,
    source: Source,
) -> Result<PathBuf> {
    let lang = cfg.lang();
    let sol = store.solution_path(p, lang);
    let code = std::fs::read_to_string(&sol)
        .with_context(|| format!("no solution yet at {}", sol.display()))?;
    let harness = harness(store, cfg, p, source).context("no test harness generated yet")?;
    if !code.contains(MARKER) && !matches!(lang.key, "python" | "javascript" | "typescript") {
        bail!(
            "{} has no `{MARKER}` line, so its own main would clash with the tests. \
             Add the line (as a comment) above your scratch main, or start over with `neet fresh`.",
            sol.display()
        );
    }
    let build = store.build_dir();
    std::fs::create_dir_all(&build)?;
    let path = build.join(format!("{}.test.{}", p.file_stem(), lang.ext));
    let mut out = solution_part(&code).trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(&harness);
    std::fs::write(&path, out)?;
    Ok(path)
}

/// Parses the harness's final `RESULT <passed>/<total>` line.
pub fn parse_result(output: &str) -> Option<(u32, u32)> {
    let line = output.lines().rev().find(|l| l.starts_with("RESULT "))?;
    let (passed, total) = line.strip_prefix("RESULT ")?.trim().split_once('/')?;
    Some((passed.trim().parse().ok()?, total.trim().parse().ok()?))
}

pub fn record(store: &Store, p: &Problem, passed: u32, total: u32) -> Result<Progress> {
    store.update_progress(|progress| {
        progress
            .entries
            .entry(p.slug.clone())
            .or_default()
            .last_test = Some(TestRun {
            passed,
            total,
            at: Local::now(),
        });
    })
}

/// Whether a test run passed completely.
pub fn all_passed(result: Option<(u32, u32)>) -> bool {
    matches!(result, Some((p, t)) if p == t && t > 0)
}

/// Generates and saves a harness (blocking).
pub fn generate(store: &Store, cfg: &Config, p: &Problem) -> Result<PathBuf> {
    let prompt = harness_prompt(store, cfg, p);
    let text = claude::run(
        cfg,
        claude::Kind::Tests,
        &prompt,
        &Default::default(),
        |_| {},
    )?;
    save_harness(store, cfg, p, &text)
}

/// Summary line shown after a test run.
pub fn summary(output: &str) -> String {
    match parse_result(output) {
        Some((p, t)) if p == t && t > 0 => format!("tests: all {t} passed ✓"),
        Some((p, t)) => format!("tests: {p}/{t} passed"),
        None => {
            "tests: no result line, so it probably didn't compile or crashed (see output)".into()
        }
    }
}

pub fn ensure_has_solution(store: &Store, cfg: &Config, p: &Problem) -> Result<()> {
    if !store.solution_path(p, cfg.lang()).exists() {
        bail!("no solution yet, open it in your editor first");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_at_marker_line() {
        let code = format!("class Solution:\n    pass\n\n# {MARKER}\nprint(1)\n");
        assert_eq!(solution_part(&code), "class Solution:\n    pass\n\n");
        assert_eq!(solution_part("no marker"), "no marker");
    }

    #[test]
    fn parses_results() {
        assert_eq!(
            parse_result("PASS a\nFAIL b: x\nRESULT 1/2\n"),
            Some((1, 2))
        );
        assert_eq!(parse_result("Traceback ..."), None);
    }

    #[test]
    fn extracts_first_code_block() {
        let text = "Here:\n```python\nprint(1)\n```\ntrailing";
        assert_eq!(extract_code(text).as_deref(), Some("print(1)\n"));
        assert_eq!(extract_code("no code"), None);
    }
}
