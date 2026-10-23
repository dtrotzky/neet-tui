//! Built-in test cases, so `neet test` works without Claude: the examples from the synced
//! statement, plus edge cases and per-problem settings shipped in `data/tests.json`. A generic
//! harness (`harness.py`) runs them; other languages get them as input for Claude's harness.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::{Problem, Store};

/// Per-problem overrides and extra cases, keyed by LeetCode slug.
const SHIPPED: &str = include_str!("../data/tests.json");
const HARNESS: &str = include_str!("harness.py");

static OVERRIDES: LazyLock<HashMap<String, Override>> =
    LazyLock::new(|| serde_json::from_str(SHIPPED).unwrap_or_default());

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Override {
    /// Why the generic harness can't check this problem (several valid answers, node
    /// references as input, an interactive API, ...); Claude writes its tests instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare: Option<Compare>,
    /// Compare this argument (by position) after the call instead of the return value,
    /// for in-place problems that don't mutate the first argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_arg: Option<usize>,
    /// Edge cases with expected outputs computed by NeetCode's reference solution.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<Case>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    Exact,
    /// The top-level list may be in any order.
    AnyOrder,
    /// Every nested list may be in any order (e.g. groups of anagrams).
    AnyOrderDeep,
    /// Each element of the top-level list may be in any order, but not the top level itself.
    AnyOrderInner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub name: String,
    /// `{param: value}` for a function, `{"ops": [...], "args": [...]}` for a design class.
    pub input: Value,
    pub expected: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// Everything the harness needs, serialized as `NEET_SPEC`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub function: Option<String>,
    pub class: Option<String>,
    pub params: Vec<Param>,
    pub returns: String,
    pub compare: Compare,
    pub result_arg: Option<usize>,
    pub cases: Vec<Case>,
    /// Print each case's output instead of checking it (used with a reference solution).
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub record: bool,
}

/// Whether a problem has built-in tests, and if not, why.
pub enum Availability {
    Ready(Spec),
    Unavailable(String),
}

/// The built-in test spec for a problem, from its synced statement and Python starter code.
pub fn spec(store: &Store, p: &Problem) -> Availability {
    let over = OVERRIDES.get(&p.slug).cloned().unwrap_or_default();
    if let Some(why) = over.unsupported {
        return Availability::Unavailable(why);
    }
    let Some(snippet) = store
        .load_details(&p.slug)
        .and_then(|d| d.snippets.get("python3").cloned())
    else {
        return Availability::Unavailable("no starter code".into());
    };
    let Some(sig) = signature(&snippet) else {
        return Availability::Unavailable("couldn't read the starter code's signature".into());
    };
    let statement = store.load_description(&p.slug).unwrap_or_default();
    let design = sig.class.is_some();
    let mut cases: Vec<Case> = examples(&statement, design)
        .into_iter()
        .enumerate()
        .map(|(i, (input, expected))| Case {
            name: format!("example {}", i + 1),
            input,
            expected,
        })
        .collect();
    cases.extend(over.cases);
    if cases.is_empty() {
        return Availability::Unavailable("no examples could be read from the statement".into());
    }
    let compare = over.compare.unwrap_or(if mentions_any_order(&statement) {
        Compare::AnyOrder
    } else {
        Compare::Exact
    });
    // In-place problems return nothing: check the (first) argument they modify.
    let result_arg = over
        .result_arg
        .or_else(|| (!design && sig.returns == "None" && !sig.params.is_empty()).then_some(0));
    Availability::Ready(Spec {
        function: sig.function,
        class: sig.class,
        params: sig.params,
        returns: sig.returns,
        compare,
        result_arg,
        cases,
        record: false,
    })
}

impl Spec {
    /// "3 examples and 5 edge cases".
    pub fn describe(&self) -> String {
        let examples = self
            .cases
            .iter()
            .filter(|c| c.name.starts_with("example"))
            .count();
        let edge = self.cases.len() - examples;
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        match edge {
            0 => plural(examples, "example"),
            _ => format!(
                "{} and {}",
                plural(examples, "example"),
                plural(edge, "edge case")
            ),
        }
    }
}

/// The Python harness for a spec, appended after the solution code.
pub fn python_harness(spec: &Spec) -> String {
    let json = serde_json::to_string(spec).expect("spec serializes");
    // A JSON string literal is also a valid Python string literal.
    let literal = serde_json::to_string(&json).expect("string serializes");
    format!("\nNEET_SPEC = {literal}\n{HARNESS}")
}

fn mentions_any_order(statement: &str) -> bool {
    let s = statement.to_lowercase();
    s.contains("any order") || s.contains("does not matter")
}

struct Signature {
    function: Option<String>,
    class: Option<String>,
    params: Vec<Param>,
    returns: String,
}

