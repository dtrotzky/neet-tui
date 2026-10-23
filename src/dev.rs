//! Maintainer commands (`neet dev ...`, hidden from `--help`): checking the built-in tests
//! against NeetCode's reference solutions, and generating the shipped edge cases.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::actions;
use crate::cases::{self, Availability, Case, Override, Spec};
use crate::claude::{self, Kind};
use crate::config::Config;
use crate::lang::{self, Lang};
use crate::proc::Cancel;
use crate::store::{Problem, Store};
use crate::testing;

const TIMEOUT: Duration = Duration::from_secs(30);

fn python() -> &'static Lang {
    lang::by_key("python").expect("python is a language")
}

/// A test file running `spec` against the reference solution (which, unlike starter code,
/// doesn't define ListNode/TreeNode itself).
fn reference_test_file(store: &Store, p: &Problem, spec: &Spec) -> Option<std::path::PathBuf> {
    let lang = python();
    let reference = store.load_ref_solution(&p.slug, lang)?;
    let mut code = String::from(lang.prelude);
    for (_, def) in lang.node_defs {
        code.push_str(def);
    }
    code.push_str(testing::solution_part(&reference));
    code.push_str(&cases::python_harness(spec));
    let build = store.build_dir().join("dev");
    std::fs::create_dir_all(&build).ok()?;
    let file = build.join(format!("{}.py", p.file_stem()));
    std::fs::write(&file, code).ok()?;
    Some(file)
}

/// Runs every problem's built-in tests (or only `only`) against the reference solution and
/// prints a report; fails if any supported problem fails.
pub fn check_tests(store: &Store, problems: &[Problem], only: &[String]) -> Result<()> {
    let cfg = Config {
        language: "python".into(),
        ..Config::default()
    };
    let (mut pass, mut fail, mut unsupported, mut no_ref) = (0, Vec::new(), Vec::new(), 0);
    let mut broken_ref = Vec::new();
    for p in problems
        .iter()
        .filter(|p| only.is_empty() || only.contains(&p.slug))
    {
        let spec = match cases::spec(store, p) {
            Availability::Ready(spec) => spec,
            Availability::Unavailable(why) => {
                unsupported.push(format!("{}: {why}", p.slug));
                continue;
            }
        };
        let Some(file) = reference_test_file(store, p, &spec) else {
            no_ref += 1;
            continue;
        };
        let out = actions::run_captured(&cfg, store, &file, TIMEOUT, &Cancel::default())?;
        let result = testing::parse_result(&out);
        match result {
            Some((passed, total)) if passed == total && total > 0 => pass += 1,
            // NeetCode's file itself doesn't compile; nothing to check it against.
            None | Some(_)
                if ["SyntaxError", "IndentationError", "no class Solution"]
                    .iter()
                    .any(|e| out.contains(e)) =>
            {
                broken_ref.push(p.slug.clone());
            }
            _ => {
                let why: Vec<&str> = out
                    .lines()
                    .filter(|l| l.starts_with("FAIL") || l.contains("Error"))
                    .take(3)
                    .collect();
                fail.push(format!("{}:\n    {}", p.slug, why.join("\n    ")));
            }
        }
    }
    println!(
        "{pass} pass, {} fail, {} unsupported, {no_ref} without a reference solution, \
         {} with a broken reference",
        fail.len(),
        unsupported.len(),
        broken_ref.len()
    );
    if !broken_ref.is_empty() {
        println!("broken reference: {}", broken_ref.join(", "));
    }
    for f in &fail {
        println!("FAIL {f}");
    }
    for u in &unsupported {
        println!("unsupported {u}");
    }
    if !fail.is_empty() {
        bail!(
            "{} problems fail against the reference solution",
            fail.len()
        );
    }
    Ok(())
}

/// Edge cases per problem, at most.
const MAX_EDGE_CASES: usize = 6;
/// Larger cases (input plus expected output, as JSON) are dropped: unreadable when they fail.
const MAX_CASE_BYTES: usize = 4096;

/// Asks Claude for edge-case inputs for each problem that has built-in tests and a working
/// reference solution, computes the expected outputs with the reference solution, and writes
/// them into `file` (`data/tests.json`). Problems that already have edge cases are skipped
/// unless `force`.
/// Rewrites `file` in the canonical layout, dropping oversized cases.
pub fn tidy_tests(file: &Path) -> Result<()> {
    let mut shipped: BTreeMap<String, Override> =
        serde_json::from_str(&std::fs::read_to_string(file)?)?;
    let mut dropped = 0;
    for o in shipped.values_mut() {
        let before = o.cases.len();
        o.cases
            .retain(|c| c.input.to_string().len() + c.expected.to_string().len() <= MAX_CASE_BYTES);
        dropped += before - o.cases.len();
    }
    std::fs::write(file, shipped_json(&shipped)?)?;
    println!(
        "dropped {dropped} oversized cases, wrote {}",
        file.display()
    );
    Ok(())
}

