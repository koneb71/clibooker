//! Tauri commands: the API the web UI talks to.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use bk_core::env::{Mode, discover};
use bk_core::history::{self, RunRecord};
use bk_core::plan::{self, PlanSummary, RunRequest};
use bk_core::remote::{self, RemoteInputs};
use bk_core::template::{self, Param};
use bk_core::{Bookmark, Entry, Scope, Settings, paths, projects, render, store, trust};
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};

use tauri::{AppHandle, Manager, State};

use crate::runs::{RemoteCtl, RunInfo, Spawn, StartOptions};
use crate::state::AppState;
use crate::watcher::FileWatcher;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    // anyhow's alternate format includes the cause chain.
    format!("{e:#}")
}

fn anyhow_err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
pub struct EntryDto {
    id: String,
    name: String,
    scope: Scope,
    file: String,
    /// Project directory for project bookmarks.
    project_dir: Option<String>,
    /// Resolved working directory (display form, with `~`).
    cwd: Option<String>,
    bookmark: Bookmark,
    params: Vec<Param>,
    summary: String,
    shadowed: bool,
    /// Project files must be approved before their bookmarks run.
    trusted: bool,
    last_run: Option<RunRecord>,
}

#[derive(Serialize)]
pub struct ProjectDto {
    file: String,
    dir: String,
    name: String,
}

#[derive(Serialize)]
pub struct LibraryDto {
    settings: Settings,
    entries: Vec<EntryDto>,
    errors: Vec<store::LoadError>,
    global_file: String,
    home: String,
    projects: Vec<ProjectDto>,
}

fn entry_dto(e: &Entry, shadowed: bool, trusted: bool, last_run: Option<RunRecord>) -> EntryDto {
    EntryDto {
        id: e.id.clone(),
        name: e.name.clone(),
        scope: e.scope.clone(),
        file: e.file.display().to_string(),
        project_dir: (e.scope == Scope::Project).then(|| paths::contract_tilde(&e.base_dir)),
        cwd: e.cwd().map(|c| paths::contract_tilde(&c)),
        bookmark: e.bookmark.clone(),
        params: template::bookmark_params(&e.bookmark),
        summary: e.bookmark.summary(),
        shadowed,
        trusted,
        last_run,
    }
}

#[tauri::command]
pub fn get_library(app: AppHandle, state: State<'_, AppState>) -> LibraryDto {
    if let Some(w) = app.try_state::<Arc<FileWatcher>>() {
        w.watch_projects(&state.project_files());
    }
    let lib = state.library();
    let mut last: HashMap<String, RunRecord> = HashMap::new();
    for r in history::read(&state.paths.history_file(), 5000, None).unwrap_or_default() {
        last.entry(r.entry_id.clone()).or_insert(r);
    }
    let entries = lib
        .entries
        .iter()
        .map(|e| {
            let trusted = trust::is_trusted(&state.paths, &e.file);
            entry_dto(e, lib.is_shadowed(e), trusted, last.get(&e.id).cloned())
        })
        .collect();
    let projects = state
        .project_files()
        .into_iter()
        .map(|f| {
            let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
            ProjectDto {
                file: f.display().to_string(),
                name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                dir: paths::contract_tilde(&dir),
            }
        })
        .collect();
    LibraryDto {
        settings: lib.settings.clone(),
        entries,
        errors: lib.errors.clone(),
        global_file: state.paths.config_file.display().to_string(),
        home: paths::home().display().to_string(),
        projects,
    }
}

/// `file` = None means the global file.
fn target_file(state: &AppState, file: Option<String>) -> PathBuf {
    file.map(PathBuf::from).unwrap_or_else(|| state.paths.config_file.clone())
}

