//! `bk sync`: sync bookmarks, servers, settings and history through your Telegram bot.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use bk_core::sync::{self, SyncConfig, telegram::Bot};
use console::style;
use dialoguer::{Confirm, Input, Password};

use crate::cli::{SyncCmd, SyncSetupArgs};
use crate::commands::{self, Ctx};

pub fn sync(ctx: &Ctx, cmd: Option<SyncCmd>) -> Result<i32> {
    match cmd {
        None => run(ctx),
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
    eprintln!("{} {}", style("✓").green().bold(), s.line());
    for skipped in &s.skipped {
        eprintln!("{} skipped {skipped}", style("warning:").yellow());
    }
    if let Some(w) = &s.warning {
        eprintln!("{} {w}", style("warning:").yellow());
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
    match state.last_sync_ms {
        Some(ms) => {
            println!("last sync {} ({})", local_time(ms), state.last_summary.as_deref().unwrap_or("no summary"))
        }
        None => println!("last sync never"),
    }
    match sync::remote_info(&cfg) {
        Ok(Some((device, ms))) => println!("latest    from {device} at {}", local_time(ms)),
        Ok(None) => println!("latest    nothing synced yet"),
        Err(e) => println!("latest    {}", style(format!("{e:#}")).red()),
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
    eprintln!("{} {} is set up; run `bk sync` whenever you want to sync", style("✓").green().bold(), cfg.device);
    let now = a.yes
        || !interactive
        || Confirm::with_theme(&commands::theme()).with_prompt("Sync now?").default(true).interact()?;
    if now { run(ctx) } else { Ok(0) }
}
