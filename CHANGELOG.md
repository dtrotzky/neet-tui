# Changelog

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
