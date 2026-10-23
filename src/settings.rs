//! The settings editable from the TUI (`,`), as pure operations on `Config`.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::claude::Kind;
use crate::config::Config;
use crate::lang::LANGS;

/// Model aliases offered when cycling; "" means "default" (or "inherit").
const MODEL_CHOICES: &[&str] = &["", "opus", "sonnet", "haiku"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Language,
    Editor,
    Split,
    Lsp,
    MockLsp,
    Workspace,
    RunTimeout,
    MockEasy,
    MockMedium,
    MockHard,
    ChatInEditor,
    ClaudeModel,
    Model(Kind),
    ClaudeCmd,
}

pub const SECTIONS: &[(&str, &[Setting])] = &[
    (
        "Editor",
        &[
            Setting::Language,
            Setting::Lsp,
            Setting::MockLsp,
            Setting::Split,
            Setting::Editor,
            Setting::Workspace,
        ],
    ),
    (
        "Practice",
        &[
            Setting::RunTimeout,
            Setting::MockEasy,
            Setting::MockMedium,
            Setting::MockHard,
        ],
    ),
    (
        "Claude",
        &[
            Setting::ChatInEditor,
            Setting::ClaudeModel,
            Setting::Model(Kind::Hint),
            Setting::Model(Kind::Review),
            Setting::Model(Kind::Mock),
            Setting::Model(Kind::Tests),
            Setting::Model(Kind::Ask),
            Setting::Model(Kind::Chat),
            Setting::Model(Kind::Coach),
            Setting::Model(Kind::Sync),
            Setting::ClaudeCmd,
        ],
    ),
];

/// All settings in display order.
pub fn all() -> Vec<Setting> {
    SECTIONS
        .iter()
        .flat_map(|(_, s)| s.iter().copied())
        .collect()
}

/// How a setting is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Toggle,
    /// Cycles through choices; a custom value can also be typed.
    Cycle,
    Text,
    Number,
}

impl Setting {
    pub fn label(self) -> String {
        match self {
            Setting::Language => "Language".into(),
            Setting::Editor => "Editor command".into(),
            Setting::Split => "Statement split in editor".into(),
            Setting::Lsp => "LSP, completion & AI in nvim".into(),
            Setting::MockLsp => "Interview mode during mocks".into(),
            Setting::Workspace => "Solutions folder".into(),
            Setting::RunTimeout => "Run timeout (seconds)".into(),
            Setting::MockEasy => "Mock time: Easy (min)".into(),
            Setting::MockMedium => "Mock time: Medium (min)".into(),
            Setting::MockHard => "Mock time: Hard (min)".into(),
            Setting::ChatInEditor => "Chat next to your code".into(),
            Setting::ClaudeModel => "Default model".into(),
            Setting::Model(kind) => format!("Model for {}", kind.label()),
            Setting::ClaudeCmd => "Claude command".into(),
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Setting::Language => "Language for starter code, tests, runs and solutions.",
            Setting::Editor => "Command to open files with. Empty = $EDITOR, then nvim.",
            Setting::Split => "Show the problem statement read-only next to your code (vim/nvim).",
            Setting::Lsp => {
                "Off = interview mode: Neovim keeps syntax highlighting, but no LSP, completion, \
                 diagnostics or Copilot, like most real interview editors."
            }
            Setting::MockLsp => "Always use interview mode while a mock interview is running.",
            Setting::Workspace => {
                "Where your solutions and notes live, e.g. ~/neetcode (a git repo works well). \
                 Empty = ~/.local/share/neet/workspace. Existing files are not moved."
            }
            Setting::RunTimeout => "Kill a run after this long. Tests get twice as long.",
            Setting::MockEasy | Setting::MockMedium | Setting::MockHard => {
                "Time limit for a mock interview at this difficulty."
            }
            Setting::ChatInEditor => {
                "On = the chat (c) opens in Neovim, split next to your solution. Off = full \
                 screen. Either way, c continues the problem's last chat and C starts a new one."
            }
            Setting::ClaudeModel => {
                "Model alias passed to claude --model. Default = Claude Code's own default."
            }
            Setting::Model(_) => {
                "Overrides the default model for this task. Inherit = default model."
            }
            Setting::ClaudeCmd => "The Claude Code binary to run.",
        }
    }

