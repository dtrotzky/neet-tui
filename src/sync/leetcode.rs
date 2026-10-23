//! Problem statements, examples and starter code from LeetCode's GraphQL API.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::AGENT;
use super::premium::PREMIUM_MARKER;
use crate::model::store::{Details, Problem, Store, write_json};
use crate::render::sanitize;

const LEETCODE_GRAPHQL: &str = "https://leetcode.com/graphql";

const QUESTION_QUERY: &str = "query q($titleSlug: String!) { question(titleSlug: $titleSlug) { \
    questionFrontendId content isPaidOnly exampleTestcaseList hints \
    topicTags { name } codeSnippets { langSlug code } } }";

pub(super) fn fetch_descriptions(store: &Store, problems: &[Problem], force: bool) -> Result<()> {
    let todo: Vec<&Problem> = problems
        .iter()
        .filter(|p| force || !store.details_path(&p.slug).exists())
        .collect();
    let total = todo.len();
    if total == 0 {
        println!("    all statements already cached (use --force to refetch)");
        return Ok(());
    }
    let queue = Mutex::new(todo);
    let done = AtomicUsize::new(0);
    let failed: Mutex<Vec<String>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| {
                loop {
                    let Some(p) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    if let Err(e) = fetch_one(store, p) {
                        failed.lock().unwrap().push(format!("{}: {e:#}", p.slug));
                    }
                    let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                    print!("\r    {n}/{total}");
                    let _ = std::io::stdout().flush();
                }
            });
        }
    });
    println!();
    let failed = failed.into_inner().unwrap();
    if !failed.is_empty() {
        eprintln!(
            "    {} statements failed (re-run `neet sync` to retry):",
            failed.len()
        );
        for f in failed.iter().take(10) {
            eprintln!("      {f}");
        }
    }
    Ok(())
}

fn fetch_one(store: &Store, p: &Problem) -> Result<()> {
    let body = json!({ "query": QUESTION_QUERY, "variables": { "titleSlug": p.slug } });
    let mut attempt = 0;
    let resp: Value = loop {
        attempt += 1;
        let res = AGENT
            .post(LEETCODE_GRAPHQL)
            .header("Referer", "https://leetcode.com")
            .send_json(&body);
        match res {
            Ok(mut r) => break r.body_mut().read_json()?,
            // Retry rate limits, server errors and network hiccups; not other 4xx.
            Err(ureq::Error::StatusCode(code)) if code != 429 && code < 500 => {
                bail!("HTTP {code}")
            }
            Err(_) if attempt < 4 => std::thread::sleep(Duration::from_secs(2 * attempt)),
            Err(e) => return Err(e.into()),
        }
    };
    save_question(store, p, &resp)
}

