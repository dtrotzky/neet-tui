//! Config and data locations. Follows the XDG base directory spec on every platform, macOS
//! included, so the paths match the docs and `XDG_*` overrides work everywhere (the `dirs`
//! crate would use `~/Library/Application Support` on macOS).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    resolve(
        std::env::var_os("XDG_CONFIG_HOME"),
        dirs::home_dir().as_deref(),
        ".config",
    )
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    resolve(
        std::env::var_os("XDG_DATA_HOME"),
        dirs::home_dir().as_deref(),
        ".local/share",
    )
}

/// The spec says relative (or empty) values are invalid and must be ignored.
fn resolve(var: Option<OsString>, home: Option<&Path>, fallback: &str) -> Option<PathBuf> {
    var.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(fallback)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_absolute_var_else_home_fallback() {
        let home = Path::new("/home/u");
        assert_eq!(
            resolve(Some("/x/data".into()), Some(home), ".local/share"),
            Some(PathBuf::from("/x/data"))
        );
        for ignored in [None, Some("".into()), Some("rel/data".into())] {
            assert_eq!(
                resolve(ignored, Some(home), ".local/share"),
                Some(PathBuf::from("/home/u/.local/share"))
            );
        }
        assert_eq!(resolve(None, None, ".config"), None);
    }
}
