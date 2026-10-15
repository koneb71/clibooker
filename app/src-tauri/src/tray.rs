//! Menu-bar icon with pinned/recent bookmarks and running jobs.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use bk_core::history;
use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::palette;
use crate::runs::Status;
use crate::state::AppState;

const TRAY_ID: &str = "main";
/// macOS menu bars want a black template glyph; Windows taskbars and Linux panels a colored icon.
#[cfg(target_os = "macos")]
const ICON: &[u8] = include_bytes!("../icons/tray-template.png");
#[cfg(not(target_os = "macos"))]
const ICON: &[u8] = include_bytes!("../icons/32x32.png");

static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether the tray icon exists (Linux desktops without an AppIndicator host have none).
pub fn available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON)?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Clibooker")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle(app, event.id().as_ref()))
        .build(app)?;
    AVAILABLE.store(true, Ordering::Relaxed);
    Ok(())
}

pub fn refresh(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let state = app.state::<AppState>();
    let lib = state.library();
    let hotkey = lib.settings.hotkey.clone().unwrap_or_else(|| palette::DEFAULT_HOTKEY.into());

    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = vec![
        Box::new(MenuItem::with_id(app, "open", "Open Clibooker", true, None::<&str>)?),
        Box::new(MenuItem::with_id(app, "palette", "Quick Run…", true, Some(hotkey.as_str()))?),
    ];

    let pinned: Vec<_> = lib.entries.iter().filter(|e| e.bookmark.pinned && !lib.is_shadowed(e)).collect();
    if !pinned.is_empty() {
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
        items.push(Box::new(MenuItem::with_id(app, "h-pinned", "Pinned", false, None::<&str>)?));
        for e in pinned.iter().take(10) {
            items.push(Box::new(MenuItem::with_id(app, format!("run:{}", e.id), &e.name, true, None::<&str>)?));
        }
    }

    let mut seen = HashSet::new();
    let recent: Vec<_> = history::read(&state.paths.history_file(), 200, None)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| lib.by_id(&r.entry_id).is_some() && seen.insert(r.entry_id.clone()))
        .take(5)
        .collect();
    if !recent.is_empty() {
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
        items.push(Box::new(MenuItem::with_id(app, "h-recent", "Recent", false, None::<&str>)?));
        for r in recent {
            let mark = match r.exit_code {
                Some(0) => "✓",
                Some(_) => "✗",
                None => "·",
            };
            let label = format!("{mark} {}", r.name);
            items.push(Box::new(MenuItem::with_id(app, format!("run:{}", r.entry_id), label, true, None::<&str>)?));
        }
    }

    let running: Vec<_> = state.runs.list().into_iter().filter(|r| r.status == Status::Running).collect();
    if !running.is_empty() {
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
        items.push(Box::new(MenuItem::with_id(app, "h-running", "Running", false, None::<&str>)?));
        for r in running {
            let step = if r.total_steps > 1 { format!(" ({}/{})", r.step, r.total_steps) } else { String::new() };
            let label = format!("● {}{step} — {}", r.name, truncate(&r.step_label, 40));
            items.push(Box::new(MenuItem::with_id(app, format!("show:{}", r.id), label, true, None::<&str>)?));
        }
    }

    let mut server_names: Vec<String> = lib.servers.keys().cloned().collect();
    server_names.extend(bk_core::servers::listed_config_hosts(&lib.servers, &lib.settings.hidden_servers));
    if !server_names.is_empty() {
        let subs: Vec<MenuItem<Wry>> = server_names
            .iter()
            .take(20)
            .map(|n| MenuItem::with_id(app, format!("connect:{n}"), n, true, None::<&str>))
            .collect::<tauri::Result<_>>()?;
        let refs: Vec<&dyn IsMenuItem<Wry>> = subs.iter().map(|m| m as &dyn IsMenuItem<Wry>).collect();
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
        items.push(Box::new(Submenu::with_items(app, "Connect to", true, &refs)?));
    }

    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    if let Some(version) = crate::updater::available(app) {
        let label = format!("Update to {version}…");
        items.push(Box::new(MenuItem::with_id(app, "update", label, true, None::<&str>)?));
    }
    if bk_core::sync::SyncConfig::load(&app.state::<AppState>().paths).is_some() {
        items.push(Box::new(MenuItem::with_id(app, "sync", "Sync Now", true, None::<&str>)?));
    }
    items.push(Box::new(MenuItem::with_id(app, "quit", "Quit Clibooker", true, Some("CmdOrCtrl+Q"))?));
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

fn handle(app: &AppHandle, id: &str) {
    match id {
        "open" => palette::show_main(app),
        "palette" => palette::toggle(app),
        "quit" => crate::request_quit(app),
        "sync" => crate::sync_cmds::sync_from_tray(app),
        "update" => {
            palette::show_main(app);
            let _ = app.emit_to("main", "show-updates", ());
        }
        _ => {
            if let Some(entry_id) = id.strip_prefix("run:") {
                palette::request_run(app, entry_id.to_string(), None);
            } else if let Some(name) = id.strip_prefix("connect:") {
                palette::request_connect(app, name.to_string());
            } else if let Some(run_id) = id.strip_prefix("show:") {
                palette::show_main(app);
                let _ = app.emit_to("main", "focus-run", run_id.to_string());
            }
        }
    }
}
