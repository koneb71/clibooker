mod commands;
mod external_term;
mod markers;
mod palette;
mod runs;
mod servers_cmds;
mod state;
mod tray;
mod tunnels;
mod watcher;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Listener, Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use state::AppState;
use watcher::FileWatcher;

/// Set once the user confirmed quitting while runs are active.
static QUIT_CONFIRMED: AtomicBool = AtomicBool::new(false);

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| palette::show_main(app)))
        .plugin(tauri_plugin_window_state::Builder::default().with_denylist(&["palette"]).build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(palette::plugin())
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

            tray::create(app.handle())?;
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
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                // Keep running in the menu bar; runs continue in the background.
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(false) if window.label() == "palette" => {
                let _ = window.hide();
            }
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
            external_term::open_in_terminal,
            palette::set_hotkey,
            palette::hotkey_error,
            palette::hide_palette,
            palette::show_main_window,
            palette::palette_run,
            palette::palette_connect,
            palette::set_dock_visible,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Clibooker");

    app.run(|app, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => palette::show_main(app),
        RunEvent::ExitRequested { api, .. } => {
            if !QUIT_CONFIRMED.load(Ordering::SeqCst) && app.state::<AppState>().runs.any_running() {
                api.prevent_exit();
                request_quit(app);
            } else {
                app.state::<AppState>().runs.stop_all();
            }
        }
        _ => {}
    });
}
