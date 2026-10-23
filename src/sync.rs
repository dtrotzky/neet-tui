//! One-time download of everything needed to practice offline:
//! the NeetCode problem lists, NeetCode solution articles and code
//! (from the neetcode-gh/leetcode repo), and LeetCode problem statements.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::claude;
use crate::config::Config;
use crate::lang::LANGS;
use crate::proc::Cancel;
use crate::render::sanitize;
use crate::store::{Details, Problem, Store, is_safe_ident, pattern_rank, write_json};

const PRACTICE_URL: &str = "https://neetcode.io/practice";
const TARBALL_URL: &str = "https://codeload.github.com/neetcode-gh/leetcode/tar.gz/refs/heads/main";
const LEETCODE_GRAPHQL: &str = "https://leetcode.com/graphql";
const USER_AGENT: &str = "neet (offline NeetCode practice; https://github.com/dtrotzky/neet-tui)";
/// Upper bound for any single downloaded document (HTML, JS bundle, JSON).
const MAX_DOC: u64 = 32 * 1024 * 1024;
/// Upper bound for a single file extracted from the repo tarball.
const MAX_ENTRY: u64 = 4 * 1024 * 1024;

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(USER_AGENT)
        .build()
        .into()
});

pub fn run(store: &Store, cfg: &Config, force: bool) -> Result<()> {
    println!("==> Fetching NeetCode problem lists");
    let problems = match fetch_lists_from_site() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("    neetcode.io lists unavailable ({e:#})");
            match store.load_problems() {
                // An earlier sync is usually newer than the bundled snapshot.
                Ok(cached) if cached.iter().any(|p| p.nc250) => {
                    eprintln!("    keeping the cached problem list");
                    cached
                }
                _ => {
                    eprintln!("    using the problem list bundled with neet");
                    bundled_lists()?
                }
            }
        }
    };
    let count = |f: fn(&Problem) -> bool| problems.iter().filter(|p| f(p)).count();
    println!(
        "    {} problems (Blind 75: {}, NeetCode 150: {}, NeetCode 250: {})",
        problems.len(),
        count(|p| p.blind75),
        count(|p| p.nc150),
        count(|p| p.nc250),
    );
    store.save_problems(&problems)?;

    println!("==> Downloading NeetCode solutions (neetcode-gh/leetcode)");
    let (articles, codes) = fetch_repo(store, &problems)?;
    println!("    {articles} solution articles, {codes} solution files");

    println!("==> Fetching problem statements from LeetCode");
    fetch_descriptions(store, &problems, force)?;
    write_premium_statements(store, cfg, &problems)?;
    write_premium_starters(store, cfg, &problems)?;

    println!("==> Done. Data stored in {}", store.root.display());
    Ok(())
}

fn get(url: &str, timeout: Duration) -> Result<impl Read> {
    let resp = AGENT
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .with_context(|| format!("GET {url}"))?;
    Ok(resp.into_body().into_reader())
}

