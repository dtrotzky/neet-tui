//! neet: practise the NeetCode 150/250 offline in the terminal, with Claude as a coach.
//!
//! - `cli`: command-line parsing and the `neet <command>` implementations
//! - `tui`: the interactive terminal UI
//! - `practice`: operations shared by both (editing, chat, solving, notes, picking problems)
//! - `judge`: running and testing solutions (built-in and Claude-written tests)
//! - `coach`: Claude Code integration and mock interviews
//! - `sync`: downloading the problem lists, statements and NeetCode solutions
//! - `model`: the data model and storage, languages, spaced repetition, stats
//! - `config`: the config file, data paths and the settings editable in the TUI
//! - `render`: Markdown and syntax highlighting for the terminal

pub mod cli;
pub mod coach;
pub mod config;
pub mod judge;
pub mod model;
pub mod practice;
pub mod render;
pub mod sync;
pub mod tui;
