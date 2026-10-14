//! Sync between devices through the user's Telegram bot (Settings → Sync, tray → Sync now).

use bk_core::sync::{self, SyncConfig, telegram::Bot};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn e(err: impl std::fmt::Display) -> String {
    format!("{err:#}")
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(e)?.map_err(e)
}

#[derive(Serialize)]
pub struct SyncStatus {
    configured: bool,
    device: Option<String>,
    bot: Option<String>,
    last_sync_ms: Option<i64>,
    last_summary: Option<String>,
    default_device: String,
}

#[tauri::command]
pub fn sync_status(state: State<'_, AppState>) -> SyncStatus {
    let cfg = SyncConfig::load(&state.paths);
    let st = sync::State::load(&state.paths);
    SyncStatus {
        configured: cfg.is_some(),
        device: cfg.as_ref().map(|c| c.device.clone()),
        bot: cfg.as_ref().map(|c| c.bot_username.clone()),
        last_sync_ms: st.last_sync_ms,
        last_summary: st.last_summary,
        default_device: sync::default_device_name(),
    }
}

/// Setup step 1: is this a bot token Telegram accepts? Returns the bot's username.
#[tauri::command]
pub async fn sync_check_token(token: String) -> CmdResult<String> {
    blocking(move || Ok(Bot::new(&token).get_me()?.username)).await
}

#[derive(Serialize)]
pub struct Contact {
    chat_id: i64,
    user_id: i64,
    name: String,
    /// The chat already holds synced data (so the passphrase must match it).
    has_data: bool,
}

/// Setup step 2: wait (up to 2 minutes) for the user to press Start in the bot's chat.
#[tauri::command]
pub async fn sync_wait_for_start(token: String) -> CmdResult<Contact> {
    blocking(move || {
        let c = Bot::new(&token).wait_for_contact(std::time::Duration::from_secs(120))?;
        let has_data = sync::has_remote(&token, c.chat_id)?;
        Ok(Contact { chat_id: c.chat_id, user_id: c.user_id, name: c.name, has_data })
    })
    .await
}

/// Setup step 3: check the passphrase against what's synced (or start fresh) and save.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn sync_finish_setup(
    state: State<'_, AppState>,
    token: String,
    bot: String,
    chat_id: i64,
    user_id: i64,
    passphrase: String,
    device: String,
) -> CmdResult<()> {
    let paths = state.paths.clone();
    blocking(move || {
        let contact = sync::telegram::Contact { chat_id, user_id, name: String::new() };
        sync::finish_setup(&paths, &token, &bot, &contact, &passphrase, &device).map(drop)
    })
    .await
}

#[derive(Serialize)]
pub struct SyncResult {
    /// e.g. "↓ 3 updated from laptop · ↑ 1 sent".
    line: String,
    skipped: Vec<String>,
    warning: Option<String>,
}

#[tauri::command]
pub async fn sync_now(state: State<'_, AppState>) -> CmdResult<SyncResult> {
    let paths = state.paths.clone();
    blocking(move || {
        let cfg = SyncConfig::load(&paths).ok_or_else(|| anyhow::anyhow!("sync isn't set up on this device"))?;
        let s = sync::sync(&paths, &cfg)?;
        Ok(SyncResult { line: s.line(), skipped: s.skipped, warning: s.warning })
    })
    .await
}

#[tauri::command]
pub fn sync_forget(state: State<'_, AppState>) -> CmdResult<()> {
    SyncConfig::forget(&state.paths).map_err(e)
}

#[derive(Clone, Serialize)]
struct SyncEvent {
    summary: Option<String>,
    error: Option<String>,
}

/// Tray → Sync now: sync in the background and tell the main window how it went.
pub fn sync_from_tray(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let paths = app.state::<AppState>().paths.clone();
        let result = SyncConfig::load(&paths)
            .ok_or_else(|| anyhow::anyhow!("sync isn't set up on this device"))
            .and_then(|cfg| sync::sync(&paths, &cfg));
        let event = match result {
            Ok(s) => SyncEvent { summary: Some(s.line()), error: None },
            Err(err) => SyncEvent { summary: None, error: Some(e(err)) },
        };
        let _ = app.emit("sync-finished", event);
    });
}
