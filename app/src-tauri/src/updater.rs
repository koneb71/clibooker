//! Updates: the app looks for a newer release on GitHub (10 s after launch, then every six
//! hours, unless Settings turns that off) and installs it when you say so. The updater only
//! installs what Clibooker's release key signed (`plugins.updater` in tauri.conf.json).

use std::sync::Mutex;
use std::time::Duration;

use bk_core::history::now_ms;
use serde::Serialize;
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::state::AppState;

const FIRST: Duration = Duration::from_secs(10);
const EVERY_MS: i64 = 6 * 3600 * 1000;

#[derive(Default)]
pub struct Updates {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    update: Option<Update>,
    last_checked_ms: Option<i64>,
    error: Option<String>,
    checking: bool,
    installing: bool,
    /// The version the UI was told about (it hears about each one once).
    announced: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Available {
    version: String,
    /// RFC 3339.
    date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    current: String,
    available: Option<Available>,
    /// This install can update itself (otherwise: download it from the releases page).
    can_install: bool,
    /// How it was installed: app, nsis, msi, appimage, deb or rpm.
    bundle: Option<&'static str>,
    checking: bool,
    installing: bool,
    last_checked_ms: Option<i64>,
    error: Option<String>,
    /// Looks for updates by itself.
    auto: bool,
    releases_url: String,
}

fn bundle() -> Option<&'static str> {
    Some(match tauri::utils::platform::bundle_type()? {
        BundleType::App | BundleType::Dmg => "app",
        BundleType::Nsis => "nsis",
        BundleType::Msi => "msi",
        BundleType::AppImage => "appimage",
        BundleType::Deb => "deb",
        BundleType::Rpm => "rpm",
    })
}

/// Whether this copy of the app can replace itself: an installed release build. A macOS app
/// opened straight from the download (run from a read-only "translocated" copy) can't.
fn can_install(bundle: Option<&str>, debug: bool, exe: &std::path::Path) -> bool {
    !debug && bundle.is_some() && !exe.to_string_lossy().contains("/AppTranslocation/")
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let inner = app.state::<Updates>().inner.lock().expect("updates lock").snapshot();
    let exe = std::env::current_exe().unwrap_or_default();
    UpdateStatus {
        current: app.package_info().version.to_string(),
        can_install: can_install(bundle(), cfg!(debug_assertions), &exe),
        bundle: bundle(),
        auto: app.state::<AppState>().library().settings.check_updates.unwrap_or(true),
        releases_url: format!("{}/latest", bk_core::update::releases_base()),
        ..inner
    }
}

impl Inner {
    fn snapshot(&self) -> UpdateStatus {
        UpdateStatus {
            current: String::new(),
            available: self.update.as_ref().map(|u| Available {
                version: u.version.clone(),
                date: u.raw_json.get("pub_date").and_then(|d| d.as_str()).map(String::from),
            }),
            can_install: false,
            bundle: None,
            checking: self.checking,
            installing: self.installing,
            last_checked_ms: self.last_checked_ms,
            error: self.error.clone(),
            auto: true,
            releases_url: String::new(),
        }
    }
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit("update-status", status(app));
}

/// The updater, with what must happen before Windows' installer closes the app (it exits the
/// process at once): stop the runs and keep the window where it was.
fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let handle = app.clone();
    app.updater_builder().on_before_exit(move || before_restart(&handle)).build().map_err(|e| format!("{e}"))
}

fn before_restart(app: &AppHandle) {
    use tauri_plugin_window_state::{AppHandleExt, StateFlags};
    crate::confirm_quit();
    app.state::<AppState>().runs.stop_all();
    let _ = app.save_window_state(StateFlags::all());
}

