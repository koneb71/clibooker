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

    /// Capture the login shell env; falls back to the process env.
    pub fn load_base_env(&self) -> Result<usize, String> {
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
