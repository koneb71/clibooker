//! "Open in terminal": run a bookmark in the user's own terminal app.
//!
//! macOS: Terminal, iTerm, Ghostty or Warp. Linux: `$TERMINAL`, `xdg-terminal-exec` or the
//! first common terminal found. Windows: Windows Terminal or a PowerShell window.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use bk_core::render::{self, Dialect};
use bk_core::{env::EnvMap, paths};
use tauri::State;

use crate::state::AppState;

/// The pop-out terminal used when the setting is empty.
pub fn default_terminal() -> &'static str {
    if cfg!(target_os = "macos") {
        "terminal"
    } else if cfg!(windows) {
        "windows-terminal"
    } else {
        "auto"
    }
}

fn spawn_detached(program: &str, args: &[String]) -> Result<()> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    Ok(())
}

// ---------- macOS ----------

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn osascript(script: &str) -> Result<()> {
    let out = Command::new("/usr/bin/osascript").arg("-e").arg(script).output().context("running osascript")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Type `command` into a new window of `app` (terminal | iterm | ghostty | warp).
fn open_macos(app: &str, name: &str, command: &str) -> Result<()> {
    match app {
        "iterm" => osascript(&format!(
            "tell application \"iTerm\"\n activate\n set w to (create window with default profile)\n tell current session of w to write text {}\nend tell",
            applescript_string(command)
        )),
        "ghostty" => {
            // Keep an interactive shell open after the command finishes.
            let shell_cmd = format!("{command}; exec \"$SHELL\" -l");
            let status = Command::new("/usr/bin/open")
                .args(["-na", "Ghostty", "--args", "-e", "/bin/zsh", "-lic", &shell_cmd])
                .status()?;
            if !status.success() {
                bail!("could not open Ghostty");
            }
            Ok(())
        }
        "warp" => {
            let dir = paths::home().join(".warp/launch_configurations");
            std::fs::create_dir_all(&dir)?;
            let yaml = format!(
                "---\nname: Clibooker\nwindows:\n  - tabs:\n      - title: {}\n        layout:\n          commands:\n            - exec: {}\n",
                serde_json::to_string(name)?,
                serde_json::to_string(command)?
            );
            std::fs::write(dir.join("clibooker.yaml"), yaml)?;
            let status = Command::new("/usr/bin/open").arg("warp://launch/clibooker.yaml").status()?;
            if !status.success() {
                bail!("could not open Warp");
            }
            Ok(())
        }
        _ => osascript(&format!(
            "tell application \"Terminal\"\n activate\n do script {}\nend tell",
            applescript_string(command)
        )),
    }
}

// ---------- Linux ----------

/// Terminals tried by "auto", in order, with how each takes a command and a title.
const LINUX_TERMINALS: &[&str] =
    &["gnome-terminal", "konsole", "xfce4-terminal", "kitty", "alacritty", "wezterm", "ghostty", "xterm"];

/// argv that opens `term` running `cmd` (already a full argv).
fn linux_argv(term: &str, title: &str, cmd: &[String]) -> Vec<String> {
    let mut a: Vec<String> = match term {
        "gnome-terminal" => vec![format!("--title={title}"), "--".into()],
        "konsole" => vec!["-p".into(), format!("tabtitle={title}"), "-e".into()],
        "xfce4-terminal" => vec!["-T".into(), title.into(), "-x".into()],
        "kitty" => vec!["--title".into(), title.into()],
        "alacritty" => vec!["--title".into(), title.into(), "-e".into()],
        "wezterm" => vec!["start".into(), "--".into()],
        "xdg-terminal-exec" => vec![],
        "xterm" => vec!["-T".into(), title.into(), "-e".into()],
        _ => vec!["-e".into()],
    };
    a.extend(cmd.iter().cloned());
    a
}

fn open_linux(app: &str, name: &str, script: &str, env: &EnvMap) -> Result<()> {
    let user_shell = bk_core::env::get_string(env, "SHELL").unwrap_or_else(|| "/bin/bash".into());
    // An interactive login shell that runs the script, then stays open.
    let cmd = vec![user_shell, "-lic".into(), format!("{script}; exec \"${{SHELL:-/bin/bash}}\" -l")];
    let found = |t: &str| bk_core::env::which_in(env, t).map(|p| p.display().to_string());
    let (term, program) = match app {
        "auto" | "" => {
            let from_env = bk_core::env::get_string(env, "TERMINAL").and_then(|t| found(&t).map(|p| (t, p)));
            from_env
                .or_else(|| found("xdg-terminal-exec").map(|p| ("xdg-terminal-exec".to_string(), p)))
                .or_else(|| LINUX_TERMINALS.iter().find_map(|t| found(t).map(|p| (t.to_string(), p))))
                .or_else(|| found("x-terminal-emulator").map(|p| ("x-terminal-emulator".to_string(), p)))
                .context("no terminal app found (set $TERMINAL, or pick one in Settings)")?
        }
        other => (other.to_string(), found(other).with_context(|| format!("`{other}` is not installed"))?),
    };
    spawn_detached(&program, &linux_argv(&term, name, &cmd))
}

// ---------- Windows ----------

fn open_windows(app: &str, name: &str, script: &str, env: &EnvMap) -> Result<()> {
    let shell = bk_core::env::which_in(env, "pwsh")
        .or_else(|| bk_core::env::which_in(env, "powershell"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "powershell.exe".into());
    let encoded = bk_core::shell::pwsh_encode_interactive(script);
    let ps_args: Vec<String> =
        ["-NoExit", "-NoLogo", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded].map(String::from).to_vec();
    let wt = bk_core::env::which_in(env, "wt");
    match (app, wt) {
        ("powershell", _) | (_, None) => new_console(&shell, &ps_args),
        (_, Some(wt)) => {
            // `;` separates Windows Terminal commands, so keep it out of the title.
            let mut args = vec!["new-tab".to_string(), "--title".into(), name.replace(';', ","), "--".into(), shell];
            args.extend(ps_args);
            spawn_detached(&wt.display().to_string(), &args)
        }
    }
}

/// Start a console program in a new window of its own.
fn new_console(program: &str, args: &[String]) -> Result<()> {
    let mut c = Command::new(program);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        c.creation_flags(CREATE_NEW_CONSOLE);
    }
    c.spawn().with_context(|| format!("starting {program}"))?;
    Ok(())
}

/// Open a new terminal window running `script` (in this OS's shell language).
pub fn open(app: &str, name: &str, script: &str, env: &EnvMap) -> Result<()> {
    if cfg!(target_os = "macos") {
        open_macos(app, name, script)
    } else if cfg!(windows) {
        open_windows(app, name, script, env)
    } else {
        open_linux(app, name, script, env)
    }
}

#[tauri::command]
pub fn open_in_terminal(
    state: State<'_, AppState>,
    id: String,
    params: BTreeMap<String, String>,
) -> Result<(), String> {
    let lib = state.library();
    let e = lib.by_id(&id).ok_or("bookmark not found")?;
    if !bk_core::trust::is_trusted(&state.paths, &e.file) {
        return Err(format!("{} is not trusted yet", bk_core::paths::contract_tilde(&e.file)));
    }
    let base = state.base_env();
    let script = if e.bookmark.is_remote() {
        crate::commands::one_liner_for(&state, &lib, e, &params)?
    } else {
        render::script_for(Dialect::native(), e, &params, &[], &paths::home(), &base).map_err(|e| format!("{e:#}"))?
    };
    let app = lib.settings.terminal.as_deref().unwrap_or(default_terminal());
    open(app, &e.name, &script, &base).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_applescript() {
        assert_eq!(applescript_string(r#"echo "hi" \n"#), r#""echo \"hi\" \\n""#);
    }

    #[test]
    fn linux_terminal_argv() {
        let cmd = vec!["/bin/bash".to_string(), "-lic".into(), "make; exec bash -l".into()];
        assert_eq!(linux_argv("gnome-terminal", "t", &cmd)[..2], ["--title=t", "--"]);
        assert_eq!(linux_argv("kitty", "t", &cmd)[2..], cmd[..]);
        assert_eq!(linux_argv("x-terminal-emulator", "t", &cmd)[0], "-e");
    }
}
