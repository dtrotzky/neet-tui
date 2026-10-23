//! Operations shared by the TUI and the plain CLI: finding problems, editing, the chat,
//! solved status, notes and coaching bookkeeping.

mod chat;
mod coaching;
mod editor;
mod notes;
mod problems;
mod progress;

pub use chat::*;
pub use coaching::*;
pub use editor::*;
pub use notes::*;
pub use problems::*;
pub use progress::*;
