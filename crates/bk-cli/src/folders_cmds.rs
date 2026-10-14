//! `bk locate` and `bk folders`: where folders named by bookmarks from your other devices
//! are on this one. The mappings live on this device only; bookmarks are never rewritten.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use bk_core::folders::{self, FolderMap, LocateResult, Located, Suggester};
use bk_core::{Entry, Library, doctor, paths};
use console::style;
use dialoguer::{Confirm, Input};

use crate::cli::FoldersCmd;
use crate::commands::{self, Ctx};

pub fn locate(ctx: &Ctx, name: &str, dir: Option<PathBuf>) -> Result<i32> {
    let lib = ctx.library();
    let e = ctx.find(&lib, name)?;
    let dir = match dir {
        Some(d) => paths::resolve(&d.to_string_lossy(), &ctx.cwd),
        None => ctx.cwd.clone(),
    };
    report(&folders::locate(&ctx.paths, &lib, e, &dir)?);
    Ok(0)
}

pub fn folders(ctx: &Ctx, cmd: Option<FoldersCmd>) -> Result<i32> {
    match cmd {
        None => list(ctx),
        Some(FoldersCmd::Add { shared, local }) => {
            let local = paths::resolve(&local, &ctx.cwd);
            if !local.is_dir() {
                bail!("{} is not a folder on this device", local.display());
            }
            let shown = paths::contract_tilde(&local);
            let replaced = FolderMap::add(&ctx.paths, &shared, &shown)?;
            eprintln!("{} {shared} → {shown} on this device", style("✓").green().bold());
            if let Some(old) = replaced {
                eprintln!("  {}", style(format!("(was {old})")).dim());
            }
            Ok(0)
        }
        Some(FoldersCmd::Rm { shared }) => {
            if !FolderMap::remove(&ctx.paths, &shared)? {
                bail!("`{shared}` isn't mapped on this device (see `bk folders`)");
            }
            eprintln!("{} {shared} is no longer mapped on this device", style("✓").green().bold());
            Ok(0)
        }
    }
}

fn list(ctx: &Ctx) -> Result<i32> {
    let map = FolderMap::load(&ctx.paths);
    if map.rules().is_empty() {
        eprintln!("No folders are mapped on this device.");
        eprintln!(
            "When a bookmark from another device can't find its folder here, `bk locate NAME [DIR]` says where it is."
        );
        return Ok(0);
    }
    let lib = ctx.library();
    let width = map.rules().iter().map(|r| r.shared.chars().count()).max().unwrap_or(0);
    for rule in map.rules() {
        let here = paths::expand_tilde(&rule.local).is_dir();
        let local =
            if here { style(rule.local.clone()) } else { style(format!("{} (not found)", rule.local)).yellow() };
        let n = map.used_by(rule, &lib);
        let used = style(format!("{n} bookmark{}", if n == 1 { "" } else { "s" })).dim();
        println!("{:<width$}  →  {local}  {used}", rule.shared);
    }
    Ok(0)
}

fn report(r: &LocateResult) {
    eprintln!("{} {} → {} on this device", style("✓").green().bold(), r.shared, r.local);
    if let Some(old) = &r.replaced {
        eprintln!("  {}", style(format!("(was {old})")).dim());
    }
    if !r.fixed.is_empty() {
        eprintln!("  {} {}", style("also found:").dim(), r.fixed.join(", "));
    }
}

/// Before a run: when `e`'s folder isn't on this device, offer the folder we found (or ask
/// where it is) and remember it; without a terminal, say how. Returns the reloaded library
/// when a folder was located.
pub fn ensure_here(ctx: &Ctx, lib: &Library, e: &Entry) -> Result<Option<Library>> {
    let Some(reason) = e.folder().as_ref().and_then(Located::missing_reason) else { return Ok(None) };
    let written = e.bookmark.cwd.as_deref().unwrap_or_default();
    let suggestion = Suggester::new(lib, &doctor::project_dirs(&ctx.paths)).suggest(written).into_iter().next();
    if !commands::interactive() {
        let dir = suggestion.as_deref().map(shown).unwrap_or_else(|| "DIR".into());
        bail!("`{}`'s folder {reason}\n  say where it is here: bk locate {} {dir}", e.name, e.name);
    }
    eprintln!("{} `{}`'s folder {reason}", style("!").yellow().bold(), e.name);
    let dir = match suggestion {
        Some(s)
            if Confirm::with_theme(&commands::theme())
                .with_prompt(format!("Use {} on this device?", shown(&s)))
                .default(true)
                .interact()? =>
        {
            s
        }
        _ => {
            let typed: String = Input::with_theme(&commands::theme())
                .with_prompt("Where is it on this device? (empty to cancel)")
                .allow_empty(true)
                .interact_text()?;
            if typed.trim().is_empty() {
                bail!("cancelled");
            }
            paths::resolve(typed.trim(), &ctx.cwd)
        }
    };
    report(&folders::locate(&ctx.paths, lib, e, &dir)?);
    Ok(Some(ctx.library()))
}

/// A folder as the user would type it (`~/…`, quoted when it has spaces).
fn shown(p: &Path) -> String {
    let s = paths::contract_tilde(p);
    if s.contains(' ') { format!("\"{s}\"") } else { s }
}
