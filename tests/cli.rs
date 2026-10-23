//! End-to-end tests of the `neet` binary against a sandboxed config and data folder. They
//! use the bundled problem list instead of `neet sync`, so no network or Claude is needed.
//! The test-runner tests need `python3` on the PATH.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const PROBLEMS: &str = include_str!("../data/problems.json");
const MARKER: &str =
    "---- neet: scratch checks below; `neet test` replaces everything from here ----";

struct Sandbox {
    dir: PathBuf,
    /// `$EDITOR` for neet, used when the config can't be loaded.
    editor: String,
}

impl Sandbox {
    /// A fresh config + data folder with the bundled problem list and `editor = "true"`.
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let sb = Self {
            dir,
            editor: "true".into(),
        };
        std::fs::create_dir_all(sb.data()).unwrap();
        std::fs::write(sb.data().join("problems.json"), PROBLEMS).unwrap();
        sb.write_config("language = \"python\"\neditor = \"true\"\n");
        sb
    }

    fn data(&self) -> PathBuf {
        self.dir.join("data/neet")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config/neet/config.toml")
    }

    fn write_config(&self, toml: &str) {
        std::fs::create_dir_all(self.config().parent().unwrap()).unwrap();
        std::fs::write(self.config(), toml).unwrap();
    }

    fn solution(&self, stem: &str) -> PathBuf {
        self.data().join(format!("workspace/python/{stem}.py"))
    }

    fn neet(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_neet"))
            .args(args)
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("EDITOR", &self.editor)
            .env_remove("VISUAL")
            .output()
            .unwrap()
    }

    /// Runs `neet`, asserts success and returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.neet(args);
        assert!(
            out.status.success(),
            "neet {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn progress(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.data().join("progress.json")).unwrap())
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn paths_follow_xdg_dirs() {
    let sb = Sandbox::new("paths");
    let out = sb.ok(&["paths"]);
    assert!(out.contains(&sb.config().display().to_string()), "{out}");
    assert!(
        out.contains(&sb.data().join("workspace").display().to_string()),
        "{out}"
    );
}

#[test]
fn list_done_next_and_undo() {
    let sb = Sandbox::new("done");
    let list = sb.ok(&["list", "-l", "blind75"]);
    assert_eq!(
        list.matches(" Easy ").count()
            + list.matches(" Medium ").count()
            + list.matches(" Hard ").count(),
        75
    );
    assert!(sb.ok(&["next"]).starts_with("Contains Duplicate"));

    let done = sb.ok(&["done", "contains-duplicate"]);
    assert!(done.contains("solved"), "{done}");
    assert!(sb.ok(&["list"]).contains("  ✓ Easy   Contains Duplicate"));
    assert!(sb.ok(&["next"]).starts_with("Valid Anagram"));
    assert_eq!(
        sb.progress()["entries"]["contains-duplicate"]["status"],
        "Solved"
    );

    sb.ok(&["done", "contains-duplicate", "--undo"]);
    assert!(sb.ok(&["next"]).starts_with("Contains Duplicate"));
}

#[test]
fn due_reviews_are_listed_and_completed() {
    let sb = Sandbox::new("due");
    let yesterday = (chrono::Local::now().date_naive() - chrono::Days::new(1)).to_string();
    let progress = serde_json::json!({
        "entries": { "two-sum": {
            "status": "Solved", "review_stage": 1, "next_review": yesterday,
        }}
    });
    std::fs::write(sb.data().join("progress.json"), progress.to_string()).unwrap();

    assert!(sb.ok(&["due"]).contains("Two Sum"));
    assert!(
        sb.ok(&["list", "-l", "blind75"])
            .contains("⟳ Easy   Two Sum")
    );
    sb.ok(&["done", "two-sum"]);
    assert!(sb.ok(&["due"]).contains("Nothing due today"));
    let entry = &sb.progress()["entries"]["two-sum"];
    assert_eq!(entry["review_stage"], 2);
    assert!(entry["next_review"].as_str().unwrap() > yesterday.as_str());
}

#[test]
fn edit_creates_a_starter_file() {
    let sb = Sandbox::new("edit");
    sb.ok(&["edit", "two-sum"]);
    let code = std::fs::read_to_string(sb.solution("0001-two-sum")).unwrap();
    assert!(code.starts_with("# Two Sum (Easy)"), "{code}");
    assert!(code.contains(MARKER), "{code}");
    assert!(sb.progress()["entries"]["two-sum"]["last_opened"].is_string());
}

const TWO_SUM: &str = "\
class Solution:
    def twoSum(self, nums, target):
        seen = {}
        for i, n in enumerate(nums):
            if target - n in seen:
                return [seen[target - n], i]
            seen[n] = i
";

const HARNESS: &str = r#"
if __name__ == "__main__":
    cases = [("example 1", [2, 7, 11, 15], 9, [0, 1]), ("example 2", [3, 2, 4], 6, [1, 2])]
    passed = 0
    for name, nums, target, expected in cases:
        got = Solution().twoSum(nums, target)
        if sorted(got or []) == expected:
            passed += 1
            print(f"PASS {name}")
        else:
            print(f"FAIL {name}: got={got}")
    print(f"RESULT {passed}/{len(cases)}")
"#;

/// A solution plus a cached harness, so `neet test` doesn't need Claude.
fn with_harness(sb: &Sandbox, solution: &str) {
    let sol = sb.solution("0001-two-sum");
    std::fs::create_dir_all(sol.parent().unwrap()).unwrap();
    std::fs::write(&sol, solution).unwrap();
    let harness = sb.data().join("tests/python/0001-two-sum.py");
    std::fs::create_dir_all(harness.parent().unwrap()).unwrap();
    std::fs::write(harness, HARNESS).unwrap();
}

#[test]
fn test_runner_passes_a_correct_solution() {
    let sb = Sandbox::new("test-pass");
    with_harness(&sb, &format!("{TWO_SUM}\n# {MARKER}\nprint('scratch')\n"));
    let out = sb.ok(&["test", "two-sum"]);
    assert!(out.contains("PASS example 2"), "{out}");
    assert!(out.contains("all 2 passed"), "{out}");
    assert!(
        !out.contains("scratch"),
        "scratch checks should be replaced: {out}"
    );
    let run = &sb.progress()["entries"]["two-sum"]["last_test"];
    assert_eq!(
        (run["passed"].as_u64(), run["total"].as_u64()),
        (Some(2), Some(2))
    );
}

#[test]
fn test_runner_fails_a_wrong_solution() {
    let sb = Sandbox::new("test-fail");
    with_harness(
        &sb,
        "class Solution:\n    def twoSum(self, nums, target):\n        return [0, 0]\n",
    );
    let out = sb.neet(&["test", "two-sum"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("FAIL example 1"), "{stdout}");
    assert!(stdout.contains("0/2 passed"), "{stdout}");
}

#[test]
fn notes_export_collects_notes() {
    let sb = Sandbox::new("notes");
    let notes = sb.data().join("workspace/notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("0001-two-sum.md"),
        "# Two Sum\n\nHash map of seen values, O(n).\n",
    )
    .unwrap();
    let out = sb.ok(&["notes", "--export"]);
    assert!(out.contains("Arrays & Hashing"), "{out}");
    assert!(out.contains("Hash map of seen values"), "{out}");
}

#[test]
fn broken_config_is_reported_but_can_still_be_fixed() {
    let mut sb = Sandbox::new("config");
    sb.write_config("languag = \"python\"\n");
    let out = sb.neet(&["list"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("languag"), "{}", stderr(&out));

    // `neet config` falls back to $EDITOR when the config is broken; this one fixes the typo.
    let fixer = sb.dir.join("fix-config.sh");
    std::fs::write(
        &fixer,
        "#!/bin/sh\nprintf 'language = \"python\"\\n' > \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fixer, PermissionsExt::from_mode(0o755)).unwrap();
    sb.editor = fixer.display().to_string();
    sb.ok(&["config"]);
    sb.ok(&["list"]);
}

#[test]
fn corrupt_progress_is_an_error_and_left_untouched() {
    let sb = Sandbox::new("corrupt");
    let path = sb.data().join("progress.json");
    std::fs::write(&path, "{ not json").unwrap();
    let out = sb.neet(&["done", "two-sum"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("corrupt"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
}

/// Seeds a synced statement and Python starter snippet, like `neet sync` would.
fn seed(sb: &Sandbox, slug: &str, snippet: &str, statement: &str) {
    let dir = sb.data().join("problems").join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let details = serde_json::json!({
        "id": null, "description": statement, "hints": [], "topics": [], "examples": [],
        "snippets": { "python3": snippet }, "premium": false,
    });
    std::fs::write(dir.join("details.json"), details.to_string()).unwrap();
    std::fs::write(dir.join("description.md"), statement).unwrap();
}

/// A sandbox where any Claude call fails, to prove the built-in tests don't need one.
fn without_claude(name: &str) -> Sandbox {
    let sb = Sandbox::new(name);
    sb.write_config("language = \"python\"\neditor = \"true\"\nclaude_cmd = \"false\"\n");
    sb
}

fn write_solution(sb: &Sandbox, stem: &str, code: &str) {
    let sol = sb.solution(stem);
    std::fs::create_dir_all(sol.parent().unwrap()).unwrap();
    std::fs::write(sol, format!("{code}\n# {MARKER}\nprint('scratch')\n")).unwrap();
}

const TWO_SUM_STATEMENT: &str = "# 1. Two Sum\n\nYou can return the answer in any order.\n\n\
**Example 1:**\n\n**Input:** `nums = [2,7,11,15], target = 9`\n\n**Output:** `[0,1]`\n\n\
**Example 2:**\n\n**Input:** `nums = [3,2,4], target = 6`\n\n**Output:** `[1,2]`\n\n\
**Example 3:**\n\n**Input:** `nums = [3,3], target = 6`\n\n**Output:** `[0,1]`\n";

#[test]
fn builtin_tests_run_without_claude() {
    let sb = without_claude("builtin");
    seed(
        &sb,
        "two-sum",
        "class Solution:\n    def twoSum(self, nums: list[int], target: int) -> list[int]:\n        ",
        TWO_SUM_STATEMENT,
    );
    // Returns the pair the other way round: fine, the statement allows any order.
    write_solution(
        &sb,
        "0001-two-sum",
        "class Solution:\n    def twoSum(self, nums, target):\n        seen = {}\n        for i, n in enumerate(nums):\n            if target - n in seen:\n                return [i, seen[target - n]]\n            seen[n] = i",
    );
    let out = sb.neet(&["test", "two-sum"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", stderr(&out));
    // The three statement examples, plus whatever edge cases data/tests.json ships.
    assert!(stdout.contains("PASS example 3"), "{stdout}");
    assert!(stdout.contains("passed ✓"), "{stdout}");
    assert!(stderr(&out).contains("built-in tests"), "{}", stderr(&out));

    write_solution(
        &sb,
        "0001-two-sum",
        "class Solution:\n    def twoSum(self, nums, target):\n        return [0, 0]",
    );
    let out = sb.neet(&["test", "two-sum"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains(
            "FAIL example 2: input={\"nums\": [3, 2, 4], \"target\": 6} expected=[1, 2] got=[0, 0]"
        ),
        "{stdout}"
    );
}

#[test]
fn builtin_tests_handle_trees_design_in_place_and_nested_order() {
    let sb = without_claude("builtin-kinds");
    let tree = "class TreeNode:\n    def __init__(self, val=0, left=None, right=None):\n        self.val = val\n        self.left = left\n        self.right = right\n\n";
    seed(
        &sb,
        "invert-binary-tree",
        "# class TreeNode:\n#     def __init__(self, val=0, left=None, right=None):\nclass Solution:\n    def invertTree(self, root: TreeNode | None) -> TreeNode | None:\n        ",
        "**Input:** root = [4,2,7,1,3,6,9]\n\n**Output:** [4,7,2,9,6,3,1]\n\n**Input:** root = []\n\n**Output:** []\n",
    );
    write_solution(
        &sb,
        "0226-invert-binary-tree",
        &format!(
            "{tree}class Solution:\n    def invertTree(self, root):\n        if root:\n            root.left, root.right = self.invertTree(root.right), self.invertTree(root.left)\n        return root"
        ),
    );
    seed(
        &sb,
        "lru-cache",
        "class LRUCache:\n\n    def __init__(self, capacity: int):\n        \n\n    def get(self, key: int) -> int:\n        ",
        "**Input**\n[\"LRUCache\", \"put\", \"put\", \"get\", \"put\", \"get\"]\n[[2], [1, 1], [2, 2], [1], [3, 3], [2]]\n\n**Output**\n[null, null, null, 1, null, -1]\n",
    );
    write_solution(
        &sb,
        "0146-lru-cache",
        "from collections import OrderedDict\nclass LRUCache:\n    def __init__(self, capacity):\n        self.cap, self.d = capacity, OrderedDict()\n    def get(self, key):\n        if key not in self.d:\n            return -1\n        self.d.move_to_end(key)\n        return self.d[key]\n    def put(self, key, value):\n        self.d[key] = value\n        self.d.move_to_end(key)\n        if len(self.d) > self.cap:\n            self.d.popitem(last=False)",
    );
    seed(
        &sb,
        "rotate-image",
        "class Solution:\n    def rotate(self, matrix: list[list[int]]) -> None:\n        ",
        "**Input:** matrix = [[1,2],[3,4]]\n\n**Output:** [[3,1],[4,2]]\n",
    );
    write_solution(
        &sb,
        "0048-rotate-image",
        "class Solution:\n    def rotate(self, matrix):\n        matrix[:] = [list(r) for r in zip(*matrix[::-1])]",
    );
    // group-anagrams compares nested lists in any order (data/tests.json).
    seed(
        &sb,
        "group-anagrams",
        "class Solution:\n    def groupAnagrams(self, strs: list[str]) -> list[list[str]]:\n        ",
        "You can return the answer in **any order**.\n\n**Input:** strs = [\"eat\",\"tea\",\"bat\"]\n\n**Output:** [[\"bat\"],[\"eat\",\"tea\"]]\n",
    );
    write_solution(
        &sb,
        "0049-group-anagrams",
        "class Solution:\n    def groupAnagrams(self, strs):\n        g = {}\n        for s in reversed(strs):\n            g.setdefault(''.join(sorted(s)), []).append(s)\n        return list(g.values())",
    );
    for slug in [
        "invert-binary-tree",
        "lru-cache",
        "rotate-image",
        "group-anagrams",
    ] {
        let out = sb.neet(&["test", slug]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{slug}: {stdout}{}", stderr(&out));
        assert!(stdout.contains("passed ✓"), "{slug}: {stdout}");
    }
}
