//! Automatic sync in the app: shortly after launch, a few seconds after changes here, and when
//! another device has sent some (looked at every two minutes, and whenever the window is
//! focused). Offline, it backs off, up to half an hour. Failures never toast: Settings → Sync
//! says why.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use bk_core::Paths;
use bk_core::history;
use bk_core::sync::{self, AutoState, SyncConfig};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;
use crate::sync_cmds::SyncEvent;

/// The app's automatic syncing, shared with the commands and window events.
#[derive(Default)]
pub struct AutoSync {
    /// Look at the other devices at the next tick (the window was focused, or two devices
    /// synced at the same moment).
    nudged: AtomicBool,
    pub syncing: AtomicBool,
    failures: AtomicU32,
}

const TICK: Duration = Duration::from_secs(1);
const FIRST: Duration = Duration::from_secs(3);
/// How often to look for changes here (no network), and how long the bookmarks file must be
/// left alone before they're sent (an edit often comes in a few saves).
const LOCAL_EVERY: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_secs(5);
/// How often to ask whether another device sent something (one `getChat`).
const REMOTE_EVERY: Duration = Duration::from_secs(120);
/// Runs alone aren't urgent: they go out at most this often.
const RUNS_EVERY_MS: i64 = 10 * 60_000;

pub fn start(app: &AppHandle) {
    let auto = Arc::new(AutoSync::default());
    app.manage(auto.clone());
    if std::env::var_os("BK_NO_AUTO_SYNC").is_some() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || run(&app, &auto));
}

/// The main window was focused: see whether another device sent something.
pub fn nudge(app: &AppHandle) {
    if let Some(auto) = app.try_state::<Arc<AutoSync>>() {
        auto.nudged.store(true, Ordering::Relaxed);
    }
}

/// A sync the user asked for worked: whatever was failing is fixed.
pub fn reset(app: &AppHandle) {
    if let Some(auto) = app.try_state::<Arc<AutoSync>>() {
        auto.failures.store(0, Ordering::Relaxed);
    }
}

pub fn syncing(app: &AppHandle) -> bool {
    app.try_state::<Arc<AutoSync>>().is_some_and(|a| a.syncing.load(Ordering::Relaxed))
}

fn run(app: &AppHandle, auto: &AutoSync) {
    let paths = app.state::<AppState>().paths.clone();
    let launched = Instant::now();
    let (mut first_done, mut last_local, mut last_remote) = (false, launched, launched);
    let mut retry_at = launched;
    loop {
        std::thread::sleep(TICK);
        let Some(cfg) = SyncConfig::load(&paths).filter(|c| c.auto) else { continue };
        let now = Instant::now();
        if now < retry_at || (!first_done && now - launched < FIRST) {
            continue;
        }
        let look_here = now - last_local >= LOCAL_EVERY;
        if look_here {
            last_local = now;
        }
        let due = if !first_done || (look_here && changes_here(&paths)) {
            Ok(true)
        } else if auto.nudged.swap(false, Ordering::Relaxed) || now - last_remote >= REMOTE_EVERY {
            last_remote = now;
            sync::remote_head(&cfg).map(|head| head != sync::State::load(&paths).last_message_id)
        } else {
            Ok(false)
        };
        first_done = true;
        let result = match due {
            Ok(false) => continue,
            Ok(true) => {
                auto.syncing.store(true, Ordering::Relaxed);
                let r = sync::sync_if_idle(&paths, &cfg);
                auto.syncing.store(false, Ordering::Relaxed);
                r
            }
            Err(e) => Err(e),
        };
        match result {
            // A sync someone asked for is under way.
            Ok(None) => {}
            Ok(Some(s)) => {
                auto.failures.store(0, Ordering::Relaxed);
                let _ = AutoState::record(&paths, None);
                // Two devices synced at the same moment: look again right away.
                if s.warning.is_some() {
                    auto.nudged.store(true, Ordering::Relaxed);
                }
                let _ = app.emit("sync-finished", SyncEvent::done(&s, true));
            }
            Err(e) => {
                let failures = auto.failures.fetch_add(1, Ordering::Relaxed) + 1;
                retry_at = Instant::now() + backoff(failures);
                let msg = format!("{e:#}");
                let _ = AutoState::record(&paths, Some(msg.clone()));
                let _ = app.emit("sync-finished", SyncEvent::failed(msg, true));
            }
        }
    }
}

/// Changes to send: bookmarks, servers or settings once the file has been left alone for a
/// moment, or runs that have waited long enough.
fn changes_here(paths: &Paths) -> bool {
    let Ok(p) = sync::pending(paths) else { return false };
    let quiet = age(&paths.config_file).is_none_or(|a| a >= QUIET);
    let since_sync = history::now_ms() - sync::State::load(paths).last_sync_ms.unwrap_or(0);
    (p.items > 0 && quiet) || (p.new_runs && since_sync >= RUNS_EVERY_MS)
}

fn age(file: &Path) -> Option<Duration> {
    std::fs::metadata(file).ok()?.modified().ok()?.elapsed().ok()
}

/// 1, 2, 4… minutes, up to half an hour.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(60 * 2u64.pow(failures.saturating_sub(1).min(5))).min(Duration::from_secs(30 * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_up_to_half_an_hour() {
        let mins: Vec<u64> = (1..=8).map(|f| backoff(f).as_secs() / 60).collect();
        assert_eq!(mins, [1, 2, 4, 8, 16, 30, 30, 30]);
    }
}
