//! Progress statistics and weak-spot detection, shared by CLI and TUI.

use chrono::NaiveDate;

use crate::srs;
use crate::store::{MockRecord, Problem, Progress, Status};

#[derive(Debug, Default, Clone)]
pub struct PatternStats {
    pub pattern: String,
    pub total: usize,
    pub attempted: usize,
    pub solved: usize,
    pub hints: u32,
    pub reveals: usize,
    pub time_secs: u64,
    pub due: usize,
    pub tests_failing: usize,
}

impl PatternStats {
    /// Higher means more help needed per problem you've touched.
    pub fn weakness(&self) -> f64 {
        if self.attempted == 0 {
            return 0.0;
        }
        let n = self.attempted as f64;
        let unsolved = (self.attempted - self.solved) as f64;
        (f64::from(self.hints) + 2.0 * self.reveals as f64 + unsolved + self.tests_failing as f64)
            / n
    }
}

pub fn by_pattern(
    problems: &[Problem],
    progress: &Progress,
    include: impl Fn(&Problem) -> bool,
    today: NaiveDate,
) -> Vec<PatternStats> {
    let mut out: Vec<PatternStats> = Vec::new();
    for p in problems.iter().filter(|p| include(p)) {
        let idx = match out.iter().position(|s| s.pattern == p.pattern) {
            Some(i) => i,
            None => {
                out.push(PatternStats {
                    pattern: p.pattern.clone(),
                    ..Default::default()
                });
                out.len() - 1
            }
        };
        let s = &mut out[idx];
        s.total += 1;
        let Some(e) = progress.entries.get(&p.slug) else {
            continue;
        };
        if e.status != Status::Todo {
            s.attempted += 1;
        }
        if e.status == Status::Solved {
            s.solved += 1;
        }
        s.hints += e.hints_used;
        s.reveals += usize::from(e.revealed);
        s.time_secs += e.time_spent_secs;
        s.due += usize::from(srs::is_due(e, today));
        if e.last_test.is_some_and(|t| t.passed < t.total) {
            s.tests_failing += 1;
        }
    }
    out.sort_by_key(|s| crate::store::pattern_rank(&s.pattern));
    out
}

/// Patterns that need the most work, worst first (only ones you've started).
pub fn weakest(stats: &[PatternStats], n: usize) -> Vec<&PatternStats> {
    // Ignore patterns with a single untouched-by-help attempt: too little signal.
    let mut v: Vec<&PatternStats> = stats
        .iter()
        .filter(|s| s.weakness() > 0.5 && (s.attempted >= 2 || s.hints > 0 || s.reveals > 0))
        .collect();
    v.sort_by(|a, b| b.weakness().total_cmp(&a.weakness()));
    v.truncate(n);
    v
}

pub fn fmt_time(secs: u64) -> String {
    match secs {
        0 => "-".into(),
        s if s < 3600 => format!("{}m", s.div_ceil(60)),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Plain-text report (a monospace table), used by `neet stats`, the TUI and the coach prompt.
pub fn report(
    problems: &[Problem],
    progress: &Progress,
    mocks: &[MockRecord],
    list_name: &str,
    include: impl Fn(&Problem) -> bool,
    today: NaiveDate,
) -> String {
    let stats = by_pattern(problems, progress, include, today);
    let mut s = format!(
        "{:<24} {:>7} {:>5} {:>7} {:>7} {:>6} {:>4}\n",
        list_name, "solved", "tried", "nudges", "reveals", "time", "due"
    );
    let mut total = PatternStats::default();
    let weak: Vec<&str> = weakest(&stats, 3)
        .iter()
        .map(|w| w.pattern.as_str())
        .collect();
    for p in &stats {
        let focus = if weak.contains(&p.pattern.as_str()) {
            "  ← focus"
        } else {
            ""
        };
        s.push_str(&format!(
            "{:<24} {:>7} {:>5} {:>7} {:>7} {:>6} {:>4}{focus}\n",
            p.pattern,
            format!("{}/{}", p.solved, p.total),
            p.attempted,
            p.hints,
            p.reveals,
            fmt_time(p.time_secs),
            p.due,
        ));
        total.total += p.total;
        total.solved += p.solved;
        total.attempted += p.attempted;
        total.hints += p.hints;
        total.reveals += p.reveals;
        total.time_secs += p.time_secs;
        total.due += p.due;
    }
    s.push_str(&format!(
        "{:<24} {:>7} {:>5} {:>7} {:>7} {:>6} {:>4}\n",
        "Total",
        format!("{}/{}", total.solved, total.total),
        total.attempted,
        total.hints,
        total.reveals,
        fmt_time(total.time_secs),
        total.due,
    ));
    if !mocks.is_empty() {
        s.push_str("\nRecent mock interviews:\n");
        for m in mocks.iter().rev().take(8) {
            s.push_str(&format!(
                "  {}  {:<40} {:>6} {:>5}/{:<5} {}\n",
                m.started.format("%Y-%m-%d"),
                m.title,
                m.difficulty,
                crate::mock::fmt_duration(std::time::Duration::from_secs(m.elapsed_secs)),
                crate::mock::fmt_duration(std::time::Duration::from_secs(m.limit_secs)),
                m.result.as_deref().unwrap_or("(no debrief)"),
            ));
        }
    }
    s
}

pub fn coach_prompt(report: &str) -> String {
    format!(
        "Here are the candidate's NeetCode practice statistics. Columns: problems solved per \
pattern, problems tried, nudges (hints) used, solutions revealed, time spent, reviews due; plus \
recent mock interview results.\n\n```\n{report}```\n\n\
Give a short weak-spot analysis for their Google-style interview prep: which 2-3 patterns to \
focus on and why (cite the numbers), what that suggests about their understanding, and a \
concrete plan for the next week (which kinds of problems, how many per day, when to do mock \
interviews). Under 250 words. Use `##` headings and bullets."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Entry;

    fn problem(slug: &str, pattern: &str) -> Problem {
        Problem {
            title: slug.into(),
            slug: slug.into(),
            nc_slug: slug.into(),
            pattern: pattern.into(),
            difficulty: "Easy".into(),
            code: None,
            video: None,
            blind75: false,
            nc150: true,
            nc250: true,
        }
    }

    #[test]
    fn weakest_pattern_is_the_one_needing_help() {
        let problems = vec![
            problem("a", "Arrays & Hashing"),
            problem("b", "Arrays & Hashing"),
            problem("c", "Trees"),
        ];
        let mut progress = Progress::default();
        progress.entries.insert(
            "a".into(),
            Entry {
                status: Status::Solved,
                ..Default::default()
            },
        );
        progress.entries.insert(
            "c".into(),
            Entry {
                status: Status::Attempted,
                hints_used: 3,
                revealed: true,
                ..Default::default()
            },
        );
        let today = srs::today();
        let stats = by_pattern(&problems, &progress, |_| true, today);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].solved, 1);
        let weak = weakest(&stats, 3);
        assert_eq!(weak.len(), 1);
        assert_eq!(weak[0].pattern, "Trees");
    }
}
