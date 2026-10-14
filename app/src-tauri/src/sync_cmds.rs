//! Sync between devices through the user's Telegram bot (Settings → Sync, tray → Sync now).

use bk_core::Os;
use bk_core::sync::{self, AutoState, Summary, SyncConfig, telegram::Bot};
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
    /// Syncs by itself.
    auto: bool,
    /// Syncing right now (automatically).
    syncing: bool,
    /// Changes here not sent yet.
    pending: usize,
    /// Why automatic syncing is failing, if it is.
    last_error: Option<String>,
    devices: Vec<DeviceDto>,
}

#[derive(Serialize)]
pub struct DeviceDto {
    name: String,
    /// `macOS`, `Linux`, `Windows`.
    os: String,
    last_change_ms: i64,
    version: String,
    this: bool,
}

#[tauri::command]
pub fn sync_status(app: AppHandle, state: State<'_, AppState>) -> SyncStatus {
    let cfg = SyncConfig::load(&state.paths);
    let st = sync::State::load(&state.paths);
    let device = cfg.as_ref().map(|c| c.device.clone());
    let devices = st
        .devices
        .iter()
        .map(|(name, d)| DeviceDto {
            name: name.clone(),
            os: Os::parse(&d.os).map(|o| o.label().to_string()).unwrap_or_else(|| d.os.clone()),
            last_change_ms: d.last_change_ms,
            version: d.version.clone(),
            this: Some(name) == device.as_ref(),
        })
        .collect();
    SyncStatus {
        configured: cfg.is_some(),
        bot: cfg.as_ref().map(|c| c.bot_username.clone()),
        auto: cfg.as_ref().is_some_and(|c| c.auto),
        device,
        last_sync_ms: st.last_sync_ms,
        last_error: AutoState::load(&state.paths).failing(&st).map(str::to_string),
        last_summary: st.last_summary,
        default_device: sync::default_device_name(),
        syncing: crate::autosync::syncing(&app),
        pending: sync::pending(&state.paths).map(|p| p.items).unwrap_or(0),
        devices,
    }
}

#[tauri::command]
pub fn sync_set_auto(on: bool, state: State<'_, AppState>) -> CmdResult<()> {
    SyncConfig::set_auto(&state.paths, on).map(drop).map_err(e)
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
pub async fn sync_now(app: AppHandle, state: State<'_, AppState>) -> CmdResult<SyncResult> {
    let paths = state.paths.clone();
    let r = blocking(move || {
        let cfg = SyncConfig::load(&paths).ok_or_else(|| anyhow::anyhow!("sync isn't set up on this device"))?;
        let s = sync::sync(&paths, &cfg)?;
        let _ = AutoState::record(&paths, None);
        Ok(SyncResult { line: s.line(), skipped: s.skipped, warning: s.warning })
    })
    .await?;
    crate::autosync::reset(&app);
    Ok(r)
}

#[tauri::command]
pub fn sync_forget(state: State<'_, AppState>) -> CmdResult<()> {
    SyncConfig::forget(&state.paths).map_err(e)
}

/// How a sync went, for the main window (`sync-finished`).
#[derive(Clone, Serialize)]
pub struct SyncEvent {
    summary: Option<String>,
    error: Option<String>,
    /// Started by automatic syncing (which only speaks up when something arrived).
    auto: bool,
    /// Bookmarks, servers and settings changed here.
    received: usize,
    skipped: Vec<String>,
    warning: Option<String>,
}

impl SyncEvent {
    pub fn done(s: &Summary, auto: bool) -> SyncEvent {
        SyncEvent {
            summary: Some(s.line()),
            error: None,
            auto,
            received: s.received,
            skipped: s.skipped.clone(),
            warning: s.warning.clone(),
        }
    }

    pub fn failed(error: String, auto: bool) -> SyncEvent {
        SyncEvent { summary: None, error: Some(error), auto, received: 0, skipped: Vec::new(), warning: None }
    }
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
            Ok(s) => {
                let _ = AutoState::record(&paths, None);
                crate::autosync::reset(&app);
                SyncEvent::done(&s, false)
            }
            Err(err) => SyncEvent::failed(e(err), false),
        };
        let _ = app.emit("sync-finished", event);
    });
}
