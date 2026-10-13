//! Registry of known project `.bk.toml` files (so the desktop app can list them).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::paths::{self, PROJECT_FILE, Paths};
use crate::store::atomic_write;

/// Registered project files that still exist.
pub fn list(paths: &Paths) -> Vec<PathBuf> {
    read(paths).into_iter().filter(|p| p.is_file()).collect()
}

fn read(paths: &Paths) -> Vec<PathBuf> {
    fs::read_to_string(paths.projects_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|s| paths::expand_tilde(&s))
        .collect()
}

fn write(paths: &Paths, files: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(&paths.data_dir)?;
    let list: Vec<String> = files.iter().map(|p| paths::contract_tilde(p)).collect();
    atomic_write(&paths.projects_file(), serde_json::to_string_pretty(&list)?.as_bytes())
}

/// Accepts a project directory or its `.bk.toml`. Returns the file path. No-op if already known.
pub fn register(paths: &Paths, path: &Path) -> Result<PathBuf> {
    let file = if path.is_dir() { path.join(PROJECT_FILE) } else { path.to_path_buf() };
    let mut files = read(paths);
    if !files.contains(&file) {
        files.push(file.clone());
        write(paths, &files)?;
    }
    Ok(file)
}

pub fn unregister(paths: &Paths, file: &Path) -> Result<()> {
    let mut files = read(paths);
    files.retain(|f| f != file);
    write(paths, &files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_list_unregister() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { config_file: d.path().join("b.toml"), data_dir: d.path().join("data") };
        let proj = d.path().join("p");
        fs::create_dir_all(&proj).unwrap();
        let f = register(&paths, &proj).unwrap();
        assert_eq!(f, proj.join(PROJECT_FILE));
        register(&paths, &f).unwrap();
        assert!(list(&paths).is_empty(), "file doesn't exist yet");
        fs::write(&f, "").unwrap();
        assert_eq!(list(&paths), vec![f.clone()]);
        unregister(&paths, &f).unwrap();
        assert!(list(&paths).is_empty());
    }
}