/// Create or update a bookmark; `original_name` handles renames. Returns the new id.
#[tauri::command]
pub fn save_bookmark(
    state: State<'_, AppState>,
    file: Option<String>,
    original_name: Option<String>,
    name: String,
    bookmark: Bookmark,
) -> CmdResult<String> {
    let file = target_file(&state, file);
    store::validate_name(&name).map_err(anyhow_err)?;
    bookmark.validate()?;
    let existing = store::read_file(&file).map_err(anyhow_err)?;
    match original_name.as_deref() {
        Some(old) if old != name && existing.bookmarks.contains_key(&name) => {
            return Err(format!("`{name}` already exists"));
        }
        None if existing.bookmarks.contains_key(&name) => return Err(format!("`{name}` already exists")),
        _ => {}
    }
    trust::write_preserving(&state.paths, &file, || {
        if let Some(old) = original_name.as_deref().filter(|o| *o != name) {
            store::rename(&file, old, &name)?;
        }
        store::upsert(&file, &name, &bookmark)
    })
    .map_err(anyhow_err)?;
    Ok(format!("{}#{}", file.display(), name))
}

#[tauri::command]
pub fn delete_bookmark(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let lib = state.library();
    let e = lib.by_id(&id).ok_or("bookmark not found")?;
    trust::write_preserving(&state.paths, &e.file, || store::remove(&e.file, &e.name)).map_err(anyhow_err)?;
    Ok(())
}

#[tauri::command]
pub fn set_pinned(state: State<'_, AppState>, id: String, pinned: bool) -> CmdResult<()> {
    let lib = state.library();
    let e = lib.by_id(&id).ok_or("bookmark not found")?;
    let mut b = e.bookmark.clone();
    b.pinned = pinned;
    trust::write_preserving(&state.paths, &e.file, || store::upsert(&e.file, &e.name, &b)).map_err(anyhow_err)
}

#[tauri::command]
pub async fn get_inventory(state: State<'_, AppState>) -> CmdResult<discover::Inventory> {
    Ok(discover::inventory(&state.base_env()))
}

#[tauri::command]
pub fn scan_folder(state: State<'_, AppState>, path: String) -> discover::FolderScan {
    discover::scan_folder(&paths::expand_tilde(&path), &state.base_env())
}

#[derive(Serialize)]
pub struct Preview {
    one_liner: Option<String>,
    plan: Option<PlanSummary>,
    error: Option<String>,
    params: Vec<Param>,
}

/// Validate a draft bookmark and show what it would run (never runs activation hooks).
#[tauri::command]
pub async fn preview_bookmark(
    state: State<'_, AppState>,
    file: Option<String>,
    name: String,
    bookmark: Bookmark,
    params: BTreeMap<String, String>,
) -> CmdResult<Preview> {
    let file = target_file(&state, file);
    let scope = if file == state.paths.config_file { Scope::Global } else { Scope::Project };
    let entry = Entry::new(if name.is_empty() { "untitled" } else { &name }, scope, &file, bookmark);
    let declared = template::bookmark_params(&entry.bookmark);
    let mut values = params;
    for p in &declared {
        if !values.contains_key(&p.name) {
            values.insert(p.name.clone(), p.default.clone().unwrap_or_else(|| format!("<{}>", p.name)));
        }
    }
    let base = state.base_env();
    let home = paths::home();
    let mut out = Preview { one_liner: None, plan: None, error: None, params: declared };
    if let Err(e) = entry.bookmark.validate() {
        out.error = Some(e);
        return Ok(out);
    }
    let lib = state.library();
    if entry.bookmark.is_remote() {
        let inp = RemoteInputs {
            servers: &lib.servers,
            paths: &state.paths,
            ssh_program: bk_core::servers::ssh_program(lib.settings.ssh.as_deref()),
            base_env: &base,
            online: false,
            markers: false,
        };
        let req = RunRequest { params: values, default_cwd: home, ..Default::default() };
        match remote::build(&entry, &req, &inp) {
            Ok(p) => {
                out.one_liner = Some(command_line(&p.program, &p.args_with_forwards()));
                out.plan = Some(p.summary());
            }
            Err(e) => out.error = Some(anyhow_err(e)),
        }
        return Ok(out);
    }
    out.one_liner = render::one_liner(&entry, &values, &[], &home, &base).ok();
    let req = RunRequest { params: values, default_cwd: home, ..Default::default() };
    match plan::build(&entry, &req, &base, &lib.settings, Mode::Check) {
        Ok(p) => out.plan = Some(p.summary()),
        Err(e) => out.error = Some(anyhow_err(e)),
    }
    Ok(out)
}

