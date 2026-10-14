//! `bk sync`: sync bookmarks, servers, settings and history through your Telegram bot.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use bk_core::Os;
use bk_core::sync::{self, AutoState, SyncConfig, telegram::Bot};
use console::style;
use dialoguer::{Confirm, Input, Password};

use crate::cli::{OnOff, SyncCmd, SyncSetupArgs};
use crate::commands::{self, Ctx};

pub fn sync(ctx: &Ctx, cmd: Option<SyncCmd>, background: bool) -> Result<i32> {
    match cmd {
        None if background => run_in_background(ctx),
        None => run(ctx),
        Some(SyncCmd::Auto { state }) => auto(ctx, state),
        Some(SyncCmd::Setup(a)) => setup(ctx, a),
        Some(SyncCmd::Status) => status(ctx),
        Some(SyncCmd::Forget) => {
            SyncConfig::forget(&ctx.paths)?;
            eprintln!("{} this device stopped syncing (the chat in Telegram is untouched)", style("✓").green().bold());
            Ok(0)
        }
    }
}

fn config(ctx: &Ctx) -> Result<SyncConfig> {
    SyncConfig::load(&ctx.paths).context("sync isn't set up on this device; run `bk sync setup`")
}

use sync::local_time;

fn run(ctx: &Ctx) -> Result<i32> {
    let cfg = config(ctx)?;
    eprintln!("{}", style(format!("syncing {} with @{}…", cfg.device, cfg.bot_username)).dim());
    let s = sync::sync(&ctx.paths, &cfg)?;
    let _ = AutoState::record(&ctx.paths, None);
    eprintln!("{} {}", style("✓").green().bold(), s.line());
    for skipped in &s.skipped {
        eprintln!("{} skipped {skipped}", style("warning:").yellow());
    }
    if let Some(w) = &s.warning {
        eprintln!("{} {w}", style("warning:").yellow());
    }
    Ok(0)
}

/// `bk NAME` for a bookmark that isn't here: maybe another device just made it. Returns
/// whether a sync brought anything.
pub fn fetch_missing(ctx: &Ctx, name: &str) -> bool {
    let Some(cfg) = SyncConfig::load(&ctx.paths).filter(|c| c.auto) else { return false };
    eprintln!("{}", style(format!("`{name}` isn't here yet; syncing…")).dim());
    match sync::sync(&ctx.paths, &cfg) {
        Ok(s) => {
            let _ = AutoState::record(&ctx.paths, None);
            s.received > 0
        }
        Err(e) => {
            eprintln!("{} sync failed: {e:#}", style("warning:").yellow());
            false
        }
    }
}

/// What automatic syncing runs: quiet, gives way to a sync already under way, and keeps
/// going while edits arrive during it.
fn run_in_background(ctx: &Ctx) -> Result<i32> {
    let Some(cfg) = SyncConfig::load(&ctx.paths).filter(|c| c.auto) else { return Ok(0) };
    for _ in 0..3 {
        match sync::sync_if_idle(&ctx.paths, &cfg) {
            Ok(None) => return Ok(0),
            Ok(Some(_)) => AutoState::record(&ctx.paths, None)?,
            Err(e) => {
                AutoState::record(&ctx.paths, Some(format!("{e:#}")))?;
                return Ok(1);
            }
        }
        if sync::pending(&ctx.paths)?.items == 0 {
            break;
        }
    }
    Ok(0)
}

fn auto(ctx: &Ctx, state: Option<OnOff>) -> Result<i32> {
    let cfg = match state {
        Some(s) => SyncConfig::set_auto(&ctx.paths, s == OnOff::On)?,
        None => config(ctx)?,
    };
    if cfg.auto {
        eprintln!("{} syncs automatically (turn off with `bk sync auto off`)", cfg.device);
    } else {
        eprintln!("{} syncs only when you run `bk sync` (turn on with `bk sync auto on`)", cfg.device);
    }
    Ok(0)
}

