use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::model::lang::Lang;

/// NeetCode roadmap order of patterns.
pub const PATTERNS: &[&str] = &[
    "Arrays & Hashing",
    "Two Pointers",
    "Sliding Window",
    "Stack",
    "Binary Search",
    "Linked List",
    "Trees",
    "Tries",
    "Heap / Priority Queue",
    "Backtracking",
    "Graphs",
    "Advanced Graphs",
    "1-D Dynamic Programming",
    "2-D Dynamic Programming",
    "Greedy",
    "Intervals",
    "Math & Geometry",
    "Bit Manipulation",
];

/// Remote identifiers end up in file paths; only allow plain slug characters.
pub fn is_safe_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn pattern_rank(pattern: &str) -> usize {
    PATTERNS
        .iter()
        .position(|p| *p == pattern)
        .unwrap_or(PATTERNS.len())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    pub title: String,
    /// LeetCode slug, e.g. `contains-duplicate`.
    pub slug: String,
    /// NeetCode slug, e.g. `duplicate-integer`; also the article name.
    pub nc_slug: String,
    pub pattern: String,
    pub difficulty: String,
    /// Repo file stem, e.g. `0217-contains-duplicate`.
    pub code: Option<String>,
    pub video: Option<String>,
    pub blind75: bool,
    pub nc150: bool,
    pub nc250: bool,
}

impl Problem {
    pub fn file_stem(&self) -> String {
        self.code.clone().unwrap_or_else(|| self.slug.clone())
    }

    pub fn leetcode_url(&self) -> String {
        format!("https://leetcode.com/problems/{}/", self.slug)
    }

    pub fn neetcode_url(&self) -> String {
        format!("https://neetcode.io/problems/{}", self.nc_slug)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Details {
    pub id: Option<String>,
    /// Problem statement rendered to markdown-ish text.
    pub description: String,
    pub hints: Vec<String>,
    pub topics: Vec<String>,
    pub examples: Vec<String>,
    /// LeetCode language slug -> starter code.
    pub snippets: HashMap<String, String>,
    pub premium: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Todo,
    Attempted,
    Solved,
}

// `serde(default)` keeps older progress files loadable as fields are added.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub status: Status,
    /// Whether the reference solution was looked at.
    pub revealed: bool,
    pub hints_used: u32,
    pub last_opened: Option<DateTime<Local>>,
    pub solved_at: Option<DateTime<Local>>,
    /// Total time spent in the editor on this problem.
    pub time_spent_secs: u64,
    /// Spaced repetition: index into `srs::INTERVALS` and the next due date.
    pub review_stage: u8,
    pub next_review: Option<NaiveDate>,
    pub last_test: Option<TestRun>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TestRun {
    pub passed: u32,
    pub total: u32,
    pub at: DateTime<Local>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub entries: HashMap<String, Entry>,
}

/// One finished mock interview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockRecord {
    pub slug: String,
    pub title: String,
    pub difficulty: String,
    pub started: DateTime<Local>,
    pub elapsed_secs: u64,
    pub limit_secs: u64,
    /// Hiring signal, e.g. "Hire"; `None` if the debrief was skipped.
    pub result: Option<String>,
}

/// TUI state restored on the next start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub list: String,
    pub slug: Option<String>,
    pub hide_solved: bool,
}

/// Claude conversation memory for one problem, so nudges escalate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AiLog {
    pub entries: Vec<AiEntry>,
    /// The interactive tutor session (`c`), resumed until a fresh one is started.
    pub chat: Option<ChatSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    /// Claude Code session id.
    pub id: String,
    pub started: DateTime<Local>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiEntry {
    /// `hint`, `review` or `ask: <question>`.
    pub kind: String,
    pub at: DateTime<Local>,
    pub text: String,
}

impl AiLog {
    pub fn hint_count(&self) -> usize {
        self.entries.iter().filter(|e| e.kind == "hint").count()
    }
}

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
    pub workspace: PathBuf,
}