#[tauri::command]
pub fn one_liner(state: State<'_, AppState>, id: String, params: BTreeMap<String, String>) -> CmdResult<String> {
    let lib = state.library();
    let e = lib.by_id(&id).ok_or("bookmark not found")?;
    one_liner_for(&state, &lib, e, &params)
}

pub fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .map(|a| render::q(&a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shell text that runs a bookmark: a subshell one-liner, or an `ssh …` command for remote ones.
pub fn one_liner_for(
    state: &AppState,
    lib: &bk_core::Library,
    e: &Entry,
    params: &BTreeMap<String, String>,
) -> CmdResult<String> {
    let base = state.base_env();
    if e.bookmark.is_remote() {
        let inp = RemoteInputs {
            servers: &lib.servers,
            paths: &state.paths,
            ssh_program: bk_core::servers::ssh_program(lib.settings.ssh.as_deref()),
            base_env: &base,
            online: false,
            markers: false,
        };
        let req = RunRequest { params: params.clone(), default_cwd: paths::home(), ..Default::default() };
        let p = remote::build(e, &req, &inp).map_err(anyhow_err)?;
        return Ok(command_line(&p.program, &p.args_with_forwards()));
    }
    render::one_liner(e, params, &[], &paths::home(), &base).map_err(anyhow_err)
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    store::write_settings(&state.paths.config_file, &settings).map_err(anyhow_err)
}

#[tauri::command]
pub async fn reload_shell_env(state: State<'_, AppState>) -> CmdResult<usize> {
    state.load_base_env()
}

#[tauri::command]
pub fn get_history(state: State<'_, AppState>, limit: usize, name: Option<String>) -> CmdResult<Vec<RunRecord>> {
    history::read(&state.paths.history_file(), limit, name.as_deref()).map_err(anyhow_err)
}

#[tauri::command]
pub fn add_project(state: State<'_, AppState>, dir: String) -> CmdResult<String> {
    let dir = paths::expand_tilde(&dir);
    let file = dir.join(paths::PROJECT_FILE);
    // Adding a folder yourself approves its current .bk.toml (new or existing).
    store::touch(&file).map_err(err)?;
    trust::trust(&state.paths, &file).map_err(anyhow_err)?;
    projects::register(&state.paths, &file).map_err(anyhow_err)?;
    Ok(file.display().to_string())
}

#[tauri::command]
pub fn remove_project(state: State<'_, AppState>, file: String) -> CmdResult<()> {
    projects::unregister(&state.paths, Path::new(&file)).map_err(anyhow_err)
}

// ---- runs ----

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn start_run(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    params: BTreeMap<String, String>,
    extra_args: Vec<String>,
    cols: u16,
    rows: u16,
    background: bool,
) -> CmdResult<RunInfo> {
    let lib = state.library();
    let entry = lib.by_id(&id).ok_or("bookmark not found")?.clone();
    if !trust::is_trusted(&state.paths, &entry.file) {
        return Err(format!("{} is not trusted yet", paths::contract_tilde(&entry.file)));
    }
    let missing = template::missing(&template::bookmark_params(&entry.bookmark), &params);
    if !missing.is_empty() {
        return Err(format!("missing value for {}", missing.join(", ")));
    }
    let base = state.base_env();
    let settings = lib.settings.clone();
    let req = RunRequest { params, extra_args, overrides: vec![], default_cwd: paths::home() };
    let notify = entry.bookmark.notify;
    let opts = StartOptions {
        cols,
        rows,
        background,
        notify,
        record_history: lib.settings.history_enabled(),
        history_file: state.paths.history_file(),
    };
    let paths = state.paths.clone();
    let servers = lib.servers.clone();
    // Activation / server discovery can take a moment; keep it off the async runtime.
    let (spawn, ctl) = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<(Spawn, Option<RemoteCtl>)> {
        if entry.bookmark.is_remote() {
            let inp = RemoteInputs {
                servers: &servers,
                paths: &paths,
                ssh_program: bk_core::servers::ssh_program(settings.ssh.as_deref()),
                base_env: &base,
                online: true,
                markers: true,
            };
            let plan = remote::build(&entry, &req, &inp)?;
            let mut spawn = Spawn::remote(&plan, &paths);
            if plan.detach {
                let b = inp.batch(&plan.target);
                match remote::tmux_state(&b, &plan.name)? {
                    remote::TmuxState::NoTmux => anyhow::bail!(
                        "tmux isn't installed on {}; install it or turn off “Keep running on the server”",
                        plan.target.name
                    ),
                    remote::TmuxState::Alive => {
                        spawn.args = remote::detached_attach(&plan, &paths);
                        spawn.tmux = Some((plan.name.clone(), true));
                    }
                    remote::TmuxState::Absent => {
                        spawn.args = remote::detached_start(&plan, &b, &paths)?;
                        spawn.tmux = Some((plan.name.clone(), false));
                    }
                }
            }
            let ctl = spawn.remote.clone();
            Ok((spawn, ctl))
        } else {
            Ok((Spawn::local(&plan::build(&entry, &req, &base, &settings, Mode::Run)?), None))
        }
    })
    .await
    .map_err(err)?
    .map_err(anyhow_err)?;
    state.runs.start(app, spawn, opts, ctl.as_ref()).map_err(anyhow_err)
}

#[tauri::command]
pub fn list_runs(state: State<'_, AppState>) -> Vec<RunInfo> {
    state.runs.list()
}

#[tauri::command]
pub fn attach_run(
    state: State<'_, AppState>,
    run_id: String,
    key: String,
    on_output: Channel<InvokeResponseBody>,
) -> CmdResult<()> {
    state.runs.attach(&run_id, &key, on_output).map_err(anyhow_err)
}

#[tauri::command]
pub fn detach_run(state: State<'_, AppState>, run_id: String, key: String) {
    state.runs.detach(&run_id, &key);
}

#[tauri::command]
pub fn write_run(state: State<'_, AppState>, run_id: String, data: String) -> CmdResult<()> {
    state.runs.write(&run_id, data.as_bytes()).map_err(anyhow_err)
}

#[tauri::command]
pub fn resize_run(state: State<'_, AppState>, run_id: String, cols: u16, rows: u16) -> CmdResult<()> {
    state.runs.resize(&run_id, cols, rows).map_err(anyhow_err)
}

#[tauri::command]
pub fn stop_run(state: State<'_, AppState>, run_id: String) -> CmdResult<()> {
    state.runs.stop(&run_id).map_err(anyhow_err)
}

#[tauri::command]
pub fn detach_tmux(state: State<'_, AppState>, run_id: String) -> CmdResult<()> {
    state.runs.detach_tmux(&run_id).map_err(anyhow_err)
}

#[tauri::command]
pub fn remove_run(state: State<'_, AppState>, run_id: String) -> CmdResult<()> {
    state.runs.remove(&run_id).map_err(anyhow_err)
}

// ---- doctor & inbox ----

#[tauri::command]
pub async fn run_doctor(state: State<'_, AppState>) -> CmdResult<bk_core::doctor::Report> {
    let lib = state.library();
    Ok(bk_core::doctor::check_library(&lib, &state.base_env(), &paths::home(), &state.paths))
}

#[tauri::command]
pub fn get_inbox(state: State<'_, AppState>, limit: usize) -> CmdResult<Vec<bk_core::recent::RecentCommand>> {
    let _ = bk_core::recent::trim(&state.paths.recent_file());
    bk_core::recent::read(&state.paths.recent_file(), limit).map_err(anyhow_err)
}

#[tauri::command]
pub fn trust_project(state: State<'_, AppState>, file: String) -> CmdResult<()> {
    trust::trust(&state.paths, Path::new(&file)).map_err(anyhow_err)
}

#[tauri::command]
pub fn clear_inbox(state: State<'_, AppState>) -> CmdResult<()> {
    bk_core::recent::clear(&state.paths.recent_file()).map_err(anyhow_err)
}

// ---- command-line tool ----

#[derive(Serialize)]
pub struct CliStatus {
    /// `bk` found on the login shell's PATH.
    on_path: Option<String>,
    /// The copy bundled inside this app, if any.
    bundled: Option<String>,
    /// Where "Install" puts the link.
    link: String,
    /// The link exists and points at this app's copy.
    linked: bool,
    /// Whether ~/.local/bin is on the login shell's PATH.
    link_dir_on_path: bool,
}

fn bundled_cli() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.with_file_name("bk");
    p.is_file().then_some(p)
}

