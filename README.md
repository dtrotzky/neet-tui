# neet

Practise the NeetCode 150 / 250 (and Blind 75) entirely in the terminal: a ratatui TUI,
your own Neovim for coding, fully offline after one sync, and Claude as an interview coach
that nudges, reviews, writes tests, runs mock interviews and tracks your weak spots, without
spoiling the answer.

## Install

```sh
cargo install --path .
neet sync     # one-time download (~20 s), afterwards everything works offline
neet          # start the TUI
```

Requirements: Rust, Neovim (or any `$EDITOR`), and [Claude Code](https://claude.com/claude-code)
(`claude` on your `PATH`, logged in) for the coaching features. No API key needed: neet calls
`claude -p` with all tools disabled. If `neet` isn't found after installing, add
`~/.cargo/bin` to your `PATH`.

## What gets synced

| Data | Source |
| --- | --- |
| Problem lists (Blind 75, NeetCode 150, NeetCode 250), patterns, order | neetcode.io |
| Solution write-ups (intuition, algorithm, all approaches, complexity) and code in 9+ languages | [neetcode-gh/leetcode](https://github.com/neetcode-gh/leetcode) |
| Problem statements, starter code, examples | LeetCode |
| Statements and starter code for the 7 LeetCode Premium problems (Meeting Rooms, Alien Dictionary, ...) | written once by Claude from NeetCode's write-up |

Everything lives in `~/.local/share/neet/`; `neet paths` shows the exact locations.

## Practising

**A normal session:** press `D` to do any reviews that are due, then `n` for the next unsolved
problem. `Enter` opens it in nvim (statement on the left, your file on the right), `t` tests it,
`r` gets an interviewer-style review. Stuck? `h` nudges you one step at a time.

**Tests (`t`).** The first time you test a problem, Claude writes a harness with the statement's
examples plus a few edge cases (order-insensitive where the problem allows it), cached in
`~/.local/share/neet/tests/`. Everything below the `---- neet: scratch checks below ...` line in
your file is your scratch space and is replaced by the harness when testing. `ctrl-g` (or
`neet test --regen`) writes new tests if one looks wrong.

**Spaced repetition.** A solved problem comes back for review after 1, 3, 7, 21 and 60 days,
then counts as mastered. Due problems show `⟳` and are listed under `D` / `neet due`. Solve one
again (`x`, or a passing review) to push it out; looking at the solution while it's due
restarts the schedule. `F` archives your old attempt so you can redo it from scratch.

**Mock interviews (`m`).** Timed (Easy 20, Medium 35, Hard 50 minutes by default) on a fresh
file, in interview mode, with nudges, chat and solutions locked. Press `m` when done: Claude plays the interviewer, asks about
your approach, complexity, edge cases and a follow-up one question at a time, then rates
problem solving, coding, communication and testing and gives a hiring signal (Strong Hire to
No Hire). Type `/done` to wrap up early. Results show in the Stats pane.

**Stats and weak spots (`5`, `W`).** Per pattern: solved, tried, nudges, reveals, time spent in
the editor and reviews due; the weakest patterns are marked `← focus`. `W` asks Claude for a
weak-spot analysis and a plan for the next week.

**Interview mode.** Real interviews (CoderPad, a shared doc) give you syntax highlighting and
nothing else. Turn "LSP, completion & AI in nvim" off in the settings (`,`) and Neovim opens
without LSP clients, so no completion, diagnostics, Copilot or other AI suggestions, and with
completion plugins (blink.cmp, nvim-cmp) switched off. Treesitter highlighting and your keymaps
stay. Mock interviews use interview mode automatically (also a setting). It works with any
Neovim config, nothing to change in yours; plain Vim is unaffected. Your statusline can check
`vim.g.neet_interview`.

**Notes (`N`).** A Markdown file per problem in `<workspace>/notes/` for your one-line insight.
Shown under the statement; `neet notes --export > cheatsheet.md` collects all of them, grouped by
pattern, for the night before an interview.

## TUI keys

| Key | Action |
| --- | --- |
| `j`/`k`, `g`/`G`, mouse | move through problems (wheel scrolls, click selects) |
| `J`/`K`, `ctrl-d`/`ctrl-u` | scroll the right pane |
| `1`-`5`, `tab` | Problem / Solution / Claude / Output / Stats pane |
| `enter` / `e` | open your solution in nvim, with the statement in a read-only split |
| `t` / `ctrl-g` | test against examples and edge cases / regenerate the tests |
| `T` | run your file as-is (20 s timeout) |
| `h` | **nudge**: escalates question → key insight → outline → pseudocode, never full code |
| `r` | **review** like an interviewer: verdict, bugs, complexity, tips, likely follow-up. `PASS` marks it solved |
| `a` / `c` | ask Claude a question / interactive Claude tutoring session |
| `m` | mock interview: start, finish coding, answer the interviewer |
| `n` / `R` | next unsolved problem in roadmap order / random unsolved |
| `D` | toggle the due-for-review list |
| `x` | mark solved, complete a due review, or unmark |
| `s`, then `y` | reveal the NeetCode solution (recorded as viewed, shown as `◌`) |
| `N` / `F` | edit your notes / archive your solution and start fresh |
| `W` | weak-spot analysis from Claude |
| `l`, `/`, `u` | cycle list (150 → 250 → Blind 75 → All → Due), search, hide solved |
| `o` | open the NeetCode video in your browser |
| `,` | settings: interview mode, language, editor, mock times, Claude models, ... |
| `esc` | cancel a running Claude request or run, or clear the search |
| `?`, `q` | help, quit |

The selected list and problem are remembered between sessions. Paste works in the search, ask
and answer prompts. Mouse capture means selecting text needs `shift` held in most terminals.

Claude sees the problem statement, your current code, its earlier coaching for this problem, and
NeetCode's write-up (as grounding only, so nudges stay accurate without spoilers).

## CLI

Every TUI action also works as a command. `<problem>` can be a slug, a LeetCode number,
a title, or a solution file path.

```sh
neet list -l 250 -p graphs        # list with status
neet next [-r] [-d hard] [-e]     # next (or random) unsolved problem, optionally open it
neet edit two-sum                 # open in nvim
neet test two-sum [--regen]       # test against examples + edge cases (exit code 1 on failure)
neet run 1                        # run your file as-is
neet hint two-sum                 # next nudge, streamed
neet review two-sum
neet ask two-sum "why is a hash map better than sorting here?"
neet solution two-sum             # NeetCode write-up in your language
neet done two-sum [--undo]        # mark solved / complete a due review
neet due                          # spaced-repetition reviews due today
neet mock [problem] [-d medium]   # mock interview in the terminal
neet fresh two-sum                # archive your attempt, start over
neet notes two-sum | neet notes --export
neet stats [-l 250] [--coach]     # progress, weak spots, mock history, Claude's analysis
neet config                       # open the config file in your editor
```

## Neovim integration

Because `hint`, `review`, `test` and `ask` accept the file path, you can coach yourself without
leaving the editor. Add this to your config (for LazyVim: `lua/config/keymaps.lua`):

```lua
local function neet(args)
  vim.cmd("write")
  vim.cmd("botright vsplit | terminal neet " .. args .. " " .. vim.fn.shellescape(vim.fn.expand("%:p")))
end
vim.keymap.set("n", "<leader>Nh", function() neet("hint") end, { desc = "neet: nudge" })
vim.keymap.set("n", "<leader>Nr", function() neet("review") end, { desc = "neet: review" })
vim.keymap.set("n", "<leader>Nt", function() neet("test") end, { desc = "neet: test" })
-- Label the group in which-key's popup, if you use it.
pcall(function() require("which-key").add({ { "<leader>N", group = "neet" } }) end)
```

The prefix is `<leader>N` (capital) on purpose: LazyVim maps `<leader>n` to the notification
history, and putting mappings under it would make that key wait for a possible second key. If
`<leader>N` is taken in your config, `:map <leader>N` shows it; pick any free prefix.

## Config

Press `,` in the TUI to change settings; changes are saved immediately to
`~/.config/neet/config.toml`, keeping your comments. The file can also be edited by hand
(`neet config`); it's created on first run:

```toml
language = "python"   # python, cpp, java, go, rust, javascript, typescript, csharp, kotlin
# editor = "nvim"     # defaults to $EDITOR, then nvim
# workspace = "~/neetcode"   # put your solutions and notes in a git repo if you like
split_description = true
editor_lsp = true          # false = interview mode: highlighting only, no LSP/completion/AI
mock_disables_lsp = true   # interview mode during mock interviews
run_timeout_secs = 20      # tests get twice as long
claude_cmd = "claude"
# claude_model = "opus"

# Per-task models, falling back to claude_model
[models]
# hint = "haiku"      # nudges: fast is fine
# review = "opus"     # reviews, mock interviews and tests: accuracy matters
# mock = "opus"
# tests = "opus"
# ask = "sonnet"
# chat = "opus"       # interactive tutor (c)
# coach = "opus"      # weak-spot analysis
# sync = "sonnet"     # premium statements and starter code

[mock]                     # time limits in minutes
easy = 20
medium = 35
hard = 50

[run]
# {file} is your solution, {bin} a private build path under ~/.local/share/neet/build
# python = "python3 {file}"
# cpp = "clang++ -std=c++20 -O2 -o {bin} {file} && {bin}"
```

Unknown keys are rejected, so typos in the config fail loudly. TypeScript runs with `tsx`
(`npm i -g tsx`), C# with `dotnet script`, Java compiles with `javac`.

Solution files go in `<workspace>/<language>/<NNNN-slug>.<ext>` (old attempts in
`attempts/`), with the starter code, the example inputs as comments, and a scratch `main`
block below the marker line.

**Note on tests:** test harnesses are code written by Claude and run on your machine, like your
own solution. Claude is told not to touch files, the network or other processes, and every
harness is a plain file you can read in `~/.local/share/neet/tests/` before trusting it.

## Roadmap

Planned features and open questions are in [ROADMAP.md](ROADMAP.md).