    pub fn control(self) -> Control {
        match self {
            Setting::Split | Setting::Lsp | Setting::MockLsp | Setting::ChatInEditor => {
                Control::Toggle
            }
            Setting::Language | Setting::ClaudeModel | Setting::Model(_) => Control::Cycle,
            Setting::Editor | Setting::Workspace | Setting::ClaudeCmd => Control::Text,
            Setting::RunTimeout | Setting::MockEasy | Setting::MockMedium | Setting::MockHard => {
                Control::Number
            }
        }
    }

    pub fn display(self, cfg: &Config) -> String {
        let on_off = |b: bool| if b { "on" } else { "off" }.to_string();
        match self {
            Setting::Language => cfg.language.clone(),
            Setting::Editor => cfg
                .editor
                .clone()
                .unwrap_or_else(|| format!("default ({})", cfg.editor())),
            Setting::Split => on_off(cfg.split_description),
            Setting::Lsp => {
                if cfg.editor_lsp {
                    "on".into()
                } else {
                    "off (interview mode)".into()
                }
            }
            Setting::MockLsp => on_off(cfg.mock_disables_lsp),
            Setting::ChatInEditor => on_off(cfg.chat_in_editor),
            Setting::Workspace => cfg
                .workspace
                .as_ref()
                .map(|w| w.display().to_string())
                .unwrap_or_else(|| "default".into()),
            Setting::RunTimeout => cfg.run_timeout_secs.to_string(),
            Setting::MockEasy => cfg.mock.easy.to_string(),
            Setting::MockMedium => cfg.mock.medium.to_string(),
            Setting::MockHard => cfg.mock.hard.to_string(),
            Setting::ClaudeModel => cfg.claude_model.clone().unwrap_or_else(|| "default".into()),
            Setting::Model(kind) => cfg
                .models
                .get(kind)
                .map(str::to_string)
                .unwrap_or_else(|| "inherit".into()),
            Setting::ClaudeCmd => cfg.claude_cmd.clone(),
        }
    }

    /// The raw value for the text input.
    pub fn raw(self, cfg: &Config) -> String {
        match self {
            Setting::Editor => cfg.editor.clone().unwrap_or_default(),
            Setting::Workspace => cfg
                .workspace
                .as_ref()
                .map(|w| w.display().to_string())
                .unwrap_or_default(),
            Setting::ClaudeModel => cfg.claude_model.clone().unwrap_or_default(),
            Setting::Model(kind) => cfg.models.get(kind).unwrap_or_default().to_string(),
            other => other.display(cfg),
        }
    }

    /// Toggles a switch or steps through choices.
    pub fn step(self, cfg: &mut Config, forward: bool) {
        match self {
            Setting::Split => cfg.split_description = !cfg.split_description,
            Setting::Lsp => cfg.editor_lsp = !cfg.editor_lsp,
            Setting::MockLsp => cfg.mock_disables_lsp = !cfg.mock_disables_lsp,
            Setting::ChatInEditor => cfg.chat_in_editor = !cfg.chat_in_editor,
            Setting::Language => {
                let keys: Vec<&str> = LANGS.iter().map(|l| l.key).collect();
                cfg.language = cycle(&keys, &cfg.language, forward).to_string();
            }
            Setting::ClaudeModel => {
                let current = cfg.claude_model.clone().unwrap_or_default();
                cfg.claude_model = non_empty(cycle(MODEL_CHOICES, &current, forward));
            }
            Setting::Model(kind) => {
                let current = cfg.models.get(kind).unwrap_or_default().to_string();
                *cfg.models.get_mut(kind) = non_empty(cycle(MODEL_CHOICES, &current, forward));
            }
            Setting::RunTimeout => step_number(&mut cfg.run_timeout_secs, 5, forward),
            Setting::MockEasy => step_number(&mut cfg.mock.easy, 5, forward),
            Setting::MockMedium => step_number(&mut cfg.mock.medium, 5, forward),
            Setting::MockHard => step_number(&mut cfg.mock.hard, 5, forward),
            Setting::Editor | Setting::Workspace | Setting::ClaudeCmd => {}
        }
    }

