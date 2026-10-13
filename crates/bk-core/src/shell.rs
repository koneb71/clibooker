//! The shells that run bookmark steps: finding them, their flags, and quoting for each.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::env::{self, EnvMap};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    /// sh, bash, zsh, dash, ksh, Git Bash, ...
    Posix,
    Fish,
    /// PowerShell 7 (`pwsh`) or Windows PowerShell (`powershell`).
    Pwsh,
    /// cmd.exe
    Cmd,
}

/// Lowercase program name without folder or extension; splits on `/` and `\\` on every OS.
fn program_name(path: &Path) -> String {
    let s = path.to_string_lossy();
    let file = s.rsplit(['/', '\\']).next().unwrap_or_default().to_ascii_lowercase();
    match file.rsplit_once('.') {
        Some((stem, "exe" | "cmd" | "bat" | "com")) => stem.to_string(),
        _ => file,
    }
}

impl ShellKind {
    pub fn of(path: &Path) -> ShellKind {
        let stem = program_name(path);
        match stem.as_str() {
            "pwsh" | "powershell" => ShellKind::Pwsh,
            "cmd" => ShellKind::Cmd,
            "fish" => ShellKind::Fish,
            _ => ShellKind::Posix,
        }
    }

    /// Quote one word so this shell passes it through literally.
    pub fn quote(self, s: &str) -> String {
        match self {
            ShellKind::Posix => crate::render::q(s),
            ShellKind::Fish => {
                if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
                    s.to_string()
                } else {
                    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
                }
            }
            ShellKind::Pwsh => pwsh_quote(s),
            ShellKind::Cmd => {
                if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || "&|<>^\"()%!,;=".contains(c)) {
                    s.to_string()
                } else {
                    format!("\"{}\"", s.replace('"', "\"\""))
                }
            }
        }
    }
}

/// A PowerShell argument: bare when it's plainly safe (flags like `--port` included), else a
/// single-quoted literal.
pub fn pwsh_quote(s: &str) -> String {
    let safe =
        !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./\\:=".contains(c)) && !s.starts_with('.');
    if safe { s.to_string() } else { pwsh_string(s) }
}