fn cli_link() -> PathBuf {
    paths::home().join(".local/bin/bk")
}

#[tauri::command]
pub fn cli_status(state: State<'_, AppState>) -> CliStatus {
    let env = state.base_env();
    let bundled = bundled_cli();
    let link = cli_link();
    let linked = match (std::fs::read_link(&link), &bundled) {
        (Ok(target), Some(b)) => &target == b,
        _ => false,
    };
    let link_dir = link.parent().map(Path::to_path_buf).unwrap_or_default();
    CliStatus {
        on_path: bk_core::env::which_in(&env, "bk").map(|p| paths::contract_tilde(&p)),
        bundled: bundled.map(|p| p.display().to_string()),
        link: paths::contract_tilde(&link),
        linked,
        link_dir_on_path: bk_core::env::path_entries(&env).contains(&link_dir),
    }
}

/// Symlink the bundled `bk` into ~/.local/bin.
#[tauri::command]
pub fn install_cli() -> CmdResult<String> {
    let src = bundled_cli()
        .ok_or("this build doesn't include the bk CLI; install it with `cargo install --path crates/bk-cli`")?;
    let link = cli_link();
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&link).map_err(err)?,
        Ok(_) => {
            return Err(format!("{} already exists and isn't a link from Clibooker", paths::contract_tilde(&link)));
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(&src, &link).map_err(err)?;
    Ok(paths::contract_tilde(&link))
}