fn status(ctx: &Ctx) -> Result<i32> {
    let Some(cfg) = SyncConfig::load(&ctx.paths) else {
        eprintln!("Sync isn't set up on this device. Run `bk sync setup`.");
        return Ok(1);
    };
    let state = sync::State::load(&ctx.paths);
    println!("device    {}", cfg.device);
    println!("bot       @{}", cfg.bot_username);
    println!("auto      {}", if cfg.auto { "on" } else { "off (`bk sync auto on`)" });
    match state.last_sync_ms {
        Some(ms) => {
            println!("last sync {} ({})", local_time(ms), state.last_summary.as_deref().unwrap_or("no summary"))
        }
        None => println!("last sync never"),
    }
    if let Some(err) = AutoState::load(&ctx.paths).failing(&state) {
        println!("failing   {}", style(err).red());
    }
    let pending = sync::pending(&ctx.paths)?;
    if pending.items > 0 {
        println!("to send   {} change{}", pending.items, if pending.items == 1 { "" } else { "s" });
    }
    match sync::remote_info(&cfg) {
        Ok(Some((device, ms))) => println!("latest    from {device} at {}", local_time(ms)),
        Ok(None) => println!("latest    nothing synced yet"),
        Err(e) => println!("latest    {}", style(format!("{e:#}")).red()),
    }
    if !state.devices.is_empty() {
        println!("devices");
        let width = state.devices.keys().map(|d| d.chars().count()).max().unwrap_or(0);
        for (name, d) in &state.devices {
            let os = Os::parse(&d.os).map(Os::label).unwrap_or(&d.os);
            let this = if *name == cfg.device { " (this one)" } else { "" };
            println!("  {name:<width$}  {os:<7}  last change {}{this}", local_time(d.last_change_ms));
        }
    }
    Ok(0)
}

fn setup(ctx: &Ctx, a: SyncSetupArgs) -> Result<i32> {
    let interactive = commands::interactive();
    if let Some(cfg) = SyncConfig::load(&ctx.paths) {
        let again = a.force
            || (interactive
                && Confirm::with_theme(&commands::theme())
                    .with_prompt(format!(
                        "This device already syncs as {} with @{}. Set it up again?",
                        cfg.device, cfg.bot_username
                    ))
                    .default(false)
                    .interact()?);
        if !again {
            bail!("sync is already set up here (use --force to set it up again)");
        }
    }
    let token = match a.token {
        Some(t) => t,
        None if interactive => {
            eprintln!("1. In Telegram, message @BotFather, send /newbot, and copy the token it gives you.");
            Password::with_theme(&commands::theme()).with_prompt("Bot token").interact()?
        }
        None => bail!("pass --token (non-interactive setup)"),
    };
    let bot = Bot::new(&token);
    let me = bot.get_me()?;
    eprintln!(
        "2. Open {} in Telegram and press Start. Waiting up to 2 minutes…",
        style(format!("https://t.me/{}", me.username)).cyan()
    );
    let contact = bot.wait_for_contact(Duration::from_secs(120))?;
    eprintln!(
        "{} connected to {}",
        style("✓").green().bold(),
        if contact.name.is_empty() { "you" } else { &contact.name }
    );

    let existing = sync::has_remote(&token, contact.chat_id)?;
    let passphrase = match std::env::var("BK_SYNC_PASSPHRASE").ok().filter(|p| !p.is_empty()) {
        Some(p) => p,
        None if !interactive => bail!("set BK_SYNC_PASSPHRASE (non-interactive setup)"),
        None if existing => {
            eprintln!("3. This bot already holds synced data: enter the passphrase your other devices use.");
            Password::with_theme(&commands::theme()).with_prompt("Passphrase").interact()?
        }
        None => {
            eprintln!("3. Choose a passphrase. Everything is encrypted with it before it leaves this device;");
            eprintln!("   you'll enter it once on each device, and it can't be recovered.");
            Password::with_theme(&commands::theme())
                .with_prompt("Passphrase (8+ characters)")
                .with_confirmation("Repeat it", "the passphrases don't match")
                .interact()?
        }
    };
    let device = match a.device {
        Some(d) => d,
        None if interactive => Input::with_theme(&commands::theme())
            .with_prompt("Name for this device")
            .default(sync::default_device_name())
            .interact_text()?,
        None => sync::default_device_name(),
    };
    let cfg = sync::finish_setup(&ctx.paths, &token, &me.username, &contact, &passphrase, &device)?;
    eprintln!(
        "{} {} is set up and syncs automatically (`bk sync` syncs now, `bk sync auto off` turns it off)",
        style("✓").green().bold(),
        cfg.device
    );
    let now = a.yes
        || !interactive
        || Confirm::with_theme(&commands::theme()).with_prompt("Sync now?").default(true).interact()?;
    if now { run(ctx) } else { Ok(0) }
}
