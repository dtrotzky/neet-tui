# Changelog

## 0.2.0

- Built-in tests for 233 of the 250 problems in Python, without Claude: the statement's examples
  plus shipped edge cases with outputs from NeetCode's reference solutions. Other languages and
  the remaining problems still get a Claude-written harness, now built from the same cases.
- The chat (`c`) continues each problem's last conversation, so you can quit, work on your code
  and come back to the same chat; `C` starts a new one. Also as `neet chat <problem> [--new]`.
- New setting "Chat next to your code" (`chat_in_editor`): the chat opens in Neovim, split next
  to your solution.

## 0.1.1

- The repository is now `dtrotzky/neet-tui` (was `neet-cli`). The installer and download links
  use the new name; the command is still `neet`, and config, data and progress are unchanged.
- Sync identifies itself with the new repository URL.

## 0.1.0

First release.

- TUI and CLI for practising the NeetCode 150, NeetCode 250 and Blind 75 offline, after a
  one-time `neet sync` (lists from neetcode.io with a bundled fallback, statements from
  LeetCode, write-ups and solutions from neetcode-gh/leetcode).
- Coding in your own Neovim, with the statement in a split and an interview mode without LSP,
  completion or AI suggestions.
- Claude as a coach: escalating nudges, interviewer-style reviews, generated tests, timed mock
  interviews with a debrief and hiring signal, and a weak-spot analysis.
- Spaced-repetition reviews, notes with a cheat-sheet export, per-pattern stats, settings in
  the TUI.
- Prebuilt binaries for Linux (x86_64, ARM64) and macOS (Apple Silicon, Intel).
