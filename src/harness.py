
# ---- neet: built-in tests (generated; your code is above) ----
# Runs the cases in NEET_SPEC against your Solution (or design class) and prints one
# `PASS <name>` / `FAIL <name>: ...` line per case, then `RESULT <passed>/<total>`.
# With "record": true it prints `RECORD <json>` with each case's output instead (used to
# compute expected outputs from a reference solution).


def neet_main(spec):
    import collections as neet_collections
    import json as neet_json
    import math as neet_math

    g = globals()
    # LeetCode's Python environment has these star-imported; add what's not defined already.
    for neet_mod in ("collections", "heapq", "math", "bisect", "itertools", "functools",
                     "string", "re", "random", "operator", "typing"):
        try:
            m = __import__(neet_mod)
        except ImportError:
            continue
        g.setdefault(neet_mod, m)
        for k in dir(m):
            if not k.startswith("_"):
                g.setdefault(k, getattr(m, k))

    if "ListNode" not in g:
        class ListNode:
            def __init__(self, val=0, next=None):
                self.val = val
                self.next = next
        g["ListNode"] = ListNode
    if "TreeNode" not in g:
        class TreeNode:
            def __init__(self, val=0, left=None, right=None):
                self.val = val
                self.left = left
                self.right = right
        g["TreeNode"] = TreeNode
    if "Interval" not in g:
        class Interval:
            def __init__(self, start, end):
                self.start = start
                self.end = end

            # Some solutions treat intervals as [start, end] pairs.
            def __getitem__(self, i):
                return (self.start, self.end)[i]

            def __iter__(self):
                return iter((self.start, self.end))

            def __len__(self):
                return 2
        g["Interval"] = Interval
    ListNode, TreeNode = g["ListNode"], g["TreeNode"]

    def build_list(a):
        head = cur = ListNode(0)
        for x in a or []:
            cur.next = ListNode(x)
            cur = cur.next
        return head.next

    def build_tree(a):
        if not a or a[0] is None:
            return None
        it = iter(a)
        root = TreeNode(next(it))
        q = neet_collections.deque([root])
        while q:
            node = q.popleft()
            for side in ("left", "right"):
                try:
                    v = next(it)
                except StopIteration:
                    return root
                if v is not None:
                    child = TreeNode(v)
                    setattr(node, side, child)
                    q.append(child)
        return root

    def node_kind(ty):
        ty = ty.replace(" ", "")
        for kind in ("ListNode", "TreeNode", "Interval"):
            if ty in (kind, "Optional[%s]" % kind, kind + "|None", "None|" + kind):
                return kind
            if ty.startswith(("list[", "List[")) and kind in ty:
                return "list:" + kind
        return None

    def convert_in(v, ty):
        kind = node_kind(ty)
        if kind == "ListNode":
            return build_list(v)
        if kind == "TreeNode":
            return build_tree(v)
        if kind == "list:ListNode":
            return [build_list(x) for x in v]
        if kind == "list:TreeNode":
            return [build_tree(x) for x in v]
        if kind == "list:Interval":
            return [g["Interval"](*x) for x in v]
        return v

    def convert_out(v, ty=""):
        if v is None and node_kind(ty) in ("ListNode", "TreeNode"):
            return []
        if isinstance(v, ListNode):
            out, seen = [], 0
            while v is not None and seen < 100000:
                out.append(v.val)
                v, seen = v.next, seen + 1
            return out
        if isinstance(v, TreeNode):
            out, q = [], neet_collections.deque([v])
            while q:
                n = q.popleft()
                if n is None:
                    out.append(None)
                else:
                    out.append(n.val)
                    q.append(n.left)
                    q.append(n.right)
            while out and out[-1] is None:
                out.pop()
            return out
        if isinstance(v, (list, tuple)):
            return [convert_out(x) for x in v]
        if isinstance(v, (set, frozenset)):
            return sorted((convert_out(x) for x in v), key=neet_json.dumps)
        return v

    def canon(v):
        return neet_json.dumps(v, sort_keys=True)

    def deep_sorted(v):
        if isinstance(v, list):
            return sorted((deep_sorted(x) for x in v), key=canon)
        return v

    def same(a, b):
        if isinstance(a, bool) or isinstance(b, bool):
            return a is b or a == b
        if isinstance(a, (int, float)) and isinstance(b, (int, float)):
            return neet_math.isclose(a, b, rel_tol=1e-5, abs_tol=1e-5)
        if isinstance(a, list) and isinstance(b, list):
            return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
        return a == b

    def matches(got, expected, compare):
        if compare == "any_order" and isinstance(got, list) and isinstance(expected, list):
            return same(sorted(got, key=canon), sorted(expected, key=canon))
        if compare == "any_order_deep":
            return same(deep_sorted(got), deep_sorted(expected))
        if compare == "any_order_inner" and isinstance(got, list) and isinstance(expected, list):
            return same([deep_sorted(x) for x in got], [deep_sorted(x) for x in expected])
        return same(got, expected)

    def short(v):
        s = neet_json.dumps(v)
        return s if len(s) <= 300 else s[:300] + "..."

    def run_function(case):
        params = spec["params"]
        given = case["input"]
        if all(p["name"] in given for p in params):
            raw = [given[p["name"]] for p in params]
        else:  # the statement names parameters differently; same order
            raw = list(given.values())
        args = [convert_in(neet_json.loads(neet_json.dumps(v)), p["type"])
                for v, p in zip(raw, params)]
        if "Solution" not in g:
            raise NameError("no class Solution defined")
        solution = g["Solution"]()
        method = getattr(solution, spec["function"], None)
        if method is None:  # renamed; fine if it's the only method
            public = [m for m in dir(solution)
                      if not m.startswith("_") and callable(getattr(solution, m))]
            if len(public) != 1:
                raise AttributeError("Solution has no method " + spec["function"])
            method = getattr(solution, public[0])
        got = method(*args)
        if spec.get("result_arg") is not None:
            got = args[spec["result_arg"]]
        return convert_out(got, spec.get("returns", ""))

    def run_design(case):
        ops, op_args = case["input"]["ops"], case["input"]["args"]
        obj = g[spec["class"]](*op_args[0])
        out = [None]
        for op, a in zip(ops[1:], op_args[1:]):
            out.append(convert_out(getattr(obj, op)(*a)))
        return out

    run = run_design if spec.get("class") else run_function
    passed = 0
    for i, case in enumerate(spec["cases"]):
        name = case.get("name") or "case %d" % (i + 1)
        try:
            got = run(case)
        except Exception as e:  # one failing case shouldn't stop the others
            if spec.get("record"):
                print("RECORD " + neet_json.dumps({"name": name, "error": repr(e)}))
            else:
                print("FAIL %s: input=%s raised %s: %s"
                      % (name, short(case["input"]), type(e).__name__, e))
            continue
        if spec.get("record"):
            print("RECORD " + neet_json.dumps({"name": name, "output": got}))
        elif matches(got, case["expected"], spec.get("compare", "exact")):
            passed += 1
            print("PASS " + name)
        else:
            print("FAIL %s: input=%s expected=%s got=%s"
                  % (name, short(case["input"]), short(case["expected"]), short(got)))
    print("RESULT %d/%d" % (passed, len(spec["cases"])))


if __name__ == "__main__":
    import json as _neet_json
    neet_main(_neet_json.loads(NEET_SPEC))