/// A PowerShell single-quoted string literal (always quoted, for values in expressions).
pub fn pwsh_string(s: &str) -> String {
    // Typographic quotes also end a PowerShell string, so double them too.
    let mut out = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Make PowerShell exit with the step's real status: `-Command` alone collapses every failure
/// to 1, and native programs report through `$LASTEXITCODE`. On its own line so a trailing
/// `# comment` in the step can't swallow it.
const PWSH_TRAILER: &str = "\n$bk_ok = $?; if ($bk_ok) { exit 0 }; if ($LASTEXITCODE) { exit $LASTEXITCODE }; exit 1";

/// `-EncodedCommand` payload: base64 of the UTF-16LE script, so no quoting can go wrong.
pub fn pwsh_encode(command: &str) -> String {
    let script = format!("{command}{PWSH_TRAILER}");
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `-EncodedCommand` payload for an interactive window (`-NoExit`): the script as is.
pub fn pwsh_encode_interactive(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The argument that carries `command` for this shell (see [`ShellKind::flags`]).
pub fn command_arg(kind: ShellKind, command: &str) -> OsString {
    match kind {
        ShellKind::Pwsh => pwsh_encode(command).into(),
        _ => command.into(),
    }
}

/// A process that runs `command` in `cwd` with exactly `env` (nothing inherited).
pub fn step_process(
    shell: &Path,
    shell_args: &[String],
    kind: ShellKind,
    command: &str,
    cwd: &Path,
    env: &EnvMap,
) -> std::process::Command {
    let mut c = std::process::Command::new(shell);
    c.args(shell_args);
    #[cfg(windows)]
    if kind == ShellKind::Cmd {
        // cmd.exe doesn't follow the usual argv quoting rules; `/s /c "..."` takes the line verbatim.
        use std::os::windows::process::CommandExt;
        c.raw_arg(format!("\"{command}\""));
    } else {
        c.arg(command_arg(kind, command));
    }
    #[cfg(not(windows))]
    c.arg(command_arg(kind, command));
    c.current_dir(cwd).env_clear().envs(env);
    c
}

/// Flags that make the shell skip rc files and read the next argument as the command.
pub fn flags(path: &Path) -> Vec<String> {
    let stem = program_name(path);
    let f: &[&str] = match (ShellKind::of(path), stem.as_str()) {
        (ShellKind::Pwsh, _) => &["-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand"],
        (ShellKind::Cmd, _) => &["/d", "/s", "/c"],
        (ShellKind::Fish, _) => &["--no-config", "-c"],
        (_, "zsh") => &["-f", "-c"],
        (_, "bash") => &["--noprofile", "--norc", "-c"],
        _ => &["-c"],
    };
    f.iter().map(|s| s.to_string()).collect()
}

/// The step shell when neither the bookmark nor settings pick one.
/// Windows: `pwsh`, else Windows PowerShell. Elsewhere: `$SHELL` if it is zsh or bash, else `sh`.
pub fn default_shell(env: &EnvMap) -> String {
    if cfg!(windows) {
        return if env::which_in(env, "pwsh").is_some() { "pwsh" } else { "powershell" }.into();
    }
    let shell = env::get_string(env, "SHELL").unwrap_or_default();
    let name = Path::new(&shell).file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "zsh" | "bash" => name.to_string(),
        _ => "sh".to_string(),
    }
}

/// Resolve a shell name or path and the flags that make it skip rc files.
pub fn resolve(name: &str, env: &EnvMap) -> Result<(PathBuf, Vec<String>)> {
    let is_path = name.contains('/') || name.contains('\\') || name.starts_with('~');
    let path = if is_path {
        paths::expand_tilde(name)
    } else {
        find(name, env).with_context(|| format!("shell `{name}` not found"))?
    };
    if !path.exists() {
        bail!("shell {} not found", path.display());
    }
    let args = flags(&path);
    Ok((path, args))
}

fn find(name: &str, env: &EnvMap) -> Option<PathBuf> {
    if cfg!(windows) {
        return find_windows(name, env);
    }
    env::which_in(env, name).or_else(|| {
        ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
            .iter()
            .map(|d| Path::new(d).join(name))
            .find(|p| p.exists())
    })
}

fn find_windows(name: &str, env: &EnvMap) -> Option<PathBuf> {
    let system_root = env::get_string(env, "SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let system32 = Path::new(&system_root).join("System32");
    match name.to_ascii_lowercase().trim_end_matches(".exe") {
        "bash" | "git-bash" | "gitbash" => git_bash(env),
        "cmd" => env::get_string(env, "ComSpec").map(PathBuf::from).or_else(|| Some(system32.join("cmd.exe"))),
        "powershell" => env::which_in(env, "powershell")
            .or_else(|| Some(system32.join("WindowsPowerShell").join("v1.0").join("powershell.exe"))),
        "pwsh" => env::which_in(env, "pwsh").or_else(|| {
            env::get_string(env, "ProgramFiles")
                .map(|p| Path::new(&p).join("PowerShell").join("7").join("pwsh.exe"))
                .filter(|p| p.exists())
        }),
        _ => env::which_in(env, name),
    }
}

/// Git for Windows' bash. Never `System32\bash.exe`, which starts WSL instead.
pub fn git_bash(env: &EnvMap) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(git) = env::which_in(env, "git") {
        // ...\Git\cmd\git.exe or ...\Git\bin\git.exe
        if let Some(root) = git.parent().and_then(Path::parent) {
            candidates.push(root.join("bin").join("bash.exe"));
        }
    }
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(p) = env::get_string(env, var) {
            candidates.push(Path::new(&p).join("Git").join("bin").join("bash.exe"));
        }
    }
    if let Some(local) = env::get_string(env, "LOCALAPPDATA") {
        candidates.push(Path::new(&local).join("Programs").join("Git").join("bin").join("bash.exe"));
    }
    if let Some(found) = env::which_in(env, "bash") {
        let in_system32 = found.components().any(|c| c.as_os_str().eq_ignore_ascii_case("system32"));
        if !in_system32 && !found.to_string_lossy().to_ascii_lowercase().contains("windowsapps") {
            candidates.push(found);
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_flags() {
        assert_eq!(ShellKind::of(Path::new("/bin/zsh")), ShellKind::Posix);
        assert_eq!(ShellKind::of(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")), ShellKind::Pwsh);
        assert_eq!(ShellKind::of(Path::new("powershell.EXE")), ShellKind::Pwsh);
        assert_eq!(ShellKind::of(Path::new("cmd.exe")), ShellKind::Cmd);
        assert_eq!(flags(Path::new("/bin/zsh")), ["-f", "-c"]);
        assert_eq!(flags(Path::new("bash.exe")), ["--noprofile", "--norc", "-c"]);
        assert_eq!(flags(Path::new("pwsh")).last().unwrap(), "-EncodedCommand");
    }

    #[test]
    fn quoting() {
        assert_eq!(ShellKind::Pwsh.quote("it's"), "'it''s'");
        assert_eq!(ShellKind::Pwsh.quote("plain"), "plain");
        assert_eq!(ShellKind::Pwsh.quote("$HOME"), "'$HOME'");
        assert_eq!(ShellKind::Pwsh.quote("--port"), "--port");
        assert_eq!(ShellKind::Cmd.quote("a b"), "\"a b\"");
        assert_eq!(ShellKind::Cmd.quote("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(ShellKind::Fish.quote("it's"), "'it\\'s'");
        assert_eq!(ShellKind::Posix.quote("a b"), "'a b'");
    }

    #[test]
    fn encoded_command_is_utf16le_with_trailer() {
        let enc = pwsh_encode("echo hé");
        let bytes = base64::engine::general_purpose::STANDARD.decode(enc).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.starts_with("echo hé\n"));
        assert!(script.ends_with("exit 1"));
    }
}