/// Reads the method under test from LeetCode's Python starter code: the first method of
/// `class Solution`, or for design problems the class itself (constructor parameters).
fn signature(snippet: &str) -> Option<Signature> {
    let code: String = snippet
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let (class_name, body) = first_class(&code)?;
    let design = class_name != "Solution";
    let def = if design { "def __init__(" } else { "def " };
    let start = body.find(def)? + "def ".len();
    let rest = &body[start..];
    let name = &rest[..rest.find('(')?];
    let close = matching_paren(rest)?;
    let params_text = &rest[rest.find('(')? + 1..close];
    let after = &rest[close + 1..];
    let returns = after
        .split_once("->")
        .filter(|(before, _)| !before.contains(':'))
        .map(|(_, r)| r.split(':').next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    let params = split_top_level(params_text)
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "self")
        .map(|p| {
            let (name, ty) = p.split_once(':').unwrap_or((p, ""));
            Param {
                name: name.split('=').next().unwrap_or(name).trim().to_string(),
                ty: ty.split('=').next().unwrap_or(ty).trim().to_string(),
            }
        })
        .collect();
    Some(Signature {
        function: (!design).then(|| name.to_string()),
        class: design.then(|| class_name.to_string()),
        params,
        returns,
    })
}

/// `class Solution` if present, else the first class; returns its name and the code after it.
fn first_class(code: &str) -> Option<(&str, &str)> {
    let at = code
        .find("class Solution")
        .or_else(|| code.find("class "))?;
    let rest = &code[at + "class ".len()..];
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some((&rest[..end], &rest[end..]))
}

/// Index of the `)` matching the first `(`.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 && c == ')' {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits on commas that aren't inside brackets or strings.
fn split_top_level(s: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut quoted, mut start) = (Vec::new(), 0i32, false, 0);
    let mut prev = '\0';
    for (i, c) in s.char_indices() {
        match c {
            '"' if prev != '\\' => quoted = !quoted,
            '(' | '[' | '{' if !quoted => depth += 1,
            ')' | ']' | '}' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        prev = c;
    }
    parts.push(&s[start..]);
    parts
}

/// Bracket depth after `s` (ignoring brackets inside strings).
fn depth(s: &str) -> i32 {
    let (mut d, mut quoted, mut prev) = (0, false, '\0');
    for c in s.chars() {
        match c {
            '"' if prev != '\\' => quoted = !quoted,
            '[' | '{' | '(' if !quoted => d += 1,
            ']' | '}' | ')' if !quoted => d -= 1,
            _ => {}
        }
        prev = c;
    }
    d
}

/// The `**Input:** ... **Output:** ...` examples of a statement, as (input, expected) pairs.
/// Examples that don't parse (custom judges like "2, nums = [1,2,_]") are skipped.
fn examples(statement: &str, design: bool) -> Vec<(Value, Value)> {
    let lines: Vec<&str> = statement.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(first) = label_rest(lines[i], "Input") else {
            i += 1;
            continue;
        };
        let (input, next) = block(&lines, i, first, design);
        i = next;
        while i < lines.len() && label_rest(lines[i], "Output").is_none() {
            if label_rest(lines[i], "Input").is_some() {
                break;
            }
            i += 1;
        }
        let Some(first) = lines.get(i).and_then(|l| label_rest(l, "Output")) else {
            continue;
        };
        let (output, next) = block(&lines, i, first, false);
        i = next;
        let parsed = if design {
            parse_design(&input)
        } else {
            parse_assignments(input.first().map(String::as_str).unwrap_or(""))
        };
        let expected = output.first().and_then(|o| parse_value(o));
        if let (Some(input), Some(expected)) = (parsed, expected) {
            out.push((input, expected));
        }
    }
    out
}

/// The text after `**Input:**` / `**Input**` (and the same for Output), if `line` is one.
fn label_rest<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix("**")?.strip_prefix(label)?;
    let rest = rest.strip_prefix(':').unwrap_or(rest).strip_prefix("**")?;
    Some(rest.strip_prefix(':').unwrap_or(rest).trim())
}

/// The value(s) of a labeled block: the rest of the label line, continued over the following
/// lines while brackets are open; for design inputs, the two lines (ops, args) that follow.
/// Returns the values and the index of the next line to read.
fn block(lines: &[&str], at: usize, first: &str, design: bool) -> (Vec<String>, usize) {
    let mut values = Vec::new();
    let mut cur = first.trim_matches('`').trim().to_string();
    let mut i = at + 1;
    let wanted = if design { 2 } else { 1 };
    loop {
        // `board =` with the value on the next lines isn't complete yet.
        if !cur.is_empty() && depth(&cur) <= 0 && !cur.ends_with('=') {
            values.push(std::mem::take(&mut cur));
            if values.len() == wanted {
                break;
            }
        }
        let Some(line) = lines.get(i) else { break };
        let line = line.trim();
        if line.starts_with("**") && cur.is_empty() {
            break;
        }
        i += 1;
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        cur.push_str(line.trim_matches('`'));
    }
    (values, i)
}

