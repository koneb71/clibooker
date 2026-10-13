//! "Open in terminal": run a bookmark in the user's own terminal app.

use std::collections::BTreeMap;
use std::process::Command;

use anyhow::{Context, Result, bail};
use bk_core::{paths, render};
use tauri::State;

use crate::state::AppState;

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
pub fn open(app: &str, name: &str, command: &str) -> Result<()> {
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
    let script = if e.bookmark.is_remote() {
        crate::commands::one_liner_for(&state, &lib, e, &params)?
    } else {
        render::script(e, &params, &[], &paths::home(), &state.base_env()).map_err(|e| format!("{e:#}"))?
    };
    let app = lib.settings.terminal.as_deref().unwrap_or("terminal");
    open(app, &e.name, &script).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_applescript() {
        assert_eq!(applescript_string(r#"echo "hi" \n"#), r#""echo \"hi\" \\n""#);
    }
}