fn get_string(url: &str) -> Result<String> {
    let mut buf = Vec::new();
    get(url, Duration::from_secs(60))?
        .take(MAX_DOC)
        .read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------------------------------------------------------------------
// Problem lists

fn fetch_lists_from_site() -> Result<Vec<Problem>> {
    let html = get_string(PRACTICE_URL)?;
    let bundle = html
        .split('"')
        .find(|s| s.starts_with("main.") && s.ends_with(".js"))
        .context("could not locate main bundle on neetcode.io")?;
    let js = get_string(&format!("https://neetcode.io/{bundle}"))?;
    let problems = problems_from_objects(parse_js_objects(&js, "{problem:"));
    if problems.iter().filter(|p| p.nc250).count() < 200 {
        bail!("bundle format changed (found too few NeetCode 250 problems)");
    }
    Ok(problems)
}

/// Snapshot of the lists (titles, slugs, patterns, flags; no statements or solutions), used
/// when neetcode.io's bundle can't be parsed. To refresh it, run `neet sync` and copy
/// `<data>/problems.json` here with one problem per line.
const BUNDLED_LISTS: &str = include_str!("../data/problems.json");

fn bundled_lists() -> Result<Vec<Problem>> {
    serde_json::from_str(BUNDLED_LISTS).context("bundled problem list is invalid")
}

#[derive(Debug, Clone)]
enum JsVal {
    Str(String),
    Bool(bool),
    Num,
}

fn problems_from_objects(objs: Vec<HashMap<String, JsVal>>) -> Vec<Problem> {
    let mut order: Vec<String> = Vec::new();
    let mut by_slug: HashMap<String, Problem> = HashMap::new();
    for o in objs {
        let s = |k: &str| match o.get(k) {
            Some(JsVal::Str(s)) if !s.is_empty() => Some(sanitize(s)),
            _ => None,
        };
        let b = |k: &str| matches!(o.get(k), Some(JsVal::Bool(true)));
        let (Some(title), Some(link)) = (s("problem"), s("link")) else {
            continue;
        };
        let (blind75, nc150, nc250) = (b("blind75"), b("neetcode150"), b("neetcode250"));
        if !(blind75 || nc150 || nc250) {
            continue;
        }
        let slug = link
            .trim_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        // Slugs and codes become file names: reject anything but plain slugs.
        if !is_safe_ident(&slug) {
            continue;
        }
        let nc_slug = s("ncLink")
            .map(|l| l.trim_matches('/').to_string())
            .filter(|l| is_safe_ident(l))
            .unwrap_or_else(|| slug.clone());
        let code = s("code").filter(|c| is_safe_ident(c));
        let entry = by_slug.entry(slug.clone()).or_insert_with(|| {
            order.push(slug.clone());
            Problem {
                title,
                slug: slug.clone(),
                nc_slug,
                pattern: s("pattern").unwrap_or_else(|| "Other".into()),
                difficulty: s("difficulty").unwrap_or_else(|| "Medium".into()),
                code: code.clone(),
                video: s("video").filter(|v| is_safe_ident(v)),
                blind75: false,
                nc150: false,
                nc250: false,
            }
        });
        entry.blind75 |= blind75;
        entry.nc150 |= nc150;
        entry.nc250 |= nc250;
        if entry.code.is_none() {
            entry.code = code;
        }
    }
    let mut problems: Vec<Problem> = order
        .into_iter()
        .filter_map(|s| by_slug.remove(&s))
        .collect();
    // Stable sort keeps NeetCode's within-pattern order.
    problems.sort_by_key(|p| pattern_rank(&p.pattern));
    problems
}

/// Extracts flat object literals (`{key:"str",flag:!0,n:1}`) that start with
/// `marker` from minified JavaScript. Objects that don't parse are skipped.
fn parse_js_objects(js: &str, marker: &str) -> Vec<HashMap<String, JsVal>> {
    let bytes = js.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = js[from..].find(marker) {
        let start = from + off;
        from = start + 1;
        if let Some((obj, end)) = parse_object(bytes, start) {
            out.push(obj);
            from = end;
        }
    }
    out
}

fn parse_object(b: &[u8], mut i: usize) -> Option<(HashMap<String, JsVal>, usize)> {
    let mut obj = HashMap::new();
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        let key_start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
            i += 1;
        }
        if i == key_start || b.get(i) != Some(&b':') {
            return None;
        }
        let key = std::str::from_utf8(&b[key_start..i]).ok()?.to_string();
        i += 1;
        let val = match *b.get(i)? {
            q @ (b'"' | b'\'') => {
                let (s, end) = parse_string(b, i + 1, q)?;
                i = end;
                JsVal::Str(s)
            }
            b'!' => {
                let v = *b.get(i + 1)? == b'0';
                i += 2;
                JsVal::Bool(v)
            }
            c if c.is_ascii_digit() || c == b'-' => {
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                JsVal::Num
            }
            _ => return None,
        };
        obj.insert(key, val);
        match *b.get(i)? {
            b',' => i += 1,
            b'}' => return Some((obj, i + 1)),
            _ => return None,
        }
    }
}

