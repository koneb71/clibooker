//! `bk keys`: SSH keys kept by Clibooker, synced between your devices, and used by servers as
//! `-i key:NAME`. Making, unlocking and installing a key run OpenSSH's own tools in this
//! terminal, so passphrases and passwords are only ever typed into them.

use std::io::Read;

use anyhow::{Context, Result, bail};
use bk_core::keys::{self, Agent, KeyInfo, KeyType};
use bk_core::render::Dialect;
use bk_core::servers::{self, Batch, Reach};
use bk_core::sync::{self, SyncConfig};
use bk_core::{Library, paths};
use console::style;
use dialoguer::Confirm;

use crate::cli::{KeyKind, KeysCmd};
use crate::commands::{self, Ctx};
use crate::exec;
use crate::remote_cmds::{reach_text, ssh_program};

pub fn keys(ctx: &Ctx, cmd: Option<KeysCmd>) -> Result<i32> {
    let lib = ctx.library();
    match cmd.unwrap_or(KeysCmd::Ls) {
        KeysCmd::Ls => list(ctx, &lib),
        KeysCmd::Import { file, name } => import(ctx, &lib, &file, name),
        KeysCmd::New { name, comment, kind, no_passphrase } => new(ctx, &lib, &name, comment, kind, no_passphrase),
        KeysCmd::Install { name, server } => install(ctx, &lib, &name, &server),
        KeysCmd::Unlock { name } => agent_cmd(ctx, &lib, &name, true),
        KeysCmd::Lock { name } => agent_cmd(ctx, &lib, &name, false),
        KeysCmd::Pub { name } => {
            println!("{}", keys::info(&ctx.paths, &name)?.public);
            Ok(0)
        }
        KeysCmd::Mv { old, new } => {
            let moved = keys::rename(&ctx.paths, &old, &new)?;
            eprintln!("{} renamed key {old} → {new}", style("✓").green().bold());
            if !moved.is_empty() {
                eprintln!("  {}", style(format!("servers using it: {}", moved.join(", "))).dim());
            }
            Ok(0)
        }
        KeysCmd::Rm { name, yes } => remove(ctx, &lib, &name, yes),
    }
}

fn tool(lib: &Library, name: &str) -> String {
    keys::tool(&ssh_program(lib), name)
}

fn agent(ctx: &Ctx, lib: &Library) -> Agent {
    keys::agent(&tool(lib, "ssh-add"), &servers::ssh_env(&ctx.env))
}

/// `passphrase, unlocked` and the like.
fn lock_state(k: &KeyInfo, agent: &Agent) -> String {
    match (k.encrypted, agent.holds(&k.fingerprint)) {
        (true, true) => "passphrase · unlocked".into(),
        (true, false) => "passphrase".into(),
        (false, true) => "no passphrase · in agent".into(),
        (false, false) => "no passphrase".into(),
    }
}

fn list(ctx: &Ctx, lib: &Library) -> Result<i32> {
    let saved = keys::list(&ctx.paths);
    if saved.is_empty() {
        eprintln!("No saved SSH keys.");
        eprintln!(
            "  save one: {}   make one: {}",
            style("bk keys import ~/.ssh/id_ed25519").cyan(),
            style("bk keys new NAME").cyan()
        );
        return Ok(0);
    }
    let agent = agent(ctx, lib);
    let clashes = sync::key_clashes(&ctx.paths);
    let width = saved.iter().map(|k| k.name.len()).max().unwrap_or(0);
    let kind_width = saved.iter().map(|k| k.kind.len()).max().unwrap_or(0);
    for k in &saved {
        let used = keys::used_by(&lib.servers, &k.name);
        let used = if used.is_empty() { String::new() } else { format!("  used by {}", used.join(", ")) };
        let state = lock_state(k, &agent);
        let state = if k.encrypted { style(state).green() } else { style(state).yellow() };
        println!(
            "{:<width$}  {:<kind_width$}  {}  {state}{}",
            style(&k.name).bold(),
            k.kind,
            style(&k.fingerprint).dim(),
            style(used).dim()
        );
        if clashes.contains(&k.name) {
            println!(
                "{:<width$}  {}",
                "",
                style("another device has a different key with this name: rename one (bk keys mv)").yellow()
            );
        }
    }
    if let Agent::Unavailable(why) = &agent {
        eprintln!("{}", style(why).dim());
    }
    Ok(0)
}

/// What to say about a key that has no passphrase.
fn no_passphrase_note(ctx: &Ctx, k: &KeyInfo) {
    if k.encrypted {
        return;
    }
    let file = keys::path(&ctx.paths, &k.name).map(|p| p.display().to_string()).unwrap_or_default();
    let add = bk_core::render::command_line(Dialect::native(), "ssh-keygen", &["-p".into(), "-f".into(), file]);
    let why = if SyncConfig::load(&ctx.paths).is_some() {
        "it syncs to your devices, protected by your sync passphrase alone"
    } else {
        "anyone who gets the file can use it"
    };
    eprintln!("  {} `{}` has no passphrase: {why}", style("note:").yellow(), k.name);
    eprintln!("  add one: {}", style(add).cyan());
}

fn saved_line(k: &KeyInfo) -> String {
    format!(
        "{} saved key {} {}",
        style("✓").green().bold(),
        style(&k.name).bold(),
        style(format!("({} {})", k.kind, k.fingerprint)).dim()
    )
}

