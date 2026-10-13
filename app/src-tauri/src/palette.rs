//! Global-hotkey command palette window.

use std::collections::BTreeMap;

use serde::Serialize;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::state::AppState;

/// Alt+Space opens the window menu on Windows and GNOME, so other platforms add Ctrl.
pub const DEFAULT_HOTKEY: &str = if cfg!(target_os = "macos") { "Alt+Space" } else { "Ctrl+Alt+Space" };

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle(app);
            }
        })
        .build()
}

pub fn toggle<R: Runtime>(app: &AppHandle<R>) {
    let Some(w) = app.get_webview_window("palette") else { return };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
    } else {
        let _ = w.center();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit_to("palette", "palette-open", ());
    }
}

/// Replace the registered hotkey.
pub fn register_hotkey<R: Runtime>(app: &AppHandle<R>, hotkey: &str) -> Result<(), String> {
    let shortcut: Shortcut = hotkey.parse().map_err(|e| format!("invalid shortcut `{hotkey}`: {e}"))?;
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    gs.register(shortcut).map_err(|e| format!("could not register `{hotkey}` (is another app using it?): {e}"))
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[derive(Clone, Serialize)]
pub struct RunRequestEvent {
    pub id: String,
    pub params: Option<BTreeMap<String, String>>,
}

/// Ask the main window to run a bookmark in a terminal tab (it handles params and sizing).
pub fn request_run<R: Runtime>(app: &AppHandle<R>, id: String, params: Option<BTreeMap<String, String>>) {
    show_main(app);
    let _ = app.emit_to("main", "request-run", RunRequestEvent { id, params });
}

#[tauri::command]
pub fn hotkey_error(state: State<'_, AppState>) -> Option<String> {
    state.hotkey_error.lock().expect("hotkey lock").clone()
}

#[tauri::command]
pub fn set_hotkey(app: AppHandle, state: State<'_, AppState>, hotkey: String) -> Result<(), String> {
    register_hotkey(&app, &hotkey)?;
    *state.hotkey_error.lock().expect("hotkey lock") = None;
    let mut settings = state.library().settings;
    settings.hotkey = Some(hotkey);
    bk_core::store::write_settings(&state.paths.config_file, &settings).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn hide_palette(app: AppHandle) {
    if let Some(w) = app.get_webview_window("palette") {
        let _ = w.hide();
    }
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) {
    show_main(&app);
}

/// From the palette or tray: open an SSH session in the main window.
pub fn request_connect<R: Runtime>(app: &AppHandle<R>, name: String) {
    show_main(app);
    let _ = app.emit_to("main", "request-connect", name);
}

#[tauri::command]
pub fn palette_connect(app: AppHandle, name: String) {
    hide_palette(app.clone());
    request_connect(&app, name);
}

/// From the palette: run in the app (main window opens a tab).
#[tauri::command]
pub fn palette_run(app: AppHandle, id: String, params: BTreeMap<String, String>) {
    hide_palette(app.clone());
    request_run(&app, id, Some(params));
}

#[tauri::command]
pub fn set_dock_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    app.set_dock_visibility(visible).map_err(|e| e.to_string())?;
    let _ = (&app, visible);
    Ok(())
}