fn parse_string(b: &[u8], mut i: usize, quote: u8) -> Option<(String, usize)> {
    let mut out: Vec<u8> = Vec::new();
    while i < b.len() {
        match b[i] {
            c if c == quote => return Some((String::from_utf8(out).ok()?, i + 1)),
            b'\\' => {
                let c = *b.get(i + 1)?;
                i += 2;
                match c {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'x' => {
                        out.push(hex_at(b, i, 2)? as u8);
                        i += 2;
                    }
                    b'u' => {
                        let mut cp = hex_at(b, i, 4)?;
                        i += 4;
                        // Surrogate pair: \uD83D\uDE00
                        if (0xD800..0xDC00).contains(&cp)
                            && b.get(i..i + 2) == Some(b"\\u")
                            && let Some(lo) = hex_at(b, i + 2, 4)
                            && (0xDC00..0xE000).contains(&lo)
                        {
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            i += 6;
                        }
                        let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    other => out.push(other),
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None
}

fn hex_at(b: &[u8], i: usize, len: usize) -> Option<u32> {
    let hex = std::str::from_utf8(b.get(i..i + len)?).ok()?;
    u32::from_str_radix(hex, 16).ok()
}

// ---------------------------------------------------------------------------
// NeetCode repo: articles + per-language solutions

fn fetch_repo(store: &Store, problems: &[Problem]) -> Result<(usize, usize)> {
    extract_repo(store, problems, get(TARBALL_URL, Duration::from_secs(600))?)
}

/// Saves the articles and solution files for known problems from the repo's `.tar.gz`.
/// Destinations come from the problem list, never from names inside the archive.
fn extract_repo(store: &Store, problems: &[Problem], tgz: impl Read) -> Result<(usize, usize)> {
    let mut by_article: HashMap<&str, &str> = HashMap::new();
    let mut by_code: HashMap<&str, &str> = HashMap::new();
    for p in problems {
        by_article.insert(&p.nc_slug, &p.slug);
        by_code.insert(p.code.as_deref().unwrap_or(&p.slug), &p.slug);
    }
    // LeetCode-slug-named articles are used only when no NeetCode-named one exists.
    let mut fallback_articles: HashMap<&str, &str> = HashMap::new();
    for p in problems {
        if !by_article.contains_key(p.slug.as_str()) {
            fallback_articles.insert(&p.slug, &p.slug);
        }
    }
    let lang_dirs: HashSet<&str> = LANGS.iter().map(|l| l.repo_dir).collect();
    let mut have_article: HashSet<String> = HashSet::new();
    let (mut articles, mut codes) = (0, 0);

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        // <repo-root>/<dir>/<file>
        let [_, dir, file] = parts.as_slice() else {
            continue;
        };
        let Some((stem, ext)) = file.rsplit_once('.') else {
            continue;
        };
        if dir == "articles" && ext == "md" {
            let primary = by_article.get(stem).copied();
            let slug = primary.or_else(|| fallback_articles.get(stem).copied());
            if let Some(slug) = slug {
                if primary.is_none() && have_article.contains(slug) {
                    continue;
                }
                let text = read_entry(&mut entry)?;
                let dest = store.article_path(slug);
                std::fs::create_dir_all(store.problem_dir(slug))?;
                std::fs::write(dest, text)?;
                if have_article.insert(slug.to_string()) {
                    articles += 1;
                }
            }
        } else if lang_dirs.contains(dir.as_str())
            && let Some(slug) = by_code.get(stem)
        {
            let Some(lang) = LANGS.iter().find(|l| l.repo_dir == dir) else {
                continue;
            };
            let dest = store.ref_solution_path(slug, lang);
            std::fs::create_dir_all(store.problem_dir(slug).join("solutions"))?;
            std::fs::write(dest, read_entry(&mut entry)?)?;
            codes += 1;
        }
    }
    Ok((articles, codes))
}

/// Reads a tarball entry as sanitized text (lossy UTF-8, size-capped).
fn read_entry(entry: &mut impl Read) -> Result<String> {
    let mut buf = Vec::new();
    entry.take(MAX_ENTRY).read_to_end(&mut buf)?;
    Ok(sanitize(&String::from_utf8_lossy(&buf)))
}

// ---------------------------------------------------------------------------
// LeetCode problem statements

const QUESTION_QUERY: &str = "query q($titleSlug: String!) { question(titleSlug: $titleSlug) { \
    questionFrontendId content isPaidOnly exampleTestcaseList hints \
    topicTags { name } codeSnippets { langSlug code } } }";

fn fetch_descriptions(store: &Store, problems: &[Problem], force: bool) -> Result<()> {
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

const PREMIUM_MARKER: &str = "This is a LeetCode Premium problem";

/// Premium statements aren't public; have Claude write an equivalent one, once.
fn write_premium_statements(store: &Store, cfg: &Config, problems: &[Problem]) -> Result<()> {
    let todo: Vec<&Problem> = problems
        .iter()
        .filter(|p| {
            store
                .load_details(&p.slug)
                .is_some_and(|d| d.premium && d.description.starts_with(PREMIUM_MARKER))
        })
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    println!(
        "==> Asking Claude to write statements for {} LeetCode Premium problems",
        todo.len()
    );
    let queue = Mutex::new(todo);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(p) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    match premium_statement(store, cfg, p) {
                        Ok(()) => println!("    wrote {}", p.title),
                        Err(e) => eprintln!("    {}: {e:#} (will retry on next sync)", p.title),
                    }
                }
            });
        }
    });
    Ok(())
}