fn import(ctx: &Ctx, lib: &Library, file: &str, name: Option<String>) -> Result<i32> {
    let keygen = tool(lib, "ssh-keygen");
    let k = if file == "-" {
        let name = name.context("pass --name for a key read from stdin")?;
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        keys::import(&ctx.paths, &name, &text, None, &keygen)?
    } else {
        let path = paths::resolve(file, &ctx.cwd);
        let name = name.unwrap_or_else(|| keys::suggest_name(&path, &sync::this_device(&ctx.paths)));
        keys::import_file(&ctx.paths, &name, &path, &keygen)?
    };
    eprintln!("{}", saved_line(&k));
    no_passphrase_note(ctx, &k);
    eprintln!("  use it: {}", style(format!("bk server add NAME user@host -i key:{}", k.name)).cyan());
    Ok(0)
}

fn new(
    ctx: &Ctx,
    lib: &Library,
    name: &str,
    comment: Option<String>,
    kind: KeyKind,
    no_passphrase: bool,
) -> Result<i32> {
    if !no_passphrase && !commands::interactive() {
        bail!("ssh-keygen asks for the passphrase in a terminal; pass --no-passphrase to make a key without one");
    }
    let kind = match kind {
        KeyKind::Ed25519 => KeyType::Ed25519,
        KeyKind::Rsa => KeyType::Rsa,
    };
    let comment = comment.unwrap_or_else(|| keys::default_comment(&ctx.paths));
    let args = keys::generate_args(&ctx.paths, name, &comment, kind, !no_passphrase)?;
    let code = exec::run_attached(&tool(lib, "ssh-keygen"), &args, &servers::ssh_env(&ctx.env))?;
    if code != 0 {
        return Ok(code);
    }
    let k = keys::info(&ctx.paths, name)?;
    eprintln!("{}", saved_line(&k));
    no_passphrase_note(ctx, &k);
    eprintln!("  put it on a server: {}", style(format!("bk keys install {name} SERVER")).cyan());
    Ok(0)
}

fn agent_cmd(ctx: &Ctx, lib: &Library, name: &str, unlock: bool) -> Result<i32> {
    let ssh_add = tool(lib, "ssh-add");
    let env = servers::ssh_env(&ctx.env);
    if let Agent::Unavailable(why) = keys::agent(&ssh_add, &env) {
        bail!("{why}");
    }
    let args = if unlock {
        keys::unlock_args(&ssh_add, &ctx.paths, name)?
    } else {
        keys::lock_args(&ssh_add, &ctx.paths, name)?
    };
    exec::run_attached(&ssh_add, &args, &env)
}

fn install(ctx: &Ctx, lib: &Library, name: &str, server: &str) -> Result<i32> {
    let key = keys::info(&ctx.paths, name)?;
    let target = servers::resolve_target(server, &lib.servers)?;
    let program = ssh_program(lib);
    let env = servers::ssh_env(&ctx.env);
    eprintln!(
        "{} putting key {} on {}: log in the way you do now (a password, usually)",
        style("→").cyan(),
        style(name).bold(),
        style(&target.name).bold()
    );
    let code = exec::run_attached(&program, &keys::install_args(&program, &target, &ctx.paths, name)?, &env)?;
    if code != 0 {
        eprintln!("{} couldn't put the key on {} (exit {code})", style("✗").red(), target.name);
        eprintln!(
            "  If it only accepts keys, add this line to ~/.ssh/authorized_keys on it another way: {}",
            style(format!("bk keys pub {name}")).cyan()
        );
        return Ok(code);
    }
    let identity = format!("{}{name}", keys::PREFIX);
    if keys::after_install(&ctx.paths, &target.name, name)? {
        eprintln!("{} {} now uses key {name}", style("✓").green().bold(), target.name);
    } else {
        let file = keys::path(&ctx.paths, name)?;
        let hint = match target.source {
            servers::Source::SshConfig => {
                format!("add `IdentityFile {}` to its Host entry in ~/.ssh/config", file.display())
            }
            _ => format!("save it as a server: bk server add NAME {} -i {identity}", target.destination),
        };
        eprintln!("  {} {} isn't a Clibooker server, so to use the key: {hint}", style("note:").yellow(), target.name);
    }
    let mut with_key = target.clone();
    with_key.identity = Some(identity);
    let batch = Batch { program, target: &with_key, paths: &ctx.paths, env };
    match servers::check(&batch) {
        Reach::Reachable { .. } => eprintln!("{} logs in with the key", style("✓").green().bold()),
        Reach::AuthRequired if key.encrypted => eprintln!(
            "  {} to let background checks use it, unlock it once: {}",
            style("note:").yellow(),
            style(format!("bk keys unlock {name}")).cyan()
        ),
        r => eprintln!("  {}", reach_text(&r)),
    }
    Ok(0)
}

fn remove(ctx: &Ctx, lib: &Library, name: &str, yes: bool) -> Result<i32> {
    if !keys::exists(&ctx.paths, name) {
        bail!("no saved key named `{name}`");
    }
    let used = keys::used_by(&lib.servers, name);
    let synced = SyncConfig::load(&ctx.paths).is_some();
    if !yes {
        if !commands::interactive() {
            bail!("pass --yes to delete key `{name}`");
        }
        if !used.is_empty() {
            eprintln!("  {}", style(format!("used by {}", used.join(", "))).yellow());
        }
        if synced {
            eprintln!("  {}", style("it's deleted from your other devices too, at their next sync").yellow());
        }
        let ok = Confirm::with_theme(&commands::theme())
            .with_prompt(format!("Delete saved key `{name}`?"))
            .default(false)
            .interact()?;
        if !ok {
            return Ok(1);
        }
    }
    keys::remove(&ctx.paths, name)?;
    eprintln!("{} deleted key {name}", style("✓").green().bold());
    if !used.is_empty() {
        eprintln!(
            "  {} servers still set to use it: {} (ssh tries your agent and default keys for them instead)",
            style("note:").yellow(),
            used.join(", ")
        );
    }
    Ok(0)
}