pub fn gen_tests(
    store: &Store,
    problems: &[Problem],
    only: &[String],
    model: Option<String>,
    force: bool,
    jobs: usize,
    file: &Path,
) -> Result<()> {
    let mut shipped: BTreeMap<String, Override> = serde_json::from_str(
        &std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?,
    )
    .with_context(|| format!("parsing {}", file.display()))?;
    let mut cfg = Config {
        language: "python".into(),
        ..Config::default()
    };
    cfg.models.tests = model;

    let todo: Vec<&Problem> = problems
        .iter()
        .filter(|p| only.is_empty() || only.contains(&p.slug))
        .filter(|p| force || shipped.get(&p.slug).is_none_or(|o| o.cases.is_empty()))
        .collect();
    let total = todo.len();
    let queue = Mutex::new(todo);
    let results: Mutex<HashMap<String, Vec<Case>>> = Mutex::default();
    let done = Mutex::new(0usize);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let Some(p) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let outcome = edge_cases(store, &cfg, p);
                    let n = {
                        let mut d = done.lock().unwrap();
                        *d += 1;
                        *d
                    };
                    match outcome {
                        Ok(cases) => {
                            println!("[{n}/{total}] {}: {} edge cases", p.slug, cases.len());
                            results.lock().unwrap().insert(p.slug.clone(), cases);
                        }
                        Err(e) => println!("[{n}/{total}] {}: skipped ({e:#})", p.slug),
                    }
                }
            });
        }
    });

    for (slug, cases) in results.into_inner().unwrap() {
        if !cases.is_empty() {
            shipped.entry(slug).or_default().cases = cases;
        }
    }
    std::fs::write(file, shipped_json(&shipped)?)?;
    println!("wrote {}", file.display());
    Ok(())
}

/// `data/tests.json` with one line per problem setting and per case, for readable diffs.
fn shipped_json(shipped: &BTreeMap<String, Override>) -> Result<String> {
    let mut out = String::from("{\n");
    for (i, (slug, o)) in shipped.iter().enumerate() {
        let mut fields = Vec::new();
        let settings = Override {
            cases: Vec::new(),
            ..o.clone()
        };
        if let Value::Object(map) = serde_json::to_value(&settings)? {
            for (k, v) in map {
                fields.push(format!("    {}: {}", serde_json::to_string(&k)?, v));
            }
        }
        if !o.cases.is_empty() {
            let cases: Vec<String> = o
                .cases
                .iter()
                .map(|c| serde_json::to_string(c).map(|c| format!("      {c}")))
                .collect::<serde_json::Result<_>>()?;
            fields.push(format!("    \"cases\": [\n{}\n    ]", cases.join(",\n")));
        }
        let comma = if i + 1 < shipped.len() { "," } else { "" };
        out.push_str(&format!(
            "  {}: {{\n{}\n  }}{comma}\n",
            serde_json::to_string(slug)?,
            fields.join(",\n")
        ));
    }
    out.push_str("}\n");
    Ok(out)
}

/// Generated, reference-checked edge cases for one problem.
fn edge_cases(store: &Store, cfg: &Config, p: &Problem) -> Result<Vec<Case>> {
    let Availability::Ready(mut spec) = cases::spec(store, p) else {
        bail!("no built-in tests");
    };
    spec.cases.retain(|c| c.name.starts_with("example"));
    let examples = spec.cases.clone();
    if store.load_ref_solution(&p.slug, python()).is_none() {
        bail!("no reference solution");
    }
    if !passes(store, p, &spec)? {
        bail!("the reference solution fails the examples");
    }

    let reply = claude::run(
        cfg,
        Kind::Tests,
        &edge_prompt(store, p, &spec),
        &Cancel::default(),
        |_| {},
    )?;
    let proposed = parse_proposals(&reply, &spec)?;

    // Expected outputs come from the reference solution, never from Claude.
    let mut record = spec.clone();
    record.record = true;
    record.cases = proposed
        .iter()
        .map(|(name, input)| Case {
            name: name.clone(),
            input: input.clone(),
            expected: Value::Null,
        })
        .collect();
    let file = reference_test_file(store, p, &record).context("writing the test file")?;
    let out = actions::run_captured(cfg, store, &file, TIMEOUT, &Cancel::default())?;
    let mut outputs: HashMap<String, Value> = HashMap::new();
    for line in out.lines().filter_map(|l| l.strip_prefix("RECORD ")) {
        // A line cut off by the output cap belongs to an oversized case anyway.
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(name), Some(output)) = (v["name"].as_str(), v.get("output")) {
            outputs.insert(name.to_string(), output.clone());
        }
    }
    let mut cases: Vec<Case> = record
        .cases
        .into_iter()
        .filter_map(|c| {
            let expected = outputs.remove(&c.name)?;
            // A missing answer usually means the input broke a constraint.
            let case = Case { expected, ..c };
            let size = case.input.to_string().len() + case.expected.to_string().len();
            (!case.expected.is_null() && size <= MAX_CASE_BYTES).then_some(case)
        })
        .collect();
    cases.truncate(MAX_EDGE_CASES);

    // Everything together must pass against the reference, as users will run it.
    spec.cases = examples.into_iter().chain(cases.iter().cloned()).collect();
    if !passes(store, p, &spec)? {
        bail!("the reference solution doesn't pass its own recorded outputs");
    }
    Ok(cases)
}