impl Store {
    pub fn new(cfg: &Config) -> Result<Self> {
        let root = crate::config::paths::data_home()
            .context("no data directory on this system")?
            .join("neet");
        let workspace = match &cfg.workspace {
            Some(w) if w.is_absolute() => w.clone(),
            Some(w) => {
                let home = dirs::home_dir().context("no home directory")?;
                home.join(w.strip_prefix("~").unwrap_or(w))
            }
            None => root.join("workspace"),
        };
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&workspace)?;
        Ok(Self { root, workspace })
    }

    pub fn problems_path(&self) -> PathBuf {
        self.root.join("problems.json")
    }

    pub fn problem_dir(&self, slug: &str) -> PathBuf {
        self.root.join("problems").join(slug)
    }

    pub fn description_path(&self, slug: &str) -> PathBuf {
        self.problem_dir(slug).join("description.md")
    }

    pub fn article_path(&self, slug: &str) -> PathBuf {
        self.problem_dir(slug).join("article.md")
    }

    pub fn details_path(&self, slug: &str) -> PathBuf {
        self.problem_dir(slug).join("details.json")
    }

    pub fn ref_solution_path(&self, slug: &str, lang: &Lang) -> PathBuf {
        self.problem_dir(slug)
            .join("solutions")
            .join(format!("{}.{}", lang.repo_dir, lang.ext))
    }

    pub fn solution_path(&self, p: &Problem, lang: &Lang) -> PathBuf {
        self.workspace
            .join(lang.key)
            .join(format!("{}.{}", p.file_stem(), lang.ext))
    }

    /// Private directory for compiled solutions (`{bin}` in run commands).
    pub fn build_dir(&self) -> PathBuf {
        self.root.join("build")
    }

    /// Cached, Claude-generated test harness for a problem in one language.
    pub fn harness_path(&self, p: &Problem, lang: &Lang) -> PathBuf {
        self.root
            .join("tests")
            .join(lang.key)
            .join(format!("{}.{}", p.file_stem(), lang.ext))
    }

    /// Your own notes, kept in the workspace so they live next to your code.
    pub fn notes_path(&self, p: &Problem) -> PathBuf {
        self.workspace
            .join("notes")
            .join(format!("{}.md", p.file_stem()))
    }

    pub fn load_notes(&self, p: &Problem) -> Option<String> {
        std::fs::read_to_string(self.notes_path(p))
            .ok()
            .filter(|n| !n.trim().is_empty())
    }

    pub fn attempts_dir(&self, lang: &Lang) -> PathBuf {
        self.workspace.join(lang.key).join("attempts")
    }

    pub fn coach_path(&self) -> PathBuf {
        self.root.join("coach.md")
    }

    fn mocks_path(&self) -> PathBuf {
        self.root.join("mocks.json")
    }

    fn ui_path(&self) -> PathBuf {
        self.root.join("ui.json")
    }

    pub fn load_mocks(&self) -> Result<Vec<MockRecord>> {
        Ok(read_json_opt(&self.mocks_path())?.unwrap_or_default())
    }

    pub fn add_mock(&self, record: MockRecord) -> Result<()> {
        let mut mocks = self.load_mocks()?;
        mocks.push(record);
        write_json(&self.mocks_path(), &mocks)
    }

    pub fn load_ui(&self) -> UiState {
        read_json_opt(&self.ui_path())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn save_ui(&self, ui: &UiState) -> Result<()> {
        write_json(&self.ui_path(), ui)
    }

    fn progress_path(&self) -> PathBuf {
        self.root.join("progress.json")
    }

    fn ai_path(&self, slug: &str) -> PathBuf {
        self.root.join("ai").join(format!("{slug}.json"))
    }

    pub fn has_data(&self) -> bool {
        self.problems_path().exists()
    }

    pub fn load_problems(&self) -> Result<Vec<Problem>> {
        let problems: Vec<Problem> = read_json(&self.problems_path())
            .context("no problem data found, run `neet sync` first (needs network once)")?;
        Ok(problems
            .into_iter()
            .filter(|p| is_safe_ident(&p.slug) && is_safe_ident(&p.file_stem()))
            .collect())
    }

    pub fn save_problems(&self, problems: &[Problem]) -> Result<()> {
        write_json(&self.problems_path(), &problems)
    }

    pub fn load_details(&self, slug: &str) -> Option<Details> {
        read_json_opt(&self.details_path(slug)).ok().flatten()
    }

    pub fn load_description(&self, slug: &str) -> Option<String> {
        std::fs::read_to_string(self.description_path(slug)).ok()
    }

    pub fn load_article(&self, slug: &str) -> Option<String> {
        std::fs::read_to_string(self.article_path(slug)).ok()
    }

    pub fn load_ref_solution(&self, slug: &str, lang: &Lang) -> Option<String> {
        std::fs::read_to_string(self.ref_solution_path(slug, lang)).ok()
    }

    /// Fails on a corrupt file instead of returning empty progress that would
    /// then overwrite it.
    pub fn load_progress(&self) -> Result<Progress> {
        let path = self.progress_path();
        let mut progress: Progress = read_json_opt(&path)
            .with_context(|| format!("{} is corrupt; fix or move it", path.display()))?
            .unwrap_or_default();
        // Problems solved before spaced repetition existed: schedule from their solve date.
        for e in progress.entries.values_mut() {
            if e.status == Status::Solved && e.next_review.is_none() && e.review_stage == 0 {
                let base = e
                    .solved_at
                    .map_or_else(crate::model::srs::today, |d| d.date_naive());
                crate::model::srs::schedule_first(e, base);
            }
        }
        Ok(progress)
    }

    /// Read-modify-write against the file on disk, so changes made by another
    /// `neet` process (e.g. `neet review` from inside nvim) are not lost.
    pub fn update_progress(&self, f: impl FnOnce(&mut Progress)) -> Result<Progress> {
        let mut progress = self.load_progress()?;
        f(&mut progress);
        write_json(&self.progress_path(), &progress)?;
        Ok(progress)
    }

    pub fn load_ai(&self, slug: &str) -> Result<AiLog> {
        let path = self.ai_path(slug);
        Ok(read_json_opt(&path)
            .with_context(|| format!("{} is corrupt; fix or move it", path.display()))?
            .unwrap_or_default())
    }

    pub fn save_ai(&self, slug: &str, log: &AiLog) -> Result<()> {
        write_json(&self.ai_path(slug), log)
    }

    /// Returns the solution file, creating it from the starter snippet if needed.
    pub fn ensure_solution(&self, p: &Problem, lang: &Lang) -> Result<PathBuf> {
        let path = self.solution_path(p, lang);
        if path.exists() {
            return Ok(path);
        }
        std::fs::create_dir_all(path.parent().context("solution path has no parent")?)?;
        let details = self.load_details(&p.slug).unwrap_or_default();
        let c = lang.comment;
        let mut out = String::new();
        out.push_str(&format!("{c} {} ({})\n", p.title, p.difficulty));
        out.push_str(&format!("{c} {}\n", p.leetcode_url()));
        out.push_str(&format!("{c} Pattern: {}\n", p.pattern));
        if !details.examples.is_empty() {
            out.push_str(&format!("{c}\n{c} Example inputs:\n"));
            for ex in &details.examples {
                for line in ex.lines() {
                    out.push_str(&format!("{c}   {line}\n"));
                }
                out.push_str(&format!("{c}\n"));
            }
        }
        out.push('\n');
        out.push_str(lang.prelude);
        let snippet = details.snippets.get(lang.lc_slug);
        for (name, def) in lang.node_defs {
            if snippet.is_some_and(|s| s.contains(name)) {
                out.push_str(def);
            }
        }
        match snippet {
            Some(snippet) => {
                let snippet = snippet.trim_end();
                out.push_str(snippet);
                // Python snippets end in an empty `def ...:`, which doesn't parse.
                let last = snippet.lines().last().unwrap_or_default();
                if last.ends_with(':') {
                    let indent = last.len() - last.trim_start().len() + 4;
                    out.push_str(&format!("\n{}pass", " ".repeat(indent)));
                }
            }
            None => out.push_str(&format!("{c} (no starter code available for this problem)")),
        }
        // Everything below the marker is scratch space that `neet test` replaces.
        out.push_str(&format!("\n\n{c} {}\n", crate::judge::harness::MARKER));
        out.push_str(lang.harness.trim_start_matches('\n'));
        // create_new: never clobber a solution that appeared in the meantime.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => f.write_all(out.as_bytes())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        Ok(path)
    }
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Like [`read_json`], but a missing file is `None` rather than an error.
pub fn read_json_opt<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Unique per process and thread so concurrent writers never share a temp file.
    let tmp = path.with_extension(format!(
        "json.{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
