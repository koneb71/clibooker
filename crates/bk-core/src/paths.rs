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
    /// Resolve from `BK_CONFIG`, `BK_DATA_DIR`, XDG variables, then the platform default:
    /// `~/.config/bk` + `~/.local/share/bk` on macOS/Linux, `%APPDATA%\bk` + `%LOCALAPPDATA%\bk` on Windows.
    pub fn from_env() -> Self {
        let config_file = non_empty_var("BK_CONFIG").map(PathBuf::from).unwrap_or_else(|| {
            non_empty_var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(default_config_dir)
                .join("bk")
                .join("bookmarks.toml")
        });
        let data_dir = non_empty_var("BK_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| {
            non_empty_var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(default_data_dir).join("bk")
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
    /// Where folders from other devices live on this one (never synced).
    pub fn folders_file(&self) -> PathBuf {
        self.data_dir.join("folders.toml")
    }
}

fn non_empty_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

fn default_config_dir() -> PathBuf {
    if cfg!(windows) {
        non_empty_var("APPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Roaming"))
    } else {
        home().join(".config")
    }
}

fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        non_empty_var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local"))
    } else {
        home().join(".local").join("share")
    }
}

pub fn home() -> PathBuf {
    env::home_dir().unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }))
}

/// Expand a leading `~` / `~/` / `~\` (the last as written on Windows, accepted on any OS).
pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return home();
    }
    match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        Some(rest) => normalize(&home().join(&*portable_str(rest))),
        None => PathBuf::from(s),
    }
}

/// A relative path written with `/` whatever the OS, so it reads the same everywhere.
pub fn portable(rel: &Path) -> String {
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    parts.join("/")
}

/// Off Windows, a relative path written on Windows (`web\app`) means `web/app`.
fn portable_str(s: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) || !s.contains('\\') || s.contains('/') || is_windows_absolute(s) {
        s.into()
    } else {
        s.replace('\\', "/").into()
    }
}

/// `C:\x`, `C:/x` or `\\server\share`: a path that is absolute on Windows.
pub fn is_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
}

/// Replace the home prefix with `~` for storage/display. Always uses `/` after `~`, so stored
/// paths read the same on every OS.
pub fn contract_tilde(p: &Path) -> String {
    let home = home();
    let mut rest = p.components();
    let mut home_parts = home.components();
    loop {
        match (home_parts.next(), rest.clone().next()) {
            (None, _) => break,
            (Some(h), Some(c)) if same_component(h.as_os_str(), c.as_os_str()) => {
                rest.next();
            }
            _ => return p.display().to_string(),
        }
    }
    let parts: Vec<String> = rest.map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if parts.is_empty() { "~".into() } else { format!("~/{}", parts.join("/")) }
}

fn same_component(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
}

/// Whether two paths name the same location, lexically: ignores trailing separators, and on
/// Windows also case and `/` vs `\`.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let (mut x, mut y) = (a.components(), b.components());
    loop {
        match (x.next(), y.next()) {
            (None, None) => return true,
            (Some(p), Some(q)) if same_component(p.as_os_str(), q.as_os_str()) => {}
            _ => return false,
        }
    }
}

/// Resolve a user-written path: expand `~`, make relative paths relative to `base`, normalize `.`/`..`.
pub fn resolve(s: &str, base: &Path) -> PathBuf {
    let p = expand_tilde(&portable_str(s));
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

/// `fs::canonicalize` without Windows' `\\?\` prefix where a plain path works.
pub fn canonical(p: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    dunce::canonicalize(p)
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
        assert_eq!(p, home().join("work").join("x"));
        assert_eq!(contract_tilde(&p), "~/work/x");
        assert_eq!(contract_tilde(&home()), "~");
        // A sibling of home that shares its prefix text is not under home.
        let sibling = PathBuf::from(format!("{}x", home().display()));
        assert_eq!(contract_tilde(&sibling), sibling.display().to_string());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_relative() {
        assert_eq!(contract_tilde(Path::new("/etc")), "/etc");
        assert_eq!(resolve("frontend/../api", Path::new("/r/p")), PathBuf::from("/r/p/api"));
        assert_eq!(resolve("/abs", Path::new("/r")), PathBuf::from("/abs"));
        // Written on Windows by older versions: `\` separators in a relative path.
        assert_eq!(resolve(r"web\app", Path::new("/r")), PathBuf::from("/r/web/app"));
        assert_eq!(expand_tilde(r"~\a\b"), home().join("a").join("b"));
        assert!(is_windows_absolute(r"D:\dev") && is_windows_absolute("d:/dev") && is_windows_absolute(r"\\srv\x"));
        assert!(!is_windows_absolute("/dev") && !is_windows_absolute("dev"));
        assert_eq!(portable(Path::new("a/b")), "a/b");
        assert!(same_path(Path::new("/a/b/"), Path::new("/a/b")));
        assert!(!same_path(Path::new("/a/B"), Path::new("/a/b")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths() {
        assert_eq!(resolve("frontend/../api", Path::new(r"C:\r\p")), PathBuf::from(r"C:\r\p\api"));
        assert_eq!(resolve(r"D:\abs", Path::new(r"C:\r")), PathBuf::from(r"D:\abs"));
        assert_eq!(expand_tilde(r"~\a\b"), home().join("a").join("b"));
        assert!(same_path(Path::new(r"c:\Users\X\"), Path::new("C:/users/x")));
        let upper = PathBuf::from(home().display().to_string().to_uppercase()).join("w");
        assert_eq!(contract_tilde(&upper), "~/w");
    }
}
