//! Settings → SSH keys, and the key picker in the server editor. Making, unlocking and
//! installing a key open a terminal tab running OpenSSH's own tool: passphrases and passwords
//! are typed there, never into the app.

use bk_core::keys::{self, Agent, KeyInfo, KeyType, NoAgent};
use bk_core::servers::{self, Batch, Reach};
use bk_core::sync::{self, SyncConfig};
use bk_core::{Paths, paths};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::runs::{RunInfo, Spawn, StartOptions, Status, Then};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn e(err: impl std::fmt::Display) -> String {
    format!("{err:#}")
}

#[derive(Serialize)]
pub struct KeyDto {
    #[serde(flatten)]
    info: KeyInfo,
    /// ssh-agent holds it (its passphrase was typed and is remembered).
    unlocked: bool,
    /// Clibooker servers that use it.
    used_by: Vec<String>,
    /// Another device has a different key under this name (as of the last sync).
    clash: bool,
    /// The private key file.
    file: String,
}

#[derive(Serialize)]
pub struct KeysDto {
    keys: Vec<KeyDto>,
    /// Why ssh-agent can't remember passphrases here, and how to start it.
    no_agent: Option<NoAgent>,
    /// Sync is set up, so keys go to your other devices.
    synced: bool,
    folder: String,
}

/// The ssh program and its environment, from settings.
fn ssh(state: &AppState) -> (String, bk_core::env::EnvMap) {
    (servers::ssh_program(state.library().settings.ssh.as_deref()), servers::ssh_env(&state.base_env()))
}

#[tauri::command]
pub async fn keys_list(state: State<'_, AppState>) -> CmdResult<KeysDto> {
    let (program, env) = ssh(&state);
    let (paths, servers) = (state.paths.clone(), state.library().servers);
    tauri::async_runtime::spawn_blocking(move || {
        let agent = keys::agent(&keys::tool(&program, "ssh-add"), &env);
        let clashes = sync::key_clashes(&paths);
        let keys = keys::list(&paths)
            .into_iter()
            .map(|info| KeyDto {
                unlocked: agent.holds(&info.fingerprint),
                used_by: keys::used_by(&servers, &info.name),
                clash: clashes.contains(&info.name),
                file: keys::path(&paths, &info.name).map(|p| p.display().to_string()).unwrap_or_default(),
                info,
            })
            .collect();
        KeysDto {
            keys,
            no_agent: match agent {
                Agent::Unavailable(why) => Some(why),
                Agent::Running(_) => None,
            },
            synced: SyncConfig::load(&paths).is_some(),
            folder: paths::contract_tilde(&keys::dir(&paths)),
        }
    })
    .await
    .map_err(e)
}

/// A name for a key imported from `path` (`id_ed25519` becomes `<device>-ed25519`).
#[tauri::command]
pub fn keys_suggest_name(state: State<'_, AppState>, path: String) -> String {
    keys::suggest_name(&paths::expand_tilde(&path), &sync::this_device(&state.paths))
}

/// Save a key from a file (with its .pub next to it) or pasted text.
#[tauri::command]
pub fn keys_import(
    state: State<'_, AppState>,
    name: String,
    path: Option<String>,
    text: Option<String>,
) -> CmdResult<KeyInfo> {
    let keygen = keys::tool(&ssh(&state).0, "ssh-keygen");
    match (path, text) {
        (Some(p), _) => keys::import_file(&state.paths, &name, &paths::expand_tilde(&p), &keygen),
        (None, Some(t)) => keys::import(&state.paths, &name, &t, None, &keygen),
        (None, None) => return Err("nothing to import".into()),
    }
    .map_err(e)
}

/// Returns the servers that now use the new name.
#[tauri::command]
pub fn keys_rename(state: State<'_, AppState>, from: String, to: String) -> CmdResult<Vec<String>> {
    keys::rename(&state.paths, &from, &to).map_err(e)
}

#[tauri::command]
pub fn keys_remove(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    keys::remove(&state.paths, &name).map(drop).map_err(e)
}

/// Take a key out of ssh-agent (and forget its passphrase in the macOS Keychain).
#[tauri::command]
pub async fn keys_lock(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    let (program, env) = ssh(&state);
    let ssh_add = keys::tool(&program, "ssh-add");
    let args = keys::lock_args(&ssh_add, &state.paths, &name).map_err(e)?;
    let out = tauri::async_runtime::spawn_blocking(move || {
        bk_core::proc::background(&ssh_add).args(&args).env_clear().envs(&env).output()
    })
    .await
    .map_err(e)?
    .map_err(e)?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// The line a finished key task prints about the key.
fn saved_line(paths: &Paths, name: &str) -> String {
    match keys::info(paths, name) {
        Ok(k) => format!("\x1b[32m✓ saved key {name}\x1b[0m \x1b[2m({} {})\x1b[0m", k.kind, k.fingerprint),
        Err(err) => format!("\x1b[31m✗ {err:#}\x1b[0m"),
    }
}

fn start(app: AppHandle, spawn: Spawn, cols: u16, rows: u16) -> CmdResult<RunInfo> {
    let state = app.state::<AppState>();
    state.runs.start(app.clone(), spawn, StartOptions::interactive(&state.paths, cols, rows), None).map_err(e)
}

/// Make a new key with ssh-keygen, in a tab where it asks for the passphrase (unless
/// `passphrase` is off).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn keys_generate(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    comment: Option<String>,
    kind: KeyType,
    passphrase: bool,
    cols: u16,
    rows: u16,
) -> CmdResult<RunInfo> {
    let (program, env) = ssh(&state);
    let comment = comment.filter(|c| !c.trim().is_empty()).unwrap_or_else(|| keys::default_comment(&state.paths));
    let args = keys::generate_args(&state.paths, &name, &comment, kind, passphrase).map_err(e)?;
    let paths = state.paths.clone();
    let key = name.clone();
    let then: Then = Box::new(move |status, print| {
        if status == Status::Succeeded {
            print(&saved_line(&paths, &key));
        }
    });
    let spawn = Spawn::task(
        &format!("New key {name}"),
        &keys::tool(&program, "ssh-keygen"),
        args,
        env,
        "ssh-keygen",
        Some(then),
    );
    start(app, spawn, cols, rows)
}

