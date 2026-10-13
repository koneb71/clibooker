//! File locations and `~` handling.

use std::env;
use std::path::{Component, Path, PathBuf};

pub const PROJECT_FILE: &str = ".bk.toml";

#[derive(Debug, Clone)]
pub struct Paths {
    /// Global bookmarks file.
    pub config_file: PathBuf,
    /// Directory for history, trust, inbox, project registry.
    pub data_dir: PathBuf,
}

impl Paths {
    /// Resolve from `BK_CONFIG`, `BK_DATA_DIR`, XDG variables, then `~/.config` / `~/.local/share`.
    pub fn from_env() -> Self {
        let home = home();
        let config_file = non_empty_var("BK_CONFIG").map(PathBuf::from).unwrap_or_else(|| {
            non_empty_var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("bk")
                .join("bookmarks.toml")
        });
        let data_dir = non_empty_var("BK_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| {
            non_empty_var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local").join("share"))
                .join("bk")
        });
        Paths { config_file, data_dir }
    }

    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("history.jsonl")
    }
    pub fn recent_file(&self) -> PathBuf {
        self.data_dir.join("recent.log")
    }
    pub fn trust_file(&self) -> PathBuf {
        self.data_dir.join("trust.json")
    }
    pub fn projects_file(&self) -> PathBuf {
        self.data_dir.join("projects.json")
    }
}

fn non_empty_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

pub fn home() -> PathBuf {
    env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` / `~/`.
pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        home()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(s)
    }
}

/// Replace the home prefix with `~` for storage/display.
pub fn contract_tilde(p: &Path) -> String {
    let home = home();
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Resolve a user-written path: expand `~`, make relative paths relative to `base`, normalize `.`/`..`.
pub fn resolve(s: &str, base: &Path) -> PathBuf {
    let p = expand_tilde(s);
    let joined = if p.is_absolute() { p } else { base.join(p) };
    normalize(&joined)
}

/// Lexical normalization (no filesystem access).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Walk up from `start` looking for a `.bk.toml`.
pub fn find_project_file(start: &Path) -> Option<PathBuf> {
    start.ancestors().map(|d| d.join(PROJECT_FILE)).find(|f| f.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_round_trip() {
        let p = expand_tilde("~/work/x");
        assert!(p.is_absolute());
        assert_eq!(contract_tilde(&p), "~/work/x");
        assert_eq!(contract_tilde(Path::new("/etc")), "/etc");
    }

    #[test]
    fn resolve_relative() {
        assert_eq!(resolve("frontend/../api", Path::new("/r/p")), PathBuf::from("/r/p/api"));
        assert_eq!(resolve("/abs", Path::new("/r")), PathBuf::from("/abs"));
    }
}
