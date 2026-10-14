//! Settings → This device → Folders, and "Locate…" for a bookmark whose folder isn't here.
//! The mappings live in this device's data folder (`folders.toml`), so the file watcher
//! refreshes the library when they change.

use bk_core::folders::{self, FolderMap, LocateResult};
use bk_core::paths;
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn e(err: impl std::fmt::Display) -> String {
    format!("{err:#}")
}

#[derive(Serialize)]
pub struct FolderRuleDto {
    shared: String,
    local: String,
    /// The local folder is there.
    local_exists: bool,
    /// Bookmarks that reach their folder through it.
    used_by: usize,
}

#[tauri::command]
pub fn folders_list(state: State<'_, AppState>) -> Vec<FolderRuleDto> {
    let map = FolderMap::load(&state.paths);
    let lib = state.library();
    map.rules()
        .iter()
        .map(|r| FolderRuleDto {
            shared: r.shared.clone(),
            local: r.local.clone(),
            local_exists: paths::expand_tilde(&r.local).is_dir(),
            used_by: map.used_by(r, &lib),
        })
        .collect()
}

/// Map `shared` (as bookmarks name it) to `local` on this device. Returns what it replaced.
#[tauri::command]
pub fn folders_set(shared: String, local: String, state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let dir = paths::expand_tilde(&local);
    if !dir.is_dir() {
        return Err(format!("{local} is not a folder on this device"));
    }
    FolderMap::add(&state.paths, &shared, &paths::contract_tilde(&dir)).map_err(e)
}

#[tauri::command]
pub fn folders_remove(shared: String, state: State<'_, AppState>) -> CmdResult<bool> {
    FolderMap::remove(&state.paths, &shared).map_err(e)
}

/// "This bookmark's folder is `dir` on this device."
#[tauri::command]
pub fn locate_folder(id: String, dir: String, state: State<'_, AppState>) -> CmdResult<LocateResult> {
    let lib = state.library();
    let entry = lib.by_id(&id).ok_or("bookmark not found")?;
    folders::locate(&state.paths, &lib, entry, &paths::expand_tilde(&dir)).map_err(e)
}
