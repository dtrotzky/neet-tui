use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, TableLike, value};

use crate::claude::Kind;
use crate::lang::{self, Lang};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Language used for starter code, solution filtering and running.
    pub language: String,
    /// Editor command. Defaults to $EDITOR, falling back to nvim.
    pub editor: Option<String>,
    /// Where your own solutions live. Defaults to <data dir>/workspace;
    /// `~/` and relative paths are resolved against the home directory.
    pub workspace: Option<PathBuf>,
    /// Open the problem statement in a read-only split next to your code (vim/nvim only).
    pub split_description: bool,
    /// LSP, completion, AI suggestions and diagnostics in Neovim. Off = "interview mode":
    /// syntax highlighting only, like most real interview editors.
    pub editor_lsp: bool,
    /// Always use interview mode during mock interviews, whatever `editor_lsp` says.
    pub mock_disables_lsp: bool,
    /// Open the Claude chat (`c`) in Neovim next to the solution instead of full screen.
    pub chat_in_editor: bool,
    /// Kill a run (`T`) after this many seconds; tests get twice as long (compiling).
    pub run_timeout_secs: u64,
    /// Mock interview time limits.
    pub mock: MockConfig,
    /// The Claude Code binary.
    pub claude_cmd: String,
    /// Optional model alias passed as `--model` (e.g. "opus", "sonnet").
    pub claude_model: Option<String>,
    /// Per-task model overrides, falling back to `claude_model`.
    pub models: Models,
    /// Per-language run command overrides: `{file}` is the solution path,
    /// `{bin}` a private path for build output.
    pub run: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MockConfig {
    /// Minutes per difficulty.
    pub easy: u64,
    pub medium: u64,
    pub hard: u64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            easy: 20,
            medium: 35,
            hard: 50,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Models {
    pub hint: Option<String>,
    pub review: Option<String>,
    pub ask: Option<String>,
    pub chat: Option<String>,
    pub tests: Option<String>,
    pub mock: Option<String>,
    pub coach: Option<String>,
    pub sync: Option<String>,
}

impl Models {
    pub fn get_mut(&mut self, kind: Kind) -> &mut Option<String> {
        match kind {
            Kind::Hint => &mut self.hint,
            Kind::Review => &mut self.review,
            Kind::Ask => &mut self.ask,
            Kind::Chat => &mut self.chat,
            Kind::Tests => &mut self.tests,
            Kind::Mock => &mut self.mock,
            Kind::Coach => &mut self.coach,
            Kind::Sync => &mut self.sync,
        }
    }

    pub fn get(&self, kind: Kind) -> Option<&str> {
        match kind {
            Kind::Hint => &self.hint,
            Kind::Review => &self.review,
            Kind::Ask => &self.ask,
            Kind::Chat => &self.chat,
            Kind::Tests => &self.tests,
            Kind::Mock => &self.mock,
            Kind::Coach => &self.coach,
            Kind::Sync => &self.sync,
        }
        .as_deref()
    }

    fn entries(&self) -> [(&'static str, Option<&str>); 8] {
        [
            ("hint", self.hint.as_deref()),
            ("review", self.review.as_deref()),
            ("ask", self.ask.as_deref()),
            ("chat", self.chat.as_deref()),
            ("tests", self.tests.as_deref()),
            ("mock", self.mock.as_deref()),
            ("coach", self.coach.as_deref()),
            ("sync", self.sync.as_deref()),
        ]
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "python".into(),
            editor: None,
            workspace: None,
            split_description: true,
            editor_lsp: true,
            mock_disables_lsp: true,
            chat_in_editor: false,
            run_timeout_secs: 20,
            mock: MockConfig::default(),
            claude_cmd: "claude".into(),
            claude_model: None,
            models: Models::default(),
            run: HashMap::new(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        crate::xdg::config_home()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("neet")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, DEFAULT_CONFIG)?;
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()
            .with_context(|| format!("in {}", path.display()))?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if lang::by_key(&self.language).is_none() {
            bail!(
                "unknown language '{}' (supported: {})",
                self.language,
                lang::LANGS
                    .iter()
                    .map(|l| l.key)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !(1..=3600).contains(&self.run_timeout_secs) {
            bail!("run_timeout_secs must be between 1 and 3600");
        }
        let m = &self.mock;
        if [m.easy, m.medium, m.hard]
            .iter()
            .any(|v| !(1..=240).contains(v))
        {
            bail!("mock time limits must be between 1 and 240 minutes");
        }
        if self.claude_cmd.trim().is_empty() {
            bail!("claude_cmd can't be empty");
        }
        Ok(())
    }

    /// Writes the settings back, keeping the file's comments and layout
    /// (and leaving `[run]` alone).
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        let path = Self::path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
            Err(e) => return Err(e.into()),
        };
        let mut doc: DocumentMut = raw
            .parse()
            .with_context(|| format!("parsing {}", path.display()))?;
        self.write_into(&mut doc);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        std::fs::write(&tmp, doc.to_string())?;
        std::fs::rename(tmp, &path)?;
        Ok(())
    }

    fn write_into(&self, doc: &mut DocumentMut) {
        let root = doc.as_table_mut();
        set_value(root, "language", value(self.language.as_str()));
        set_opt(root, "editor", self.editor.as_deref());
        let workspace = self.workspace.as_ref().map(|w| w.display().to_string());
        set_opt(root, "workspace", workspace.as_deref());
        set_value(root, "split_description", value(self.split_description));
        set_value(root, "editor_lsp", value(self.editor_lsp));
        set_value(root, "mock_disables_lsp", value(self.mock_disables_lsp));
        set_value(root, "chat_in_editor", value(self.chat_in_editor));
        set_value(
            root,
            "run_timeout_secs",
            value(clamp_i64(self.run_timeout_secs)),
        );
        set_value(root, "claude_cmd", value(self.claude_cmd.as_str()));
        set_opt(root, "claude_model", self.claude_model.as_deref());

        let models = table_mut(root, "models");
        for (key, val) in self.models.entries() {
            set_opt(models, key, val);
        }
        let mock = table_mut(root, "mock");
        set_value(mock, "easy", value(clamp_i64(self.mock.easy)));
        set_value(mock, "medium", value(clamp_i64(self.mock.medium)));
        set_value(mock, "hard", value(clamp_i64(self.mock.hard)));
    }

    pub fn lang(&self) -> &'static Lang {
        lang::by_key(&self.language).unwrap_or(&lang::LANGS[0])
    }

    pub fn editor(&self) -> String {
        self.editor
            .clone()
            .or_else(|| std::env::var("EDITOR").ok().filter(|e| !e.is_empty()))
            .unwrap_or_else(|| "nvim".into())
    }

    /// Whether Neovim gets its LSP/completion setup, given whether a mock is running.
    pub fn lsp_enabled(&self, in_mock: bool) -> bool {
        self.editor_lsp && !(in_mock && self.mock_disables_lsp)
    }

    pub fn run_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.run_timeout_secs)
    }

    /// Model for a task: the `[models]` entry, else `claude_model`.
    pub fn model_for(&self, kind: Kind) -> Option<&str> {
        self.models.get(kind).or(self.claude_model.as_deref())
    }

    pub fn run_cmd(&self) -> String {
        let lang = self.lang();
        self.run
            .get(lang.key)
            .cloned()
            .unwrap_or_else(|| lang.run.to_string())
    }
}

