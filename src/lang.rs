/// A language the user can solve problems in.
pub struct Lang {
    /// Name used in the config file.
    pub key: &'static str,
    /// LeetCode code snippet slug.
    pub lc_slug: &'static str,
    /// Fence labels used for this language in NeetCode articles.
    pub fences: &'static [&'static str],
    /// Directory in the neetcode-gh/leetcode repo.
    pub repo_dir: &'static str,
    pub ext: &'static str,
    pub comment: &'static str,
    /// Inserted above the starter snippet.
    pub prelude: &'static str,
    /// Definitions added when the snippet uses them, e.g. ("TreeNode", "class TreeNode: ...").
    pub node_defs: &'static [(&'static str, &'static str)],
    /// Appended below the starter snippet, a place for quick local checks.
    pub harness: &'static str,
    /// Default run command: `{file}` is the solution path, `{bin}` a private build output path.
    pub run: &'static str,
}

pub static LANGS: &[Lang] = &[
    Lang {
        key: "python",
        lc_slug: "python3",
        fences: &["python", "py"],
        repo_dir: "python",
        ext: "py",
        comment: "#",
        prelude: "from typing import *\nimport collections, heapq, math, bisect, functools, itertools\n\n\n",
        node_defs: &[
            (
                "ListNode",
                "class ListNode:\n    def __init__(self, val=0, next=None):\n        self.val = val\n        self.next = next\n\n\n",
            ),
            (
                "TreeNode",
                "class TreeNode:\n    def __init__(self, val=0, left=None, right=None):\n        self.val = val\n        self.left = left\n        self.right = right\n\n\n",
            ),
        ],
        harness: "\n\nif __name__ == \"__main__\":\n    s = Solution()\n    # print(s.method(...))\n",
        run: "python3 {file}",
    },
    Lang {
        key: "cpp",
        lc_slug: "cpp",
        fences: &["cpp", "c++"],
        repo_dir: "cpp",
        ext: "cpp",
        comment: "//",
        prelude: "#include <bits/stdc++.h>\nusing namespace std;\n\n",
        node_defs: &[
            (
                "ListNode",
                "struct ListNode {\n    int val;\n    ListNode *next;\n    ListNode() : val(0), next(nullptr) {}\n    ListNode(int x) : val(x), next(nullptr) {}\n    ListNode(int x, ListNode *next) : val(x), next(next) {}\n};\n\n",
            ),
            (
                "TreeNode",
                "struct TreeNode {\n    int val;\n    TreeNode *left;\n    TreeNode *right;\n    TreeNode() : val(0), left(nullptr), right(nullptr) {}\n    TreeNode(int x) : val(x), left(nullptr), right(nullptr) {}\n    TreeNode(int x, TreeNode *left, TreeNode *right) : val(x), left(left), right(right) {}\n};\n\n",
            ),
        ],
        harness: "\n\nint main() {\n    Solution s;\n    // cout << s.method(...) << endl;\n}\n",
        run: "g++ -std=c++20 -O2 -o {bin} {file} && {bin}",
    },
    Lang {
        key: "java",
        lc_slug: "java",
        fences: &["java"],
        repo_dir: "java",
        ext: "java",
        comment: "//",
        prelude: "import java.util.*;\n\n",
        node_defs: &[
            (
                "ListNode",
                "class ListNode {\n    int val;\n    ListNode next;\n    ListNode() {}\n    ListNode(int val) { this.val = val; }\n    ListNode(int val, ListNode next) { this.val = val; this.next = next; }\n}\n\n",
            ),
            (
                "TreeNode",
                "class TreeNode {\n    int val;\n    TreeNode left;\n    TreeNode right;\n    TreeNode() {}\n    TreeNode(int val) { this.val = val; }\n    TreeNode(int val, TreeNode left, TreeNode right) {\n        this.val = val;\n        this.left = left;\n        this.right = right;\n    }\n}\n\n",
            ),
        ],
        harness: "class Main {\n    public static void main(String[] args) {\n        Solution s = new Solution();\n        // System.out.println(s.method(...));\n    }\n}\n",
        run: "mkdir -p {bin} && javac -d {bin} {file} && java -cp {bin} Main",
    },
    Lang {
        key: "go",
        lc_slug: "golang",
        fences: &["go", "golang"],
        repo_dir: "go",
        ext: "go",
        comment: "//",
        prelude: "package main\n\nimport (\n\t\"fmt\"\n\t\"reflect\"\n\t\"sort\"\n\t\"strings\"\n)\n\nvar (\n\t_ = fmt.Println\n\t_ = reflect.DeepEqual\n\t_ = sort.Ints\n\t_ = strings.Join\n)\n\n",
        node_defs: &[
            (
                "ListNode",
                "type ListNode struct {\n	Val  int\n	Next *ListNode\n}\n\n",
            ),
            (
                "TreeNode",
                "type TreeNode struct {\n	Val   int\n	Left  *TreeNode\n	Right *TreeNode\n}\n\n",
            ),
        ],
        harness: "\n\nfunc main() {\n\t// fmt.Println(method(...))\n}\n",
        run: "go run {file}",
    },
    Lang {
        key: "rust",
        lc_slug: "rust",
        fences: &["rust", "rs"],
        repo_dir: "rust",
        ext: "rs",
        comment: "//",
        prelude: "#![allow(dead_code, unused_imports)]\nuse std::collections::*;\n\nstruct Solution;\n\n",
        node_defs: &[
            (
                "ListNode",
                "#[derive(PartialEq, Eq, Clone, Debug)]\npub struct ListNode {\n    pub val: i32,\n    pub next: Option<Box<ListNode>>,\n}\n\nimpl ListNode {\n    pub fn new(val: i32) -> Self {\n        ListNode { next: None, val }\n    }\n}\n\n",
            ),
            (
                "TreeNode",
                "#[derive(Debug, PartialEq, Eq)]\npub struct TreeNode {\n    pub val: i32,\n    pub left: Option<std::rc::Rc<std::cell::RefCell<TreeNode>>>,\n    pub right: Option<std::rc::Rc<std::cell::RefCell<TreeNode>>>,\n}\n\nimpl TreeNode {\n    pub fn new(val: i32) -> Self {\n        TreeNode { val, left: None, right: None }\n    }\n}\n\n",
            ),
        ],
        harness: "\n\nfn main() {\n    // println!(\"{:?}\", Solution::method(...));\n}\n",
        run: "rustc --edition 2021 -O -o {bin} {file} && {bin}",
    },
    Lang {
        key: "javascript",
        lc_slug: "javascript",
        fences: &["javascript", "js"],
        repo_dir: "javascript",
        ext: "js",
        comment: "//",
        prelude: "",
        node_defs: &[],
        harness: "\n\n// console.log(fn(...));\n",
        run: "node {file}",
    },
    Lang {
        key: "typescript",
        lc_slug: "typescript",
        fences: &["typescript", "ts"],
        repo_dir: "typescript",
        ext: "ts",
        comment: "//",
        prelude: "",
        node_defs: &[],
        harness: "\n\n// console.log(fn(...));\n",
        run: "tsx {file}",
    },
    Lang {
        key: "csharp",
        lc_slug: "csharp",
        fences: &["csharp", "cs", "c#"],
        repo_dir: "csharp",
        ext: "cs",
        comment: "//",
        prelude: "using System;\nusing System.Collections.Generic;\nusing System.Linq;\n\n",
        node_defs: &[],
        harness: "",
        run: "dotnet script {file}",
    },
    Lang {
        key: "kotlin",
        lc_slug: "kotlin",
        fences: &["kotlin", "kt"],
        repo_dir: "kotlin",
        ext: "kt",
        comment: "//",
        prelude: "",
        node_defs: &[],
        harness: "\n\nfun main() {\n    val s = Solution()\n    // println(s.method(...))\n}\n",
        run: "kotlinc {file} -include-runtime -d {bin}.jar && java -jar {bin}.jar",
    },
];

pub fn by_key(key: &str) -> Option<&'static Lang> {
    LANGS.iter().find(|l| l.key == key)
}
