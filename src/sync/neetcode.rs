//! Problem lists, parsed from neetcode.io's JavaScript bundle, with a bundled snapshot as
//! fallback.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use super::get_string;
use crate::model::store::{Problem, is_safe_ident, pattern_rank};
use crate::render::sanitize;

const PRACTICE_URL: &str = "https://neetcode.io/practice";

pub(super) fn fetch_lists_from_site() -> Result<Vec<Problem>> {
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
/// `<data>/problems.json` to `assets/problems.json` with one problem per line.
const BUNDLED_LISTS: &str = include_str!("../../assets/problems.json");

pub(super) fn bundled_lists() -> Result<Vec<Problem>> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn merges_duplicate_entries() {
        let js = r#"{problem:"A",link:"a/",neetcode150:!0}{problem:"A",link:"a/",neetcode250:!0,code:"0001-a"}"#;
        let problems = problems_from_objects(parse_js_objects(js, "{problem:"));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].nc150 && problems[0].nc250);
        assert_eq!(problems[0].code.as_deref(), Some("0001-a"));
    }
}