fn clamp_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Updates a value in place so the key's comments and a trailing comment survive
/// (`insert` would replace the key and drop them).
fn set_value(table: &mut dyn TableLike, key: &str, new: Item) {
    match table.get_mut(key) {
        Some(item) if item.is_value() => {
            let decor = item.as_value().map(|v| v.decor().clone());
            *item = new;
            if let (Some(decor), Some(v)) = (decor, item.as_value_mut()) {
                *v.decor_mut() = decor;
            }
        }
        _ => {
            table.insert(key, new);
        }
    }
}

fn set_opt(table: &mut dyn TableLike, key: &str, val: Option<&str>) {
    match val {
        Some(v) => set_value(table, key, value(v)),
        None => {
            table.remove(key);
        }
    }
}

/// The table at `key`, created if missing (inline tables are kept inline).
fn table_mut<'a>(root: &'a mut toml_edit::Table, key: &str) -> &'a mut dyn TableLike {
    if !root.get(key).is_some_and(Item::is_table_like) {
        root.insert(key, Item::Table(toml_edit::Table::new()));
    }
    root.get_mut(key)
        .and_then(Item::as_table_like_mut)
        .expect("just inserted")
}

const DEFAULT_CONFIG: &str = r#"# neet configuration (also editable in the TUI with `,`)

