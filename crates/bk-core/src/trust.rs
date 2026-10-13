//! direnv-style trust for project `.bk.toml` files.
//!
//! A project file (e.g. from a cloned repo) can contain arbitrary commands, so its
//! bookmarks only run after the user has approved that exact file content. Editing
//! the file through bk re-approves it; edits from elsewhere require a new approval.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::paths::{self, Paths};
use crate::store::atomic_write;

fn digest(file: &Path) -> Option<String> {
    let bytes = fs::read(file).ok()?;
    let hash = Sha256::digest(&bytes);
    Some(hash.iter().map(|b| format!("{b:02x}")).collect())
}

fn read(paths: &Paths) -> BTreeMap<String, String> {
    fs::read_to_string(paths.trust_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn key(file: &Path) -> String {
    let canon = dunce::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    paths::contract_tilde(&canon)
}

/// The global file is always trusted; project files only when their content was approved.
pub fn is_trusted(paths: &Paths, file: &Path) -> bool {
    if file == paths.config_file {
        return true;
    }
    match digest(file) {
        Some(d) => read(paths).get(&key(file)) == Some(&d),
        None => false,
    }
}

/// Approve the file's current content.
pub fn trust(paths: &Paths, file: &Path) -> Result<()> {
    let Some(d) = digest(file) else { return Ok(()) };
    let mut map = read(paths);
    map.insert(key(file), d);
    fs::create_dir_all(&paths.data_dir)?;
    atomic_write(&paths.trust_file(), serde_json::to_string_pretty(&map)?.as_bytes())
}

pub fn untrust(paths: &Paths, file: &Path) -> Result<()> {
    let mut map = read(paths);
    if map.remove(&key(file)).is_some() {
        atomic_write(&paths.trust_file(), serde_json::to_string_pretty(&map)?.as_bytes())?;
    }
    Ok(())
}

/// Run a write to `file` through bk; if the file was trusted (or new/empty) before,
/// keep it trusted afterwards. Edits never launder an untrusted file into a trusted one.
pub fn write_preserving<T>(paths: &Paths, file: &Path, write: impl FnOnce() -> Result<T>) -> Result<T> {
    let was_ok = file == paths.config_file
        || fs::metadata(file).map(|m| m.len() == 0).unwrap_or(true)
        || is_trusted(paths, file);
    let out = write()?;
    if was_ok && file != paths.config_file {
        trust(paths, file)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_follows_content() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { config_file: d.path().join("g.toml"), data_dir: d.path().join("data") };
        let f = d.path().join(".bk.toml");
        fs::write(&f, "[bookmarks.a]\ncommand = \"ls\"\n").unwrap();
        assert!(is_trusted(&paths, &paths.config_file));
        assert!(!is_trusted(&paths, &f));
        trust(&paths, &f).unwrap();
        assert!(is_trusted(&paths, &f));
        fs::write(&f, "[bookmarks.a]\ncommand = \"rm -rf ~\"\n").unwrap();
        assert!(!is_trusted(&paths, &f), "changed content needs re-approval");
        trust(&paths, &f).unwrap();
        untrust(&paths, &f).unwrap();
        assert!(!is_trusted(&paths, &f));
    }

    #[test]
    fn writes_keep_but_never_grant_trust() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { config_file: d.path().join("g.toml"), data_dir: d.path().join("data") };
        let f = d.path().join(".bk.toml");
        // New file: trusted after our write.
        write_preserving(&paths, &f, || Ok(fs::write(&f, "a = 1\n")?)).unwrap();
        assert!(is_trusted(&paths, &f));
        // Changed behind our back, then edited by us: stays untrusted.
        fs::write(&f, "a = 2\n").unwrap();
        write_preserving(&paths, &f, || Ok(fs::write(&f, "a = 3\n")?)).unwrap();
        assert!(!is_trusted(&paths, &f));
    }
}