// ---- project scripts ----

#[derive(Serialize)]
pub struct ScriptScan {
    dir: String,
    /// Folder name, a good name prefix.
    name: String,
    scripts: Vec<bk_core::scripts::ProjectScript>,
    /// Env and runtimes detected for the folder, to apply to imported bookmarks.
    env: bk_core::EnvSpec,
    runtimes: BTreeMap<String, String>,
    /// The folder's .bk.toml if it has one (or is a registered project).
    project_file: Option<String>,
    /// Set when the folder is on a server.
    server: Option<String>,
}

#[tauri::command]
pub async fn scan_scripts(state: State<'_, AppState>, dir: String, server: Option<String>) -> CmdResult<ScriptScan> {
    if let Some(server) = server.filter(|s| !s.is_empty()) {
        let lib = state.library();
        let target = bk_core::servers::resolve_target(&server, &lib.servers).map_err(anyhow_err)?;
        let (program, paths, env) = (
            bk_core::servers::ssh_program(lib.settings.ssh.as_deref()),
            state.paths.clone(),
            bk_core::servers::ssh_env(&state.base_env()),
        );
        let path = dir.clone();
        let found = tauri::async_runtime::spawn_blocking(move || {
            remote::scan_scripts(&bk_core::servers::Batch { program, target: &target, paths: &paths, env }, &path)
        })
        .await
        .map_err(err)?
        .map_err(anyhow_err)?;
        let name = dir.trim_end_matches('/').rsplit('/').next().unwrap_or("").trim_start_matches('~').to_string();
        return Ok(ScriptScan {
            name: if name.is_empty() { server.clone() } else { name },
            dir,
            scripts: found.scripts,
            env: found.env,
            runtimes: found.runtimes,
            project_file: None,
            server: Some(server),
        });
    }
    let dir = paths::expand_tilde(&dir);
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let env = state.base_env();
    let spec = bk_core::env::detect::from_folder(&dir, &env).unwrap_or_default();
    let runtimes = if matches!(spec, bk_core::EnvSpec::Mise) {
        BTreeMap::new()
    } else {
        // Don't inherit `nvm use` from the app's own environment.
        bk_core::env::detect::runtimes(&bk_core::env::EnvMap::new(), &dir)
    };
    let project_file = Some(dir.join(paths::PROJECT_FILE)).filter(|f| f.is_file()).map(|f| f.display().to_string());
    Ok(ScriptScan {
        name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        dir: paths::contract_tilde(&dir),
        scripts: bk_core::scripts::discover(&dir),
        env: spec,
        runtimes,
        project_file,
        server: None,
    })
}
