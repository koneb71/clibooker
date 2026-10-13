//! Commands that work with the interactive shell: `save`, `go`, `init`.

use std::path::PathBuf;

use anyhow::{Result, bail};
use bk_core::env::{self, EnvMap};
use bk_core::render::Dialect;
use bk_core::{Bookmark, EnvSpec, render};
use console::style;
use dialoguer::Confirm;

use crate::cli::{GoShell, InitShell, SaveArgs};
use crate::commands::{self, Ctx};

const INIT_ZSH: &str = include_str!("shell/init.zsh");
const INIT_BASH: &str = include_str!("shell/init.bash");
const INIT_POWERSHELL: &str = include_str!("shell/init.ps1");

/// The rc file and the line that loads the hook, for the shell the user is most likely in.
pub fn hook_line(env: &EnvMap) -> (&'static str, &'static str) {
    let shell = hook_shell(env).or_else(|| var_in(env, "SHELL").and_then(|s| s.rsplit('/').next().map(str::to_string)));
    const PWSH: (&str, &str) =
        ("your PowerShell profile ($PROFILE)", "bk init powershell | Out-String | Invoke-Expression");
    match shell.as_deref() {
        Some("bash") => ("~/.bashrc", "eval \"$(bk init bash)\""),
        Some("zsh") => ("~/.zshrc", "eval \"$(bk init zsh)\""),
        Some("powershell") => PWSH,
        _ if cfg!(windows) => PWSH,
        _ => ("your shell's rc file", "eval \"$(bk init bash)\"   # or zsh"),
    }
}

/// How to load the hook (see [`hook_line`]), as a paragraph.
pub fn hook_hint(env: &EnvMap) -> String {
    let (file, line) = hook_line(env);
    format!("add this to {file} and open a new terminal:\n\n    {line}\n")
}

fn var_in(env: &EnvMap, key: &str) -> Option<String> {
    env::get_string(env, key)
}

/// The shell whose bk hook is active: zsh/bash set `BK_SHELL_INTEGRATION` for each `bk` call;
/// PowerShell's hook keeps a session file named by `BK_PS_SESSION`.
pub fn hook_shell(env: &EnvMap) -> Option<String> {
    var_in(env, "BK_SHELL_INTEGRATION").or_else(|| var_in(env, "BK_PS_SESSION").map(|_| "powershell".to_string()))
}

/// What the hook knows about the last command you ran.
#[derive(Debug, Default)]
struct LastCommand {
    cmd: String,
    expanded: String,
    pwd: String,
    status: String,
    conda: String,
    venv: String,
    kind: String,
}

fn non_empty(s: String) -> Option<String> {
    Some(s).filter(|s| !s.trim().is_empty())
}

/// From zsh/bash's `BK_LAST_*` variables, or PowerShell's session file.
fn last_command(ctx: &Ctx) -> Option<LastCommand> {
    if var(ctx, "BK_SHELL_INTEGRATION").is_some() {
        let v = |k: &str| var(ctx, k).unwrap_or_default();
        return Some(LastCommand {
            cmd: v("BK_LAST_CMD"),
            expanded: v("BK_LAST_CMD_EXPANDED"),
            pwd: v("BK_LAST_PWD"),
            status: v("BK_LAST_STATUS"),
            conda: v("BK_LAST_CONDA"),
            venv: v("BK_LAST_VENV"),
            kind: v("BK_LAST_KIND"),
        });
    }
    let file = var(ctx, "BK_PS_SESSION")?;
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
            let f = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            Some(LastCommand {
                cmd: f("cmd"),
                expanded: f("expanded"),
                pwd: f("pwd"),
                status: f("status"),
                conda: f("conda"),
                venv: f("venv"),
                kind: f("kind"),
            })
        }
        // The hook is loaded; nothing has run in this session yet.
        Err(_) => Some(LastCommand::default()),
    }
}

pub fn init(shell: InitShell, record: bool) -> Result<i32> {
    let paths = bk_core::paths::Paths::from_env();
    let recent = paths.recent_file().display().to_string();
    let sessions = paths.data_dir.join("sessions").display().to_string();
    let quote = |s: &str| match shell {
        InitShell::Powershell => bk_core::shell::pwsh_string(s),
        _ => render::q(s),
    };
    let template = match shell {
        InitShell::Zsh => INIT_ZSH,
        InitShell::Bash => INIT_BASH,
        InitShell::Powershell => INIT_POWERSHELL,
    };
    let script = template
        .replace("__BK_RECORD_VALUE__", if record { "1" } else { "0" })
        .replace("__BK_RECENT_FILE__", &quote(&recent))
        .replace("__BK_SESSION_DIR__", &quote(&sessions));
    print!("{script}");
    Ok(0)
}

fn var(ctx: &Ctx, key: &str) -> Option<String> {
    env::get_string(&ctx.env, key)
}

