//! Automatic sync for the CLI (devices without the desktop app): after a command, when this
//! device has changes to send or hasn't synced for a while, a detached `bk sync --background`
//! does it. It never blocks or fails the command that triggered it.

use std::process::{Command, Stdio};

use bk_core::history;
use bk_core::sync::{self, AutoState, State, SyncConfig};

use crate::commands::Ctx;

const MINUTE: i64 = 60_000;

pub fn after_command(ctx: &Ctx) {
    if std::env::var_os("BK_NO_AUTO_SYNC").is_some() {
        return;
    }
    let Some(cfg) = SyncConfig::load(&ctx.paths) else { return };
    if !cfg.auto {
        return;
    }
    let Ok(pending) = sync::pending(&ctx.paths) else { return };
    let now = history::now_ms();
    let since_attempt = now - AutoState::load(&ctx.paths).last_attempt_ms.unwrap_or(0);
    let since_sync = now - State::load(&ctx.paths).last_sync_ms.unwrap_or(0);
    // Changes go out within seconds; otherwise check in (and send runs) every ten minutes.
    let due = (pending.items > 0 && since_attempt > 5_000) || (since_sync > 10 * MINUTE && since_attempt > 2 * MINUTE);
    if due && AutoState::record(&ctx.paths, None).is_ok() {
        let _ = spawn_background();
    }
}

fn spawn_background() -> std::io::Result<()> {
    let mut c = Command::new(std::env::current_exe()?);
    c.args(["sync", "--background"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Not the caller's folder, which it may want to delete.
        .current_dir(std::env::temp_dir());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own session, so closing the terminal or Ctrl+C doesn't stop it halfway.
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Outlive a terminal that closes its job; not every job allows breaking away.
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if c.spawn().is_ok() {
            return Ok(());
        }
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    c.spawn().map(drop)
}