fn premium_statement(store: &Store, cfg: &Config, p: &Problem) -> Result<()> {
    let article = store.load_article(&p.slug).unwrap_or_default();
    let prompt = format!(
        "Write the problem statement for the LeetCode problem \"{}\" (slug `{}`), \
as NeetCode presents it. Below is NeetCode's solution write-up so you match its function \
signature and conventions. Output ONLY the statement in plain Markdown: description, the \
function to implement (as `name(params) -> return`), 2-3 examples formatted as \
`**Example 1:**` / `**Input:** ...` / `**Output:** ...` / optional `**Explanation:** ...`, \
and a `**Constraints:**` bullet list. No headings, no hints, no solution.\n\n\
<writeup>\n{}\n</writeup>",
        p.title,
        p.slug,
        article.chars().take(6000).collect::<String>()
    );
    let text = sanitize(&claude::run(
        cfg,
        claude::Kind::Sync,
        &prompt,
        &Cancel::default(),
        |_| {},
    )?);
    if text.trim().is_empty() {
        bail!("empty response");
    }
    let mut details = store.load_details(&p.slug).unwrap_or_default();
    details.description = text.trim().to_string();
    write_json(&store.details_path(&p.slug), &details)?;
    let old = store.load_description(&p.slug).unwrap_or_default();
    let header: String = old
        .split_inclusive('\n')
        .take_while(|l| !l.starts_with(PREMIUM_MARKER))
        .collect();
    let md = format!(
        "{header}{}\n\n> LeetCode Premium problem: statement written by Claude from NeetCode's write-up.\n",
        details.description
    );
    std::fs::write(store.description_path(&p.slug), md)?;
    Ok(())
}

/// LeetCode has no starter code for premium problems; have Claude write it, once.
fn write_premium_starters(store: &Store, cfg: &Config, problems: &[Problem]) -> Result<()> {
    let todo: Vec<&Problem> = problems
        .iter()
        .filter(|p| {
            store.load_details(&p.slug).is_some_and(|d| {
                d.premium && d.snippets.is_empty() && !d.description.starts_with(PREMIUM_MARKER)
            })
        })
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    println!(
        "==> Asking Claude for starter code for {} LeetCode Premium problems",
        todo.len()
    );
    let queue = Mutex::new(todo);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(p) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    match premium_starter(store, cfg, p) {
                        Ok(n) => println!("    {}: {n} languages", p.title),
                        Err(e) => eprintln!("    {}: {e:#} (will retry on next sync)", p.title),
                    }
                }
            });
        }
    });
    Ok(())
}

fn premium_starter(store: &Store, cfg: &Config, p: &Problem) -> Result<usize> {
    let mut details = store.load_details(&p.slug).context("no details")?;
    let slugs: Vec<&str> = LANGS.iter().map(|l| l.lc_slug).collect();
    let prompt = format!(
        "Write LeetCode-style starter code (signature only, empty body) for this problem in \
each of these languages: {}.\n\n<statement>\n{}\n</statement>\n\n\
Match the function name and types in the statement. Use LeetCode conventions: a `Solution` \
class (`impl Solution` in Rust, plain functions in Go/JavaScript/TypeScript as LeetCode does). \
If the problem uses a helper type such as `Interval`, include its definition above the \
Solution. Reply with one fenced code block per language, the fence labelled with exactly the \
language name from the list (e.g. ```python3). No other text.",
        slugs.join(", "),
        details.description
    );
    let text = claude::run(cfg, claude::Kind::Sync, &prompt, &Cancel::default(), |_| {})?;
    for (label, code) in fenced_blocks(&text) {
        if slugs.contains(&label.as_str()) {
            details.snippets.insert(label, sanitize(code.trim_end()));
        }
    }
    if details.snippets.is_empty() {
        bail!("no usable code blocks in the reply");
    }
    write_json(&store.details_path(&p.slug), &details)?;
    Ok(details.snippets.len())
}

