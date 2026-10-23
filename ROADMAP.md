# Roadmap

Ideas and open questions for neet, collected while building and using it. Nothing here
is implemented yet.

## Features

- **NeetCode account login and live progress sync.** Sign in to a real neetcode.io account and
  keep progress in sync both ways, so problems solved in neet show up on the website and vice
  versa. Open questions:
  - NeetCode has no public API. The site uses Firebase Authentication (Google/GitHub sign-in)
    and Firebase functions, so this means either an official API/agreement with NeetCode or
    reverse-engineering their private endpoints, which may break at any time and may conflict
    with their terms of service. Check the terms and ask NeetCode first.
  - Auth flow in a terminal: a browser-based OAuth/device flow, with the refresh token stored in
    the OS keyring (e.g. the `keyring` crate), never in `config.toml` or the data folder.
  - Sync model: neet must stay fully usable offline, so changes are queued locally and pushed
    when online. Needs a merge rule for conflicts (e.g. solved wins, newest timestamp for
    everything else) and a mapping between neet's local state (nudges, reviews, mocks, which
    the site doesn't have) and the site's solved flags.
  - "Live" tracking: poll or subscribe for changes while the TUI is open, and show sync status
    in the header.

- **Reset progress locally (also once login exists).** Makes sense, and is useful even today
  (there is no reset yet; the only way is deleting `progress.json`). Typical reasons: starting a
  fresh round of the 150 before an interview loop, or redoing one pattern from scratch.
  - Scope: everything, one list, one pattern or one problem. What gets reset should be explicit:
    solved status and review schedule, nudge counts and reveals, coaching history, mock results,
    test results. Solutions are archived to `attempts/` (like `F`), never deleted.
  - Always back up first (e.g. `progress.json.<date>.bak`) and confirm with a summary of what
    will change.
  - With login, a local reset must not silently wipe the NeetCode account. Keep "solved on the
    account" separate from "solved in this practice round": a reset starts a new local round
    while the account keeps its history, and only an explicit second option (with its own
    confirmation) also resets the account, if NeetCode's API even allows that. The sync must
    not push a local reset back as "unsolved".

## To investigate: moving the solutions folder

What happens today when the solutions folder (`workspace`) changes, from the default
(`~/.local/share/neet/workspace`) to a custom path, and again later:

- Nothing is moved. Existing solutions, `attempts/` and `notes/` stay in the old folder, and
  the new folder starts empty.
- Progress, nudge/review history, mock results, test harnesses and spaced-repetition schedules
  live in the data folder (`~/.local/share/neet`), not the workspace, so they carry over. A
  problem therefore still shows as solved, but its code and notes are "missing", and opening it
  creates a fresh starter file in the new folder.
- Changing the path back makes the old files visible again; after several changes, work ends
  up scattered over all the folders used.
- The config file itself is always `$XDG_CONFIG_HOME/neet/config.toml` (default
  `~/.config/neet/config.toml`); there is no option to move it, and the data folder follows
  `$XDG_DATA_HOME`.

To decide and build:

- When the path changes (settings popup or a hand edit of `config.toml`), offer to move or copy
  the existing workspace to the new location, with a clear preview, and refuse to overwrite
  files that already exist there (or merge per file with a prompt).
- Remember previous workspace locations, so neet can find files after a hand edit, offer to
  merge them, and warn when solved problems have no file in the current folder.
- Edge cases: the new folder is inside the old one (or the reverse), on another filesystem
  (rename fails, needs copy and delete), is an existing git repo, or isn't writable; and
  switching while a run, test or mock is in progress.
- Whether to support a custom config file location (a `--config` flag or `NEET_CONFIG`
  environment variable) and a custom data folder, and how those interact with the above.

## Platforms and integrations

- **macOS hands-on test.** CI builds and tests on macOS, `o` uses `open`, and config and data
  use the same XDG paths as on Linux (not `~/Library/Application Support`). Still needs a manual
  pass on a Mac: the TUI, Neovim and interview mode, runs and tests, and the Claude features.
- **Windows.** Out of scope for now: killing runs relies on Unix process groups and signals, so
  it would need a different process model.
- **Homebrew tap.** An extra install route for macOS users (`dist` can generate the formula).
- **Git integration.** Optionally auto-commit solutions and notes to the workspace repo, so the
  history doubles as a progress log.