# One of: python, cpp, java, go, rust, javascript, typescript, csharp, kotlin
language = "python"

# Editor to open solutions with (defaults to $EDITOR, then nvim)
# editor = "nvim"

# Where your solutions are stored (defaults to ~/.local/share/neet/workspace)
# workspace = "/home/me/neetcode"

# Show the problem statement in a read-only split next to your code (vim/nvim)
split_description = true

# Neovim LSP, completion, AI suggestions and diagnostics. false = interview mode:
# syntax highlighting only, like CoderPad or a Google Doc.
editor_lsp = true
# Always use interview mode during mock interviews
mock_disables_lsp = true

# Open the Claude chat (c) in Neovim, next to your solution, instead of full screen
chat_in_editor = false

# Seconds before a run is killed (tests get twice as long)
run_timeout_secs = 20

# Claude Code binary and optional model alias
claude_cmd = "claude"
# claude_model = "opus"

# Per-task model overrides (fall back to claude_model)
[models]
# hint = "haiku"     # nudges: fast is fine
# review = "opus"    # reviews and mock interviews: use the strongest
# mock = "opus"
# tests = "opus"     # writes test cases, accuracy matters
# ask = "sonnet"
# chat = "opus"      # interactive tutor (c)
# coach = "opus"     # weak-spot analysis
# sync = "sonnet"    # premium statements and starter code

# Mock interview time limits in minutes
[mock]
easy = 20
medium = 35
hard = 50

# Override how solutions are run; {file} is the solution path,
# {bin} a private path for compiled output
[run]
# python = "python3 {file}"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_to_defaults() {
        let cfg: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_keeps_comments_and_run_table() {
        let original = "# my comment\nlanguage = \"python\"  # trailing\n\n[run]\npython = \"python3 -u {file}\"\n";
        let mut doc: DocumentMut = original.parse().unwrap();
        let mut cfg: Config = toml::from_str(original).unwrap();
        cfg.language = "cpp".into();
        cfg.editor_lsp = false;
        cfg.models.hint = Some("haiku".into());
        cfg.write_into(&mut doc);
        let out = doc.to_string();
        assert!(out.contains("# my comment"));
        assert!(out.contains("language = \"cpp\"  # trailing"), "{out}");
        assert!(out.contains("python = \"python3 -u {file}\""));
        let back: Config = toml::from_str(&out).unwrap();
        assert_eq!(back.language, "cpp");
        assert!(!back.editor_lsp);
        assert_eq!(back.models.hint.as_deref(), Some("haiku"));
        assert_eq!(
            back.run.get("python").map(String::as_str),
            Some("python3 -u {file}")
        );

        // Clearing an option removes the key again.
        cfg.models.hint = None;
        cfg.write_into(&mut doc);
        let back: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(back.models.hint, None);
    }

    #[test]
    fn mock_overrides_lsp() {
        let mut cfg = Config::default();
        assert!(cfg.lsp_enabled(false));
        assert!(!cfg.lsp_enabled(true));
        cfg.mock_disables_lsp = false;
        assert!(cfg.lsp_enabled(true));
        cfg.editor_lsp = false;
        assert!(!cfg.lsp_enabled(false));
    }
}