/// Look for an update now. Tells the UI about a new version once.
pub async fn check_now(app: &AppHandle) -> UpdateStatus {
    {
        let updates = app.state::<Updates>();
        let mut inner = updates.inner.lock().expect("updates lock");
        if inner.checking || inner.installing {
            drop(inner);
            return status(app);
        }
        inner.checking = true;
    }
    emit_status(app);
    let result = match updater(app) {
        Ok(u) => u.check().await.map_err(|e| format!("{e}")),
        Err(e) => Err(e),
    };
    let fresh = {
        let updates = app.state::<Updates>();
        let mut inner = updates.inner.lock().expect("updates lock");
        inner.checking = false;
        inner.last_checked_ms = Some(now_ms());
        match result {
            Ok(update) => {
                inner.error = None;
                inner.update = update;
            }
            Err(e) => inner.error = Some(e),
        }
        let version = inner.update.as_ref().map(|u| u.version.clone());
        let fresh = version.is_some() && version != inner.announced;
        if fresh {
            inner.announced = version;
        }
        fresh
    };
    let status = status(app);
    let _ = app.emit("update-status", status.clone());
    if fresh {
        let _ = app.emit("update-available", status.clone());
        crate::tray::refresh(app);
    }
    status
}

/// Check 10 s after launch, then every six hours (by the clock, so a laptop that slept checks
/// when it wakes). Development builds don't.
pub fn start(app: &AppHandle) {
    app.manage(Updates::default());
    if cfg!(debug_assertions) || std::env::var_os("BK_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST);
        loop {
            let auto = app.state::<AppState>().library().settings.check_updates.unwrap_or(true);
            let last = app.state::<Updates>().inner.lock().expect("updates lock").last_checked_ms;
            if auto && last.is_none_or(|t| now_ms() - t >= EVERY_MS) {
                tauri::async_runtime::block_on(check_now(&app));
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    });
}

/// The version to offer in the tray, if there is one.
pub fn available(app: &AppHandle) -> Option<String> {
    let updates = app.try_state::<Updates>()?;
    let inner = updates.inner.lock().expect("updates lock");
    inner.update.as_ref().map(|u| u.version.clone())
}

#[tauri::command]
pub fn update_status(app: AppHandle) -> UpdateStatus {
    status(&app)
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> UpdateStatus {
    check_now(&app).await
}

/// Download the update (reporting `update-progress`), install it, and restart. Runs stop
/// only once it's installed (on Windows, when the installer takes over).
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let update = {
        let updates = app.state::<Updates>();
        let mut inner = updates.inner.lock().expect("updates lock");
        if inner.installing {
            return Err("already updating".into());
        }
        let update = inner.update.clone().ok_or("no update to install; check again")?;
        inner.installing = true;
        inner.error = None;
        update
    };
    let fail = |app: &AppHandle, e: String| {
        let updates = app.state::<Updates>();
        let mut inner = updates.inner.lock().expect("updates lock");
        inner.installing = false;
        inner.error = Some(e.clone());
        drop(inner);
        emit_status(app);
        e
    };
    if !status(&app).can_install {
        return Err(fail(&app, "this copy of Clibooker can't update itself; download the new version".into()));
    }
    emit_status(&app);
    let mut got = 0u64;
    let progress = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                got += chunk as u64;
                let _ = progress.emit("update-progress", (got, total));
            },
            || {},
        )
        .await
        .map_err(|e| fail(&app, format!("downloading the update: {e}")))?;
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|e| format!("{e}"))
        .and_then(|r| r.map_err(|e| format!("{e}")));
    if let Err(e) = installed {
        return Err(fail(&app, format!("installing the update: {e}")));
    }
    before_restart(&app);
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn which_copies_update_themselves() {
        let exe = Path::new("/Applications/Clibooker.app/Contents/MacOS/Clibooker");
        assert!(can_install(Some("app"), false, exe));
        assert!(!can_install(Some("app"), true, exe), "not a development build");
        assert!(!can_install(None, false, exe), "not a bare binary (no installer to update)");
        let translocated =
            Path::new("/private/var/folders/x/AppTranslocation/1234/d/Clibooker.app/Contents/MacOS/Clibooker");
        assert!(!can_install(Some("app"), false, translocated));
        assert!(can_install(Some("deb"), false, Path::new("/usr/bin/Clibooker")));
    }
}
