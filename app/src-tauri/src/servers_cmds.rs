//! Tauri commands for SSH servers: list/edit, connection status, discovery, host keys,
//! interactive sessions, and remote folder hints for the editor.

use bk_core::remote::{self, RemoteInputs};
use bk_core::servers::{self, Batch, Reach, RemoteInventory, Source, Target};
use bk_core::{Server, paths, store};
use serde::Serialize;
use std::collections::BTreeMap;
use tauri::{AppHandle, State};

use crate::runs::{RunInfo, Spawn, StartOptions};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn e(err: impl std::fmt::Display) -> String {
    format!("{err:#}")
}

#[derive(Serialize)]
pub struct ServerDto {
    name: String,
    source: Source,
    /// `user@host:port` as shown to the user.
    destination: String,
    description: Option<String>,
    /// Editable definition (Clibooker servers only).
    server: Option<Server>,
    status: Option<Reach>,
    /// When the server was last scanned for envs/runtimes (ms), if ever.
    scanned_ms: Option<i64>,
    /// Bookmarks that run on it.
    bookmarks: usize,
}

fn program(state: &AppState) -> String {
    servers::ssh_program(state.library().settings.ssh.as_deref())
}

fn batch<'a>(state: &'a AppState, target: &'a Target) -> Batch<'a> {
    Batch { program: program(state), target, paths: &state.paths, env: servers::ssh_env(&state.base_env()) }
}

fn target(state: &AppState, name: &str) -> CmdResult<Target> {
    servers::resolve_target(name, &state.library().servers).map_err(e)
}

#[tauri::command]
pub fn list_servers(state: State<'_, AppState>) -> Vec<ServerDto> {
    let lib = state.library();
    let statuses = state.server_status.lock().expect("status lock").clone();
    let used = |name: &str| lib.entries.iter().filter(|x| x.bookmark.server.as_deref() == Some(name)).count();
    let mut out: Vec<ServerDto> = lib
        .servers
        .iter()
        .map(|(name, s)| ServerDto {
            name: name.clone(),
            source: Source::Clibooker,
            destination: format!(
                "{}{}{}",
                s.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(),
                s.host,
                s.port.map(|p| format!(":{p}")).unwrap_or_default()
            ),
            description: s.description.clone(),
            server: Some(s.clone()),
            status: statuses.get(name).cloned(),
            scanned_ms: servers::load_cached(&state.paths, name).map(|i| i.probed_ms),
            bookmarks: used(name),
        })
        .collect();
    for alias in servers::listed_config_hosts(&lib.servers, &lib.settings.hidden_servers) {
        out.push(ServerDto {
            name: alias.clone(),
            source: Source::SshConfig,
            destination: alias.clone(),
            description: Some("from ~/.ssh/config".into()),
            server: None,
            status: statuses.get(&alias).cloned(),
            scanned_ms: servers::load_cached(&state.paths, &alias).map(|i| i.probed_ms),
            bookmarks: used(&alias),
        });
    }
    out
}

#[tauri::command]
pub fn save_server(
    state: State<'_, AppState>,
    original_name: Option<String>,
    name: String,
    server: Server,
) -> CmdResult<()> {
    let file = state.paths.config_file.clone();
    let existing = store::read_file(&file).map_err(e)?;
    if original_name.as_deref() != Some(name.as_str()) && existing.servers.contains_key(&name) {
        return Err(format!("a server named `{name}` already exists"));
    }
    store::upsert_server(&file, &name, &server).map_err(e)?;
    if let Some(old) = original_name.filter(|o| *o != name) {
        store::remove_server(&file, &old).map_err(e)?;
    }
    state.server_status.lock().expect("status lock").remove(&name);
    Ok(())
}

#[tauri::command]
pub fn delete_server(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    if !store::remove_server(&state.paths.config_file, &name).map_err(e)? {
        return Err(format!("no Clibooker server named `{name}`"));
    }
    servers::forget_cached(&state.paths, &name);
    state.server_status.lock().expect("status lock").remove(&name);
    Ok(())
}

/// Hide an `~/.ssh/config` host from the lists (or show it again). ~/.ssh/config is never edited.
#[tauri::command]
pub fn hide_server(state: State<'_, AppState>, name: String, hidden: bool) -> CmdResult<()> {
    store::set_server_hidden(&state.paths.config_file, &name, hidden).map_err(e)?;
    Ok(())
}

/// Connection check (BatchMode, never prompts). Cached for the Servers list.
#[tauri::command]
pub async fn check_server(state: State<'_, AppState>, name: String) -> CmdResult<Reach> {
    let t = target(&state, &name)?;
    let b = batch(&state, &t);
    let reach = tauri::async_runtime::spawn_blocking({
        let (program, paths, env, t) = (b.program.clone(), state.paths.clone(), b.env.clone(), t.clone());
        move || servers::check(&Batch { program, target: &t, paths: &paths, env })
    })
    .await
    .map_err(e)?;
    state.server_status.lock().expect("status lock").insert(name, reach.clone());
    Ok(reach)
}