fn passes(store: &Store, p: &Problem, spec: &Spec) -> Result<bool> {
    let cfg = Config::default();
    let file = reference_test_file(store, p, spec).context("writing the test file")?;
    let out = actions::run_captured(&cfg, store, &file, TIMEOUT, &Cancel::default())?;
    Ok(testing::all_passed(testing::parse_result(&out)))
}

fn edge_prompt(store: &Store, p: &Problem, spec: &Spec) -> String {
    let statement = store.load_description(&p.slug).unwrap_or_default();
    let shape = match &spec.class {
        Some(class) => format!(
            "This is a design problem: each input is {{\"ops\": [\"{class}\", ...], \"args\": [[constructor args], ...]}}, \
             one args list per op, starting with the constructor."
        ),
        None => format!(
            "Each input is an object with exactly these keys: {}.",
            spec.params
                .iter()
                .map(|p| format!("{} ({})", p.name, p.ty))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    format!(
        "<statement>\n{statement}\n</statement>\n\n<existing_examples>\n{}\n</existing_examples>\n\n\
Write 3 to 5 additional test inputs for this problem that cover edge cases the examples miss \
(smallest sizes, duplicates, negatives, all-equal values, already sorted or reversed input, \
boundary values). {shape} Use LeetCode's JSON formats (linked lists and trees as arrays, trees \
in level order with null).\n\n\
Rules:\n\
- Every input must satisfy all constraints and guarantees in the statement exactly (sizes, value \
  ranges, \"exactly one solution\", \"the answer is unique\", sorted input, ...). An invalid input \
  is worse than none.\n\
- Keep inputs small: at most 30 elements, so a failing case is easy to read.\n\
- Inputs only, no expected outputs (they are computed separately).\n\
- Name each case \"edge: <what it covers>\" in a few words.\n\n\
Reply with only a JSON array in a ```json block: [{{\"name\": \"edge: ...\", \"input\": {{...}}}}]",
        serde_json::to_string(&spec.cases.iter().map(|c| &c.input).collect::<Vec<_>>())
            .unwrap_or_default()
    )
}

/// The proposed (name, input) pairs that have the right shape for this problem.
fn parse_proposals(reply: &str, spec: &Spec) -> Result<Vec<(String, Value)>> {
    let json = testing::extract_code(reply).unwrap_or_else(|| reply.to_string());
    let list: Vec<Value> =
        serde_json::from_str(json.trim()).context("Claude's reply isn't a JSON array")?;
    let mut out = Vec::new();
    for item in list {
        let (Some(name), Some(input)) = (item["name"].as_str(), item.get("input")) else {
            continue;
        };
        let name = if name.starts_with("edge") {
            name.to_string()
        } else {
            format!("edge: {name}")
        };
        let ok = match &spec.class {
            Some(_) => {
                let (ops, args) = (input["ops"].as_array(), input["args"].as_array());
                matches!((ops, args), (Some(o), Some(a)) if !o.is_empty() && o.len() == a.len())
            }
            None => input.as_object().is_some_and(|o| {
                o.len() == spec.params.len() && spec.params.iter().all(|p| o.contains_key(&p.name))
            }),
        };
        if ok && !out.iter().any(|(n, _)| *n == name) {
            out.push((name, input.clone()));
        }
    }
    if out.is_empty() {
        bail!("no usable inputs in Claude's reply");
    }
    Ok(out)
}