/// `nums = [2,7,11,15], target = 9` → `{"nums": [...], "target": 9}`.
fn parse_assignments(text: &str) -> Option<Value> {
    let mut map = serde_json::Map::new();
    for part in split_top_level(text) {
        let (name, value) = part.split_once('=')?;
        map.insert(name.trim().to_string(), parse_value(value)?);
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

fn parse_design(values: &[String]) -> Option<Value> {
    let [ops, args] = values else { return None };
    let (ops, args) = (parse_value(ops)?, parse_value(args)?);
    let same_len = ops.as_array()?.len() == args.as_array()?.len();
    same_len.then(|| serde_json::json!({ "ops": ops, "args": args }))
}

/// A JSON value; Python-style tuples (`[(0,30),(5,10)]`) are read as lists.
fn parse_value(text: &str) -> Option<Value> {
    let text = text.trim().trim_matches('`').trim();
    serde_json::from_str(text).ok().or_else(|| {
        let (mut quoted, mut prev) = (false, '\0');
        let listed: String = text
            .chars()
            .map(|c| {
                if c == '"' && prev != '\\' {
                    quoted = !quoted;
                }
                prev = c;
                match c {
                    '(' if !quoted => '[',
                    ')' if !quoted => ']',
                    c => c,
                }
            })
            .collect();
        serde_json::from_str(&listed).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_function_signatures() {
        let s = signature(
            "# Definition for a binary tree node.\n# class TreeNode:\n#     def __init__(self, val=0):\n\
             class Solution:\n    def invertTree(self, root: TreeNode | None) -> TreeNode | None:\n        ",
        )
        .unwrap();
        assert_eq!(s.function.as_deref(), Some("invertTree"));
        assert_eq!(
            s.params,
            [Param {
                name: "root".into(),
                ty: "TreeNode | None".into()
            }]
        );
        assert_eq!(s.returns, "TreeNode | None");

        let s = signature(
            "class Solution:\n    def rotate(self, matrix: list[list[int]]) -> None:\n        \"\"\"x\"\"\"",
        )
        .unwrap();
        assert_eq!(s.returns, "None");
        assert_eq!(s.params[0].ty, "list[list[int]]");
    }

    #[test]
    fn reads_design_signatures() {
        let s = signature(
            "class LRUCache:\n\n    def __init__(self, capacity: int):\n        \n\n    def get(self, key: int) -> int:\n",
        )
        .unwrap();
        assert_eq!(s.class.as_deref(), Some("LRUCache"));
        assert_eq!(s.function, None);
        assert_eq!(s.params[0].name, "capacity");
    }

    #[test]
    fn parses_examples() {
        let md = "**Example 1:**\n\n**Input:** `nums = [2,7,11,15], target = 9`\n\n**Output:** `[0,1]`\n\n\
                  **Explanation:** because\n\n**Example 2:**\n\n**Input:** grid = [\n  [\"1\",\"0\"],\n  [\"0\",\"1\"]\n]\n\n\
                  **Output:** 2\n\n**Input:** board =\n[[1,2]\n,[3,4]]\n**Output:** true\n\n\
                  **Input:** intervals = [(0,30),(5,10)]\n**Output:** false\n\n**Example 3:**\n\n**Input:** nums = [1,1,2]\n\n**Output:** 2, nums = [1,2,_]\n";
        assert_eq!(
            examples(md, false),
            [
                (json!({"nums": [2, 7, 11, 15], "target": 9}), json!([0, 1])),
                (json!({"grid": [["1", "0"], ["0", "1"]]}), json!(2)),
                (json!({"board": [[1, 2], [3, 4]]}), json!(true)),
                (json!({"intervals": [[0, 30], [5, 10]]}), json!(false)),
            ]
        );
    }

    #[test]
    fn parses_design_examples() {
        let md = "**Input**\n[\"LRUCache\", \"put\", \"get\"]\n[[2], [1, 1], [1]]\n\n**Output**\n[null, null, 1]\n";
        assert_eq!(
            examples(md, true),
            [(
                json!({"ops": ["LRUCache", "put", "get"], "args": [[2], [1, 1], [1]]}),
                json!([null, null, 1])
            )]
        );
    }

    #[test]
    fn shipped_overrides_parse() {
        let parsed: HashMap<String, Override> = serde_json::from_str(SHIPPED).unwrap();
        for (slug, o) in &parsed {
            assert!(crate::store::is_safe_ident(slug), "{slug}");
            assert!(o.unsupported.is_none() || o.cases.is_empty(), "{slug}");
        }
    }

    #[test]
    fn harness_embeds_the_spec_as_a_python_string() {
        let spec = Spec {
            function: Some("f".into()),
            class: None,
            params: vec![],
            returns: String::new(),
            compare: Compare::Exact,
            result_arg: None,
            cases: vec![Case {
                name: "quote \" and \\ ".into(),
                input: json!({}),
                expected: json!("'''"),
            }],
            record: false,
        };
        let h = python_harness(&spec);
        let line = h.lines().find(|l| l.starts_with("NEET_SPEC = ")).unwrap();
        let literal: String = serde_json::from_str(&line["NEET_SPEC = ".len()..]).unwrap();
        assert_eq!(serde_json::from_str::<Spec>(&literal).unwrap(), spec);
    }
}
