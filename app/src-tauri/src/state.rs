use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bk_core::env::{self, EnvMap, login};
use bk_core::{Library, Paths, projects};

use crate::runs::RunManager;

pub struct AppState {
    pub paths: Paths,
    /// Login-shell environment (GUI apps don't inherit the terminal's PATH).
    base_env: RwLock<Option<EnvMap>>,
    pub runs: Arc<RunManager>,
    /// Why the quick-run shortcut couldn't be registered, if it couldn't.
    pub hotkey_error: std::sync::Mutex<Option<String>>,
    /// Last connection check per server.
    pub server_status: std::sync::Mutex<std::collections::HashMap<String, bk_core::servers::Reach>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            paths: Paths::from_env(),
            base_env: RwLock::new(None),
            runs: Arc::new(RunManager::default()),
            hotkey_error: std::sync::Mutex::new(None),
            server_status: Default::default(),
        }
    }

    /// Capture the login shell env; falls back to the process env. Windows apps already get
    /// the user's full environment, so there it is the process env.
    pub fn load_base_env(&self) -> Result<usize, String> {
        if cfg!(windows) {
            let env = env::current();
            let n = env.len();
            *self.base_env.write().expect("env lock") = Some(env);
            return Ok(n);
        }
        self.capture_login_env()
    }

    /// "Reload environment": re-read the login shell, or on Windows the user's environment as
    /// a freshly started program would get it (picks up tools installed since launch).
    pub fn reload_base_env(&self) -> Result<usize, String> {
        #[cfg(windows)]
        {
            let env = fresh_windows_env().map_err(|e| format!("{e:#}"))?;
            let n = env.len();
            *self.base_env.write().expect("env lock") = Some(env);
            Ok(n)
        }
        #[cfg(not(windows))]
        self.capture_login_env()
    }

    fn capture_login_env(&self) -> Result<usize, String> {
        let shell = login::user_shell();
        let result = login::login_env(&shell, Duration::from_secs(8));
        let (env, out) = match result {
            Ok(env) => {
                let n = env.len();
                (env, Ok(n))
            }
            Err(e) => (env::current(), Err(format!("{e:#}"))),
        };
        *self.base_env.write().expect("env lock") = Some(env);
        out
    }

    pub fn base_env(&self) -> EnvMap {
        if let Some(env) = self.base_env.read().expect("env lock").as_ref() {
            return env.clone();
        }
        let _ = self.load_base_env();
        self.base_env.read().expect("env lock").clone().unwrap_or_else(env::current)
    }

    pub fn project_files(&self) -> Vec<PathBuf> {
        projects::list(&self.paths)
    }

    pub fn library(&self) -> Library {
        Library::load(&self.paths, &self.project_files())
    }
}

/// The user's environment from the registry (`CreateEnvironmentBlock`), including volatile
/// variables like APPDATA, as a newly started program would see it.
#[cfg(windows)]
fn fresh_windows_env() -> anyhow::Result<EnvMap> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY};
    use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    let mut block: *mut c_void = std::ptr::null_mut();
    // SAFETY: plain Win32 calls with out-pointers we own; the block is walked only up to its
    // double-NUL terminator and freed with DestroyEnvironmentBlock.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE, &mut token) == 0 {
            anyhow::bail!("OpenProcessToken failed: {}", std::io::Error::last_os_error());
        }
        let ok = CreateEnvironmentBlock(&mut block, token, 0);
        CloseHandle(token);
        if ok == 0 {
            anyhow::bail!("CreateEnvironmentBlock failed: {}", std::io::Error::last_os_error());
        }
        let mut env = EnvMap::new();
        let mut p = block as *const u16;
        loop {
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            if len == 0 {
                break;
            }
            let entry = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            // Entries like `=C:=C:\x` are cmd.exe's per-drive folders, not variables.
            if let Some((k, v)) = entry.split_once('=').filter(|(k, _)| !k.is_empty()) {
                env::set(&mut env, k, v);
            }
            p = p.add(len + 1);
        }
        DestroyEnvironmentBlock(block);
        Ok(env)
    }
}