/// All fenced code blocks in `text` as (label, code).
fn fenced_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        let fence = line.trim_start().strip_prefix("```");
        match (&mut current, fence) {
            (None, Some(label)) => current = Some((label.trim().to_lowercase(), String::new())),
            (Some(_), Some(_)) => out.extend(current.take()),
            (Some((_, code)), None) => {
                code.push_str(line);
                code.push('\n');
            }
            (None, None) => {}
        }
    }
    out
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

    fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("neet-sync-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store {
            root: dir.clone(),
            workspace: dir.join("workspace"),
        }
    }

    fn two_sum() -> Problem {
        bundled_lists()
            .unwrap()
            .into_iter()
            .find(|p| p.slug == "two-sum")
            .unwrap()
    }

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

    fn tgz(files: &[(&str, &str)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_articles_and_solutions_for_known_problems() {
        let store = temp_store("repo");
        let problems = bundled_lists().unwrap();
        let archive = tgz(&[
            // NeetCode-named article wins over the LeetCode-named one.
            (
                "leetcode-main/articles/duplicate-integer.md",
                "# Hash set\x1b[31m\n",
            ),
            (
                "leetcode-main/articles/contains-duplicate.md",
                "older copy\n",
            ),
            (
                "leetcode-main/python/0217-contains-duplicate.py",
                "class Solution: ...\n",
            ),
            ("leetcode-main/python/9999-not-in-the-list.py", "ignored\n"),
            ("leetcode-main/articles/nested/two-sum.md", "ignored\n"),
            ("leetcode-main/README.md", "ignored\n"),
        ]);
        let counts = extract_repo(&store, &problems, archive.as_slice()).unwrap();
        assert_eq!(counts, (1, 1));
        let article = store.load_article("contains-duplicate").unwrap();
        assert!(
            article.starts_with("# Hash set") && !article.contains('\x1b'),
            "{article:?}"
        );
        let python = LANGS.iter().find(|l| l.key == "python").unwrap();
        assert!(
            store
                .load_ref_solution("contains-duplicate", python)
                .is_some()
        );
        assert!(store.load_article("two-sum").is_none());
        let _ = std::fs::remove_dir_all(&store.root);
    }

    #[test]
    fn bundled_lists_are_complete_and_safe() {
        let problems = bundled_lists().unwrap();
        let count = |f: fn(&Problem) -> bool| problems.iter().filter(|p| f(p)).count();
        assert_eq!(count(|p| p.nc250), 250);
        assert_eq!(count(|p| p.nc150), 150);
        assert_eq!(count(|p| p.blind75), 75);
        let mut slugs: Vec<&str> = problems.iter().map(|p| p.slug.as_str()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), problems.len(), "duplicate slugs");
        for p in &problems {
            assert!(
                is_safe_ident(&p.slug) && is_safe_ident(&p.file_stem()),
                "{}",
                p.slug
            );
            assert!(
                !p.pattern.is_empty() && !p.difficulty.is_empty(),
                "{}",
                p.slug
            );
        }
    }

    #[test]
    fn parses_flat_js_objects() {
        let js = r#"x=[{problem:"Two Sum",link:"two-sum/",difficulty:"Easy",code:"0001-two-sum",neetcode150:!0,blind75:!1,n:12},{problem:"bad",link:foo}]"#;
        let objs = parse_js_objects(js, "{problem:");
        assert_eq!(objs.len(), 1);
        let p = &problems_from_objects(objs)[0];
        assert_eq!(p.slug, "two-sum");
        assert_eq!(p.code.as_deref(), Some("0001-two-sum"));
        assert!(p.nc150 && !p.blind75 && !p.nc250);
    }

    #[test]
    fn decodes_string_escapes() {
        let (s, _) = parse_string(br#"a\u2019b\uD83D\uDE00c\x41\"""#, 0, b'"').unwrap();
        assert_eq!(s, "a\u{2019}b\u{1F600}cA\"");
    }

    #[test]
    fn rejects_path_like_identifiers() {
        let js = r#"{problem:"Evil",link:"/../",code:"../x",neetcode150:!0}{problem:"Ok",link:"ok/",code:"../x",ncLink:"a/b",neetcode150:!0}"#;
        let problems = problems_from_objects(parse_js_objects(js, "{problem:"));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].slug, "ok");
        assert_eq!(problems[0].code, None);
        assert_eq!(problems[0].nc_slug, "ok");
    }

    #[test]
    fn splits_fenced_blocks() {
        let text = "```python3\nclass Solution:\n    pass\n```\ntext\n```Go\nfunc f() {}\n```\n";
        let blocks = fenced_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "python3");
        assert_eq!(blocks[1], ("go".to_string(), "func f() {}\n".to_string()));
    }

    #[test]
    fn merges_duplicate_entries() {
        let js = r#"{problem:"A",link:"a/",neetcode150:!0}{problem:"A",link:"a/",neetcode250:!0,code:"0001-a"}"#;
        let problems = problems_from_objects(parse_js_objects(js, "{problem:"));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].nc150 && problems[0].nc250);
        assert_eq!(problems[0].code.as_deref(), Some("0001-a"));
    }
}