/// Writes `details.json` and the Markdown statement from a LeetCode GraphQL response.
fn save_question(store: &Store, p: &Problem, resp: &Value) -> Result<()> {
    let q = &resp["data"]["question"];
    if q.is_null() {
        match resp["errors"][0]["message"].as_str() {
            Some(msg) => bail!("LeetCode: {msg}"),
            None => bail!("not found on LeetCode"),
        }
    }
    let str_of = |v: &Value| sanitize(v.as_str().unwrap_or_default());
    let premium = q["isPaidOnly"].as_bool().unwrap_or(false);
    let content = q["content"].as_str().unwrap_or_default();
    if content.is_empty() {
        // Keep a statement Claude already wrote for this premium problem.
        if let Some(prev) = store.load_details(&p.slug)
            && !prev.description.starts_with(PREMIUM_MARKER)
        {
            return Ok(());
        }
    }
    let mut details = Details {
        id: q["questionFrontendId"].as_str().map(sanitize),
        premium,
        description: if content.is_empty() {
            format!(
                "{PREMIUM_MARKER}, so its statement isn't publicly available.\n\
                 See {} for the NeetCode version.",
                p.neetcode_url()
            )
        } else {
            html_to_text(content)
        },
        ..Default::default()
    };
    if let Some(hints) = q["hints"].as_array() {
        details.hints = hints.iter().map(|h| html_to_text(&str_of(h))).collect();
    }
    if let Some(tags) = q["topicTags"].as_array() {
        details.topics = tags.iter().map(|t| str_of(&t["name"])).collect();
    }
    if let Some(ex) = q["exampleTestcaseList"].as_array() {
        details.examples = ex.iter().map(str_of).collect();
    }
    if let Some(snips) = q["codeSnippets"].as_array() {
        for s in snips {
            details
                .snippets
                .insert(str_of(&s["langSlug"]), str_of(&s["code"]));
        }
    }

    let lists = [
        (p.blind75, "Blind 75"),
        (p.nc150, "NeetCode 150"),
        (p.nc250, "NeetCode 250"),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, n)| *n)
    .collect::<Vec<_>>()
    .join(", ");
    let id = details
        .id
        .as_deref()
        .map(|i| format!("{i}. "))
        .unwrap_or_default();
    let md = format!(
        "# {id}{}\n\n**{}** · {} · {lists}\n\n{}\n",
        p.title,
        p.difficulty,
        p.pattern,
        details.description.trim()
    );
    std::fs::create_dir_all(store.problem_dir(&p.slug))?;
    std::fs::write(store.description_path(&p.slug), md)?;
    write_json(&store.details_path(&p.slug), &details)?;
    Ok(())
}

fn html_to_text(html: &str) -> String {
    let text = html2text::from_read(html.as_bytes(), 10_000).unwrap_or_else(|_| html.to_string());
    let text = sanitize(&text);
    // Collapse runs of blank lines left over from &nbsp; paragraphs.
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::{temp_store, two_sum};

    #[test]
    fn saves_a_leetcode_question() {
        let store = temp_store("question");
        let p = two_sum();
        let resp = json!({ "data": { "question": {
            "questionFrontendId": "1",
            "isPaidOnly": false,
            "content": "<p>Given an array <code>nums</code>\u{1b}[2J, return indices.</p>",
            "exampleTestcaseList": ["[2,7,11,15]\n9"],
            "hints": ["<p>Use a <b>hash map</b>.</p>"],
            "topicTags": [{ "name": "Array" }],
            "codeSnippets": [{ "langSlug": "python3", "code": "class Solution:\n    pass" }],
        }}});
        save_question(&store, &p, &resp).unwrap();
        let d = store.load_details("two-sum").unwrap();
        assert_eq!(d.id.as_deref(), Some("1"));
        assert!(
            d.description.contains("Given an array"),
            "{}",
            d.description
        );
        assert!(!d.description.contains('\u{1b}'));
        assert_eq!(d.examples, ["[2,7,11,15]\n9"]);
        assert!(d.hints[0].contains("hash map"));
        assert_eq!(d.snippets["python3"], "class Solution:\n    pass");
        let md = store.load_description("two-sum").unwrap();
        assert!(
            md.starts_with("# 1. Two Sum\n\n**Easy** · Arrays & Hashing · Blind 75"),
            "{md}"
        );
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn premium_and_missing_questions() {
        let store = temp_store("premium");
        let p = two_sum();
        let premium = json!({ "data": { "question": { "isPaidOnly": true, "content": null }}});
        save_question(&store, &p, &premium).unwrap();
        let d = store.load_details("two-sum").unwrap();
        assert!(d.premium && d.description.starts_with(PREMIUM_MARKER));

        // A statement written later (by Claude) survives the next sync.
        let mut written = d.clone();
        written.description = "Our own statement".into();
        write_json(&store.details_path("two-sum"), &written).unwrap();
        save_question(&store, &p, &premium).unwrap();
        assert_eq!(
            store.load_details("two-sum").unwrap().description,
            "Our own statement"
        );

        let missing = json!({ "data": { "question": null }, "errors": [{ "message": "nope" }] });
        let err = save_question(&store, &p, &missing).unwrap_err();
        assert!(err.to_string().contains("nope"));
        let _ = std::fs::remove_dir_all(&store.root);
    }
}
