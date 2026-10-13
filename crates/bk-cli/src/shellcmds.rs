//! Commands that work with the interactive shell: `save`, `go`, `init`.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, bail};
use bk_core::env::{self, EnvMap, conda};
use bk_core::{Bookmark, EnvSpec, render};
use console::style;
use dialoguer::Confirm;

use crate::cli::{InitShell, SaveArgs};
use crate::commands::{self, Ctx};

const INIT_ZSH: &str = include_str!("shell/init.zsh");

const HOOK_HINT: &str = "add this to ~/.zshrc and open a new terminal:\n\n    eval \"$(bk init zsh)\"\n";

pub fn init(shell: InitShell, record: bool) -> Result<i32> {
    match shell {
        InitShell::Zsh => print!("{}", INIT_ZSH.replace("__BK_RECORD_VALUE__", if record { "1" } else { "0" })),
    }
    Ok(0)
}

fn var(ctx: &Ctx, key: &str) -> Option<String> {
    env::get_string(&ctx.env, key)
}

pub fn save(ctx: &Ctx, a: SaveArgs) -> Result<i32> {
    if var(ctx, "BK_SHELL_INTEGRATION").is_none() {
        bail!("`bk save` needs the shell hook to know what you just ran; {HOOK_HINT}");
    }
    let Some(mut command) = var(ctx, "BK_LAST_CMD") else {
        bail!("no command recorded in this shell yet; run something first, then `bk save {}`", a.name);
    };
    let (file, project_dir) = commands::target_file(ctx, a.project);
    commands::ensure_free(&file, &a.name, a.force)?;

    // Saved commands run without your aliases/functions; offer the expanded text.
    let kind = var(ctx, "BK_LAST_KIND").unwrap_or_default();
    if matches!(kind.as_str(), "alias" | "function") {
        let first = command.split_whitespace().next().unwrap_or_default().to_string();
        let expanded = var(ctx, "BK_LAST_CMD_EXPANDED").filter(|x| *x != command);
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

    let dir = var(ctx, "BK_LAST_PWD").map(PathBuf::from).unwrap_or_else(|| ctx.cwd.clone());
    let cwd = (!a.no_cwd).then(|| commands::stored_dir(&dir, project_dir.as_deref()));

    // The env that was active when the command ran (captured by the hook).
    let mut active: EnvMap = ctx.env.clone();
    for (key, from) in [("VIRTUAL_ENV", "BK_LAST_VENV"), ("CONDA_DEFAULT_ENV", "BK_LAST_CONDA")] {
        match var(ctx, from) {
            Some(v) => active.insert(OsString::from(key), OsString::from(v)),
            None => active.remove(&OsString::from(key)),
        };
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

    if let Some(status) = var(ctx, "BK_LAST_STATUS").filter(|s| s != "0") {
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

pub fn go(ctx: &Ctx, name: &str, print: bool) -> Result<i32> {
    let lib = ctx.library();
    let e = ctx.find(&lib, name)?;
    if e.bookmark.is_remote() {
        return crate::remote_cmds::go(ctx, &lib, e, print);
    }
    let mut script = render::enter_script(e, &ctx.cwd, &ctx.env);
    // `conda activate` needs conda's shell function; load it if the shell doesn't have it.
    if matches!(e.bookmark.env, EnvSpec::Conda { .. })
        && let Some(base) = conda::find_base(&ctx.env)
    {
        let sh = render::q_path(&base.join("etc/profile.d/conda.sh"));
        script = format!("case \"$(type conda 2>/dev/null)\" in *function*) ;; *) . {sh} ;; esac && {script}");
    }
    if print {
        println!("{script}");
        return Ok(0);
    }
    eprintln!("`bk go` changes your current shell, so it needs the shell hook; {HOOK_HINT}");
    eprintln!("or run:  eval \"$(bk go --print {name})\"");
    eprintln!("\n{}", style(&script).dim());
    Ok(1)
}
