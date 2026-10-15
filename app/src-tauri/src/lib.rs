mod autosync;
mod commands;
mod external_term;
mod folders_cmds;
#[cfg(windows)]
mod job;
mod keys_cmds;
mod markers;
mod palette;
mod runs;
mod servers_cmds;
mod state;
mod sync_cmds;
mod tray;
mod tunnels;
mod updater;
mod watcher;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Listener, Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use state::AppState;
use watcher::FileWatcher;

/// Set once the user confirmed quitting while runs are active.
static QUIT_CONFIRMED: AtomicBool = AtomicBool::new(false);

/// Quitting is settled (an update is restarting the app): don't ask about runs.
pub fn confirm_quit() {
    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
}

/// Quit, asking first if runs are still going.
pub fn request_quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.runs.any_running() {
        QUIT_CONFIRMED.store(true, Ordering::SeqCst);
        app.exit(0);
        return;
    }
    palette::show_main(app);
    let handle = app.clone();
    app.dialog()
        .message("Commands are still running. Quitting will stop them.")
        .title("Quit Clibooker?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Stop and Quit".into(), "Cancel".into()))
        .show(move |ok| {
            if ok {
                QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                handle.state::<AppState>().runs.stop_all();
                handle.exit(0);
            }
        });
}

/// `Clibooker --palette` opens the quick-run palette (for desktops where global shortcuts
/// don't work, e.g. Wayland: bind a system shortcut to this command).
fn wants_palette<S: AsRef<str>>(args: &[S]) -> bool {
    args.iter().any(|a| a.as_ref() == "--palette")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMA-BUF renderer shows a blank window on many NVIDIA setups.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
        && std::path::Path::new("/proc/driver/nvidia/version").exists()
    {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if wants_palette(&args) { palette::toggle(app) } else { palette::show_main(app) }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().with_denylist(&["palette"]).build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(palette::plugin())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::new())
        .setup(|app| {
            let handle = app.handle().clone();
            // The login-shell env takes ~0.1–1s; capture it off the main thread.
            std::thread::spawn(move || {
                let result = handle.state::<AppState>().load_base_env();
                let _ = handle.emit("shell-env-ready", result.err());
            });

            let state = app.state::<AppState>();
            match FileWatcher::start(
                app.handle().clone(),
                state.paths.config_file.clone(),
                state.paths.data_dir.clone(),
            ) {
                Ok(w) => {
                    w.watch_projects(&state.project_files());
                    app.manage::<Arc<FileWatcher>>(w);
                }
                Err(e) => eprintln!("file watcher unavailable: {e}"),
            }

            autosync::start(app.handle());
            updater::start(app.handle());
            commands::refresh_cli_copy(&state);

            if let Err(e) = tray::create(app.handle()) {
                eprintln!("no tray icon ({e}); closing the window will quit");
            }
            for event in ["library-changed", "history-changed", "run-updated"] {
                let h = app.handle().clone();
                app.listen_any(event, move |_| tray::refresh(&h));
            }

            let settings = state.library().settings;
            let hotkey = settings.hotkey.clone().unwrap_or_else(|| palette::DEFAULT_HOTKEY.into());
            if let Err(e) = palette::register_hotkey(app.handle(), &hotkey) {
                eprintln!("{e}");
                *state.hotkey_error.lock().expect("hotkey lock") = Some(e);
            }
            #[cfg(target_os = "macos")]
            if settings.show_in_dock == Some(false) {
                let _ = app.handle().set_dock_visibility(false);
            }
            if wants_palette(&std::env::args().collect::<Vec<_>>()) {
                palette::toggle(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                let app = window.app_handle();
                let keep = app.state::<AppState>().library().settings.keep_running.unwrap_or(true);
                if keep && tray::available() {
                    // Keep running in the menu bar / tray; runs continue in the background.
                    let _ = window.hide();
                } else {
                    request_quit(app);
                }
            }
            WindowEvent::Focused(false) if window.label() == "palette" => {
                let _ = window.hide();
            }
            // Back to the app: maybe another device sent something meanwhile.
            WindowEvent::Focused(true) if window.label() == "main" => autosync::nudge(window.app_handle()),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_library,
            commands::save_bookmark,
            commands::delete_bookmark,
            commands::set_pinned,
            commands::get_inventory,
            commands::scan_folder,
            commands::preview_bookmark,
            commands::one_liner,
            commands::save_settings,
            commands::reload_shell_env,
            commands::get_history,
            commands::add_project,
            commands::remove_project,
            commands::start_run,
            commands::list_runs,
            commands::attach_run,
            commands::detach_run,
            commands::write_run,
            commands::resize_run,
            commands::stop_run,
            commands::remove_run,
            commands::detach_tmux,
            commands::run_doctor,
            commands::get_inbox,
            commands::clear_inbox,
            commands::trust_project,
            commands::cli_status,
            commands::install_cli,
            commands::scan_scripts,
            keys_cmds::keys_list,
            keys_cmds::keys_suggest_name,
            keys_cmds::keys_import,
            keys_cmds::keys_rename,
            keys_cmds::keys_remove,
            keys_cmds::keys_lock,
            keys_cmds::keys_generate,
            keys_cmds::keys_unlock,
            keys_cmds::keys_passphrase,
            keys_cmds::keys_install,
            servers_cmds::list_servers,
            servers_cmds::save_server,
            servers_cmds::delete_server,
            servers_cmds::hide_server,
            servers_cmds::check_server,
            servers_cmds::server_inventory,
            servers_cmds::host_fingerprints,
            servers_cmds::trust_host,
            servers_cmds::connect_server,
            servers_cmds::shell_here,
            servers_cmds::scan_remote_folder,
            sync_cmds::sync_status,
            sync_cmds::sync_check_token,
            sync_cmds::sync_wait_for_start,
            sync_cmds::sync_finish_setup,
            sync_cmds::sync_now,
            sync_cmds::sync_forget,
            sync_cmds::sync_set_auto,
            folders_cmds::folders_list,
            folders_cmds::folders_set,
            folders_cmds::folders_remove,
            folders_cmds::locate_folder,
            external_term::open_in_terminal,
            palette::set_hotkey,
            palette::hotkey_error,
            palette::hide_palette,
            palette::show_main_window,
            palette::palette_run,
            palette::palette_connect,
            palette::set_dock_visible,
            updater::update_status,
            updater::update_check,
            updater::update_install,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Clibooker");

    app.run(|app, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => palette::show_main(app),
        RunEvent::ExitRequested { api, code, .. } => {
            let restarting = code == Some(tauri::RESTART_EXIT_CODE);
            if !restarting && !QUIT_CONFIRMED.load(Ordering::SeqCst) && app.state::<AppState>().runs.any_running() {
                api.prevent_exit();
                request_quit(app);
            } else {
                app.state::<AppState>().runs.stop_all();
            }
        }
        _ => {}
    });
}