/// Unlock a key: ssh-add asks for its passphrase once, and the agent (and on macOS, the
/// Keychain) keeps it.
#[tauri::command]
pub fn keys_unlock(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    cols: u16,
    rows: u16,
) -> CmdResult<RunInfo> {
    let (program, env) = ssh(&state);
    let ssh_add = keys::tool(&program, "ssh-add");
    if let Agent::Unavailable(why) = keys::agent(&ssh_add, &env) {
        return Err(why.to_string());
    }
    let args = keys::unlock_args(&ssh_add, &state.paths, &name).map_err(e)?;
    let then: Then = Box::new(move |status, print| {
        if status == Status::Succeeded {
            print("\x1b[32m✓ unlocked: you won't be asked for its passphrase again\x1b[0m");
        }
    });
    let spawn = Spawn::task(&format!("Unlock {name}"), &ssh_add, args, env, "ssh-add", Some(then));
    start(app, spawn, cols, rows)
}

/// Add or change a key's passphrase: ssh-keygen asks for it in a tab.
#[tauri::command]
pub fn keys_passphrase(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    cols: u16,
    rows: u16,
) -> CmdResult<RunInfo> {
    let (program, env) = ssh(&state);
    let args = keys::passphrase_args(&state.paths, &name).map_err(e)?;
    let synced = SyncConfig::load(&state.paths).is_some();
    let then: Then = Box::new(move |status, print| {
        if status == Status::Succeeded && synced {
            print("\x1b[32m✓ your other devices get it at their next sync\x1b[0m");
        }
    });
    let spawn = Spawn::task(
        &format!("Passphrase for {name}"),
        &keys::tool(&program, "ssh-keygen"),
        args,
        env,
        "ssh-keygen",
        Some(then),
    );
    start(app, spawn, cols, rows)
}

/// Put a key on a server: ssh logs in the way it does now (you type a password, usually) and
/// adds the key to `~/.ssh/authorized_keys`. Then the server uses the key, and a check shows
/// whether it logs in with it.
#[tauri::command]
pub fn keys_install(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    server: String,
    cols: u16,
    rows: u16,
) -> CmdResult<RunInfo> {
    let (program, env) = ssh(&state);
    let target = servers::resolve_target(&server, &state.library().servers).map_err(e)?;
    let args = keys::install_args(&program, &target, &state.paths, &name).map_err(e)?;
    let encrypted = keys::info(&state.paths, &name).map_err(e)?.encrypted;
    let handle = app.clone();
    let (ssh_program, key, ssh_env) = (program.clone(), name.clone(), env.clone());
    let then: Then = Box::new(move |status, print| {
        let state = handle.state::<AppState>();
        if status != Status::Succeeded {
            print(&format!(
                "\x1b[33mIf {} only accepts keys, add the public key there another way (Settings → SSH keys → Copy public key).\x1b[0m",
                target.name
            ));
            return;
        }
        match keys::after_install(&state.paths, &target.name, &key) {
            Ok(true) => print(&format!("\x1b[32m✓ {} now uses key {key}\x1b[0m", target.name)),
            Ok(false) => print(&format!(
                "\x1b[33m{} isn't a Clibooker server: point its entry in ~/.ssh/config at the key to use it.\x1b[0m",
                target.name
            )),
            Err(err) => print(&format!("\x1b[31m✗ {err:#}\x1b[0m")),
        }
        print("\x1b[2mchecking that it logs in with the key…\x1b[0m");
        let mut with_key = target.clone();
        with_key.identity = Some(format!("{}{key}", keys::PREFIX));
        let reach =
            servers::check(&Batch { program: ssh_program, target: &with_key, paths: &state.paths, env: ssh_env });
        let line = match &reach {
            Reach::Reachable { .. } => "\x1b[32m✓ logs in with the key\x1b[0m",
            Reach::AuthRequired if encrypted => {
                "\x1b[33mIt has a passphrase: unlock it (Settings → SSH keys) so background checks can use it.\x1b[0m"
            }
            Reach::AuthRequired => "\x1b[33m✗ it doesn't log in with the key yet\x1b[0m",
            Reach::UnknownHostKey => "\x1b[33mIts host key isn't trusted yet: connect once to check it.\x1b[0m",
            Reach::ChangedHostKey => "\x1b[31m✗ Its host key CHANGED: check ~/.ssh/known_hosts.\x1b[0m",
            Reach::Unreachable { reason } => &format!("\x1b[33m✗ {reason}\x1b[0m"),
        };
        print(line);
        state.server_status.lock().expect("status lock").insert(target.name.clone(), reach);
    });
    let label = format!("ssh {server}");
    let spawn = Spawn::task(&format!("Install {name} on {server}"), &program, args, env, &label, Some(then));
    start(app, spawn, cols, rows)
}