pub fn save(ctx: &Ctx, a: SaveArgs) -> Result<i32> {
    let Some(last) = last_command(ctx) else {
        bail!("`bk save` needs the shell hook to know what you just ran; {}", hook_hint(&ctx.env));
    };
    let Some(mut command) = non_empty(last.cmd.trim().to_string()) else {
        bail!("no command recorded in this shell yet; run something first, then `bk save {}`", a.name);
    };
    let (file, project_dir) = commands::target_file(ctx, a.project);
    commands::ensure_free(&file, &a.name, a.force)?;

    // Saved commands run without your aliases/functions; offer the expanded text.
    let kind = last.kind.clone();
    if matches!(kind.as_str(), "alias" | "function") {
        let first = command.split_whitespace().next().unwrap_or_default().to_string();
        let expanded = non_empty(last.expanded.clone()).filter(|x| *x != command);
        eprintln!(
            "{} `{first}` is a shell {kind}; bookmarks run without your aliases and functions.",
            style("note:").yellow().bold()
        );
        if let Some(x) = expanded {
            let use_it = a.yes
                || !commands::interactive()
                || Confirm::with_theme(&commands::theme())
                    .with_prompt(format!("Save the expanded command instead?  {x}"))
                    .default(true)
                    .interact()?;
            if use_it {
                command = x;
            }
        }
    }

    let dir = non_empty(last.pwd.clone()).map(PathBuf::from).unwrap_or_else(|| ctx.cwd.clone());
    let cwd = (!a.no_cwd).then(|| commands::stored_dir(&dir, project_dir.as_deref()));

    // The env that was active when the command ran (captured by the hook).
    let mut active: EnvMap = ctx.env.clone();
    for (key, value) in [("VIRTUAL_ENV", &last.venv), ("CONDA_DEFAULT_ENV", &last.conda)] {
        match non_empty(value.clone()) {
            Some(v) => bk_core::env::set(&mut active, key, v),
            None => bk_core::env::unset(&mut active, key),
        }
    }
    let (env_spec, env_note) = if a.no_env {
        (EnvSpec::Inherit, "not pinned")
    } else {
        commands::choose_env(a.env.as_deref(), &active, &dir)?
    };

    let runtimes =
        if a.no_env { Default::default() } else { commands::choose_runtimes(&[], &active, &dir, &env_spec)? };
    let b = Bookmark {
        description: a.description,
        cwd,
        env: env_spec,
        runtimes,
        command: Some(command),
        tags: a.tags,
        ..Default::default()
    };

    if let Some(status) = non_empty(last.status.clone()).filter(|s| s != "0") {
        eprintln!("{} that command exited with status {status}", style("note:").yellow().bold());
    }
    if !a.yes && commands::interactive() {
        eprintln!("{} {}", style("save").bold(), style(&a.name).bold());
        let preview = bk_core::Entry::new(
            &a.name,
            if a.project { bk_core::Scope::Project } else { bk_core::Scope::Global },
            &file,
            b.clone(),
        );
        commands::print_details(ctx, &preview, Some(env_note), false);
        if !Confirm::with_theme(&commands::theme()).with_prompt("Save?").default(true).interact()? {
            return Ok(1);
        }
    }
    let entry = commands::persist(ctx, &file, &a.name, a.project, &b)?;
    if a.yes || !commands::interactive() {
        commands::report_saved(ctx, &entry, env_note);
    } else {
        eprintln!(
            "{} saved — run it any time with {}",
            style("✓").green().bold(),
            style(format!("bk {}", a.name)).cyan()
        );
    }
    Ok(0)
}

/// The shell language `bk go` prints: as asked, else the hook's, else this OS's.
fn go_dialect(ctx: &Ctx, shell: Option<GoShell>) -> Dialect {
    match shell {
        Some(GoShell::Posix) => Dialect::Posix,
        Some(GoShell::Powershell) => Dialect::PowerShell,
        None => match hook_shell(&ctx.env).as_deref() {
            Some("powershell") => Dialect::PowerShell,
            Some(_) => Dialect::Posix,
            None => Dialect::native(),
        },
    }
}

pub fn go(ctx: &Ctx, name: &str, print: bool, shell: Option<GoShell>) -> Result<i32> {
    let lib = ctx.library();
    let e = ctx.find(&lib, name)?;
    let dialect = go_dialect(ctx, shell);
    if e.bookmark.is_remote() {
        return crate::remote_cmds::go(ctx, &lib, e, print, dialect);
    }
    let script = render::enter_script_for(dialect, e, &ctx.cwd, &ctx.env);
    if print {
        println!("{script}");
        return Ok(0);
    }
    // PowerShell's hook runs this at the next prompt, i.e. right after `bk go` returns.
    if let (Dialect::PowerShell, Some(session)) = (dialect, var(ctx, "BK_PS_SESSION")) {
        std::fs::write(format!("{session}.go"), &script)?;
        return Ok(0);
    }
    eprintln!("`bk go` changes your current shell, so it needs the shell hook; {}", hook_hint(&ctx.env));
    match dialect {
        Dialect::Posix => eprintln!("or run:  eval \"$(bk go --print {name})\""),
        Dialect::PowerShell => eprintln!("or run:  bk go --print {name} | Out-String | Invoke-Expression"),
    }
    eprintln!("\n{}", style(&script).dim());
    Ok(1)
}