    /// Sets a value typed by the user. The caller validates the whole config.
    pub fn set(self, cfg: &mut Config, input: &str) -> Result<()> {
        let input = input.trim();
        let number = || -> Result<u64> {
            input
                .parse()
                .map_err(|_| anyhow::anyhow!("'{input}' is not a whole number"))
        };
        match self {
            Setting::Editor => cfg.editor = non_empty(input),
            Setting::Workspace => cfg.workspace = non_empty(input).map(PathBuf::from),
            Setting::ClaudeCmd => {
                if input.is_empty() {
                    bail!("the Claude command can't be empty");
                }
                cfg.claude_cmd = input.to_string();
            }
            Setting::ClaudeModel => cfg.claude_model = non_empty(input),
            Setting::Model(kind) => *cfg.models.get_mut(kind) = non_empty(input),
            Setting::Language => {
                if !LANGS.iter().any(|l| l.key == input) {
                    bail!("unknown language '{input}'");
                }
                cfg.language = input.to_string();
            }
            Setting::RunTimeout => cfg.run_timeout_secs = number()?,
            Setting::MockEasy => cfg.mock.easy = number()?,
            Setting::MockMedium => cfg.mock.medium = number()?,
            Setting::MockHard => cfg.mock.hard = number()?,
            Setting::Split | Setting::Lsp | Setting::MockLsp | Setting::ChatInEditor => match input
            {
                "on" | "true" | "yes" => self.set_bool(cfg, true),
                "off" | "false" | "no" => self.set_bool(cfg, false),
                _ => bail!("expected on or off"),
            },
        }
        Ok(())
    }

    fn set_bool(self, cfg: &mut Config, v: bool) {
        match self {
            Setting::Split => cfg.split_description = v,
            Setting::Lsp => cfg.editor_lsp = v,
            Setting::MockLsp => cfg.mock_disables_lsp = v,
            Setting::ChatInEditor => cfg.chat_in_editor = v,
            _ => {}
        }
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// The next (or previous) choice; an unknown current value starts at the beginning.
fn cycle<'a>(choices: &[&'a str], current: &str, forward: bool) -> &'a str {
    let n = choices.len();
    match choices.iter().position(|c| *c == current) {
        Some(i) if forward => choices[(i + 1) % n],
        Some(i) => choices[(i + n - 1) % n],
        None => choices[0],
    }
}

/// Steps by `by`, staying at least `by` (validation enforces the upper bounds).
fn step_number(v: &mut u64, by: u64, forward: bool) {
    *v = if forward {
        v.saturating_add(by)
    } else {
        v.saturating_sub(by).max(by)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_and_cycles() {
        let mut cfg = Config::default();
        Setting::Lsp.step(&mut cfg, true);
        assert!(!cfg.editor_lsp);
        assert_eq!(Setting::Lsp.display(&cfg), "off (interview mode)");

        Setting::Model(Kind::Hint).step(&mut cfg, true);
        assert_eq!(cfg.models.hint.as_deref(), Some("opus"));
        Setting::Model(Kind::Hint).step(&mut cfg, false);
        assert_eq!(cfg.models.hint, None);
        Setting::Model(Kind::Hint).step(&mut cfg, false);
        assert_eq!(cfg.models.hint.as_deref(), Some("haiku"));

        Setting::Language.step(&mut cfg, true);
        assert_eq!(cfg.language, "cpp");
    }

    #[test]
    fn typed_values() {
        let mut cfg = Config::default();
        Setting::Workspace.set(&mut cfg, " ~/neetcode ").unwrap();
        assert_eq!(cfg.workspace, Some(PathBuf::from("~/neetcode")));
        Setting::Workspace.set(&mut cfg, "").unwrap();
        assert_eq!(cfg.workspace, None);
        Setting::MockHard.set(&mut cfg, "45").unwrap();
        assert_eq!(cfg.mock.hard, 45);
        assert!(Setting::MockHard.set(&mut cfg, "soon").is_err());
        assert!(Setting::ClaudeCmd.set(&mut cfg, " ").is_err());
        Setting::Model(Kind::Review)
            .set(&mut cfg, "claude-opus-5-5")
            .unwrap();
        assert_eq!(cfg.model_for(Kind::Review), Some("claude-opus-5-5"));
    }

    #[test]
    fn numbers_stay_positive() {
        let mut cfg = Config::default();
        cfg.mock.easy = 5;
        Setting::MockEasy.step(&mut cfg, false);
        assert_eq!(cfg.mock.easy, 5);
        assert!(cfg.validate().is_ok());
    }
}
