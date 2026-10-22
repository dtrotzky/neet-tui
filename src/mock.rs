//! Mock interviews: a timed attempt with help locked, then a debrief where
//! Claude plays the interviewer and gives a hiring signal.

use std::time::Duration;

use crate::claude::Coaching;
use crate::config::Config;
use crate::store::Problem;

/// Final debrief line, e.g. `MOCK RESULT: Lean Hire`.
const RESULT_PREFIX: &str = "MOCK RESULT:";
pub const RESULTS: &[&str] = &[
    "Strong Hire",
    "Hire",
    "Lean Hire",
    "Lean No Hire",
    "No Hire",
];
/// The interviewer must wrap up after this many questions.
const MAX_QUESTIONS: usize = 4;
/// Typing this ends the discussion and asks for the evaluation.
pub const DONE: &str = "/done";

pub fn time_limit(cfg: &Config, p: &Problem) -> Duration {
    let minutes = match p.difficulty.as_str() {
        "Easy" => cfg.mock.easy,
        "Hard" => cfg.mock.hard,
        _ => cfg.mock.medium,
    };
    Duration::from_secs(minutes * 60)
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    Interviewer,
    Candidate,
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub turns: Vec<(Speaker, String)>,
}

impl Transcript {
    pub fn push(&mut self, who: Speaker, text: impl Into<String>) {
        self.turns.push((who, text.into()));
    }

    fn questions(&self) -> usize {
        self.turns
            .iter()
            .filter(|(w, _)| *w == Speaker::Interviewer)
            .count()
    }

    pub fn to_markdown(&self) -> String {
        self.turns
            .iter()
            .map(|(who, text)| match who {
                Speaker::Interviewer => format!("**Interviewer:** {}", text.trim()),
                Speaker::Candidate => format!("**You:** {}", text.trim()),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The interviewer kept going well past the limit without a verdict: stop anyway.
    pub fn gave_up(&self) -> bool {
        self.questions() > MAX_QUESTIONS + 2
    }

    /// The interviewer has given the final evaluation.
    pub fn finished(&self) -> bool {
        self.turns
            .last()
            .is_some_and(|(w, t)| *w == Speaker::Interviewer && result_of(t).is_some())
    }
}

pub fn debrief_prompt(
    ctx: &Coaching,
    elapsed: Duration,
    limit: Duration,
    transcript: &Transcript,
) -> String {
    let wrap_up = transcript.questions() >= MAX_QUESTIONS
        || transcript
            .turns
            .last()
            .is_some_and(|(w, t)| *w == Speaker::Candidate && t.trim() == DONE);
    let mut s = format!(
        "{}You are now the INTERVIEWER in a mock coding interview (Google style). The candidate \
just finished coding this problem under a time limit: they used {} of {}. Their final code is \
in <candidate_code>. They had no hints and no access to the solution.\n\n\
Run the post-coding discussion like a real interviewer: ask ONE question per message, e.g. \
walk me through your approach, the time and space complexity, a specific edge case or a bug \
you noticed in their code, and one follow-up or variation. Keep each message under 80 words, \
be natural and professional, and never reveal the solution.\n\n\
When you have asked enough (at most {MAX_QUESTIONS} questions), or when told to wrap up, give \
the evaluation instead of a question: a `## Evaluation` section rating Problem solving, Coding, \
Communication and Testing (each Strong / OK / Weak with one sentence), then what to practise \
next, then a final line exactly `{RESULT_PREFIX} <one of: {}>`.\n\n",
        ctx.common(),
        fmt_duration(elapsed),
        fmt_duration(limit),
        RESULTS.join(" | "),
    );
    if transcript.turns.is_empty() {
        s.push_str("Start the discussion with your first question.");
    } else {
        s.push_str("<transcript>\n");
        for (who, text) in &transcript.turns {
            let name = match who {
                Speaker::Interviewer => "Interviewer",
                Speaker::Candidate => "Candidate",
            };
            s.push_str(&format!("{name}: {}\n\n", text.trim()));
        }
        s.push_str("</transcript>\n\n");
        if wrap_up {
            s.push_str("Wrap up now: give the evaluation.");
        } else {
            s.push_str("Reply with only the interviewer's next message.");
        }
    }
    s
}

/// The hiring signal from a final evaluation, if present.
pub fn result_of(text: &str) -> Option<String> {
    let line = text
        .lines()
        .rev()
        .find(|l| l.contains(RESULT_PREFIX))?
        .trim()
        .trim_matches(|c| c == '`' || c == '*');
    let value = line
        .split_once(RESULT_PREFIX)?
        .1
        .trim()
        .trim_matches(|c: char| c == '`' || c == '*' || c == '.' || c.is_whitespace());
    RESULTS
        .iter()
        .find(|r| r.eq_ignore_ascii_case(value))
        .map(|r| r.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hiring_signal() {
        assert_eq!(
            result_of("## Evaluation\n...\nMOCK RESULT: Lean Hire").as_deref(),
            Some("Lean Hire")
        );
        assert_eq!(
            result_of("`MOCK RESULT: no hire`").as_deref(),
            Some("No Hire")
        );
        assert_eq!(
            result_of("MOCK RESULT: **Strong Hire**.").as_deref(),
            Some("Strong Hire")
        );
        assert_eq!(result_of("What is the complexity?"), None);
        assert_eq!(result_of("MOCK RESULT: maybe"), None);
    }

    #[test]
    fn finished_only_after_evaluation() {
        let mut t = Transcript::default();
        t.push(Speaker::Interviewer, "Walk me through it.");
        assert!(!t.finished());
        t.push(Speaker::Candidate, DONE);
        assert!(!t.finished());
        t.push(Speaker::Interviewer, "## Evaluation\nMOCK RESULT: Hire");
        assert!(t.finished());
    }
}