/// What's installed on the server (conda envs, node/java/... versions, tmux). Cached on disk.
#[tauri::command]
pub async fn server_inventory(state: State<'_, AppState>, name: String, refresh: bool) -> CmdResult<RemoteInventory> {
    let t = target(&state, &name)?;
    if !refresh && let Some(inv) = servers::load_cached(&state.paths, &t.name) {
        return Ok(inv);
    }
    let (program, paths, env) = (program(&state), state.paths.clone(), servers::ssh_env(&state.base_env()));
    tauri::async_runtime::spawn_blocking(move || {
        servers::inventory(&Batch { program, target: &t, paths: &paths, env }, refresh)
    })
    .await
    .map_err(e)?
    .map_err(e)
}

#[tauri::command]
pub async fn host_fingerprints(state: State<'_, AppState>, name: String) -> CmdResult<Vec<String>> {
    let t = target(&state, &name)?;
    let (program, env) = (program(&state), servers::ssh_env(&state.base_env()));
    tauri::async_runtime::spawn_blocking(move || servers::host_fingerprints(&program, &t, &env))
        .await
        .map_err(e)?
        .map_err(e)
}

/// The user checked the fingerprint: record the host key (never accepts a changed one).
#[tauri::command]
pub async fn trust_host(state: State<'_, AppState>, name: String) -> CmdResult<Reach> {
    let t = target(&state, &name)?;
    let (program, paths, env) = (program(&state), state.paths.clone(), servers::ssh_env(&state.base_env()));
    let reach = tauri::async_runtime::spawn_blocking(move || {
        servers::accept_host_key(&Batch { program, target: &t, paths: &paths, env })
    })
    .await
    .map_err(e)?;
    state.server_status.lock().expect("status lock").insert(name, reach.clone());
    Ok(reach)
}

fn session_opts(state: &AppState, cols: u16, rows: u16) -> StartOptions {
    StartOptions {
        cols,
        rows,
        background: false,
        notify: false,
        record_history: false,
        history_file: state.paths.history_file(),
    }
}

/// Interactive login session in a terminal tab.
#[tauri::command]
pub fn connect_server(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    cols: u16,
    rows: u16,
) -> CmdResult<RunInfo> {
    let t = target(&state, &name)?;
    let program = program(&state);
    let env = servers::ssh_env(&state.base_env());
    let args = remote::session_args(&program, &t, &state.paths);
    let spawn = Spawn::session(&t.name, &program, args, &t, env.clone(), t.forward.clone());
    let ctl = crate::runs::RemoteCtl { program, target: t, paths: state.paths.clone(), env };
    state.runs.start(app, spawn, session_opts(&state, cols, rows), Some(&ctl)).map_err(e)
}

/// A shell on the server in a remote bookmark's folder and env.
#[tauri::command]
pub fn shell_here(app: AppHandle, state: State<'_, AppState>, id: String, cols: u16, rows: u16) -> CmdResult<RunInfo> {
    let lib = state.library();
    let entry = lib.by_id(&id).ok_or("bookmark not found")?;
    if !bk_core::trust::is_trusted(&state.paths, &entry.file) {
        return Err(format!("{} is not trusted yet", paths::contract_tilde(&entry.file)));
    }
    let base = state.base_env();
    let inp = RemoteInputs {
        servers: &lib.servers,
        paths: &state.paths,
        ssh_program: program(&state),
        base_env: &base,
        online: false,
        markers: false,
    };
    let (t, args) = remote::shell_here(entry, &inp).map_err(e)?;
    let env = servers::ssh_env(&base);
    let spawn = Spawn::session(&format!("{} (shell)", entry.name), &inp.ssh_program, args, &t, env, Vec::new());
    state.runs.start(app, spawn, session_opts(&state, cols, rows), None).map_err(e)
}

#[derive(Serialize)]
pub struct RemoteFolder {
    /// Files found among the ones we look for (.venv, uv.lock, .nvmrc, package.json, ...).
    files: Vec<String>,
    /// Pins found in version files: lang → version.
    runtime_hints: BTreeMap<String, String>,
    scripts: Vec<bk_core::scripts::ProjectScript>,
}

/// Peek at a folder on the server: env markers, version pins and project scripts.
#[tauri::command]
pub async fn scan_remote_folder(state: State<'_, AppState>, name: String, dir: String) -> CmdResult<RemoteFolder> {
    let t = target(&state, &name)?;
    let (program, paths, env) = (program(&state), state.paths.clone(), servers::ssh_env(&state.base_env()));
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<RemoteFolder> {
        let b = Batch { program, target: &t, paths: &paths, env };
        let mut wanted: Vec<&str> = bk_core::env::runtimes::VERSION_FILES.to_vec();
        wanted.extend(bk_core::scripts::SCRIPT_FILES);
        wanted.extend(["environment.yml"]);
        wanted.sort();
        wanted.dedup();
        let folder = servers::fetch_folder(&b, &dir, &wanted)?;
        let files = folder.files;
        let read = |f: &str| files.get(f).cloned();
        let exists = |f: &str| folder.names.iter().any(|n| n == f);
        let runtime_hints = bk_core::env::runtimes::SUPPORTED
            .iter()
            .filter_map(|l| bk_core::env::runtimes::hint_in(l, &read).map(|(v, _)| (l.to_string(), v)))
            .collect();
        Ok(RemoteFolder {
            scripts: bk_core::scripts::discover_with(&read, &exists),
            files: folder.names.clone(),
            runtime_hints,
        })
    })
    .await
    .map_err(e)?
    .map_err(e)
}
