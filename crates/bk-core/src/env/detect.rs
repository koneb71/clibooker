//! Guess which environment a folder (or the current shell) uses.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::{EnvMap, conda, get, venv};
use crate::model::EnvSpec;
use crate::paths;

/// The env active in the calling shell (ignores conda `base`, which is usually auto-activated).
pub fn from_active(env: &EnvMap, cwd: &Path) -> Option<EnvSpec> {
    if let Some(v) = get(env, "VIRTUAL_ENV") {
        let v = PathBuf::from(v);
        if v == venv::uv_env_dir(cwd, env) && venv::project_root(cwd).join("uv.lock").is_file() {
            return Some(EnvSpec::Uv { sync: false });
        }
        return Some(EnvSpec::Venv { path: relative_or_tilde(&v, cwd) });
    }
    let name = get(env, "CONDA_DEFAULT_ENV")?.to_string_lossy().into_owned();
    if name == "base" || name == "root" {
        return None;
    }
    if name.contains('/') {
        return Some(EnvSpec::Conda { name: None, prefix: Some(paths::contract_tilde(Path::new(&name))) });
    }
    Some(EnvSpec::Conda { name: Some(name), prefix: None })
}

/// Look at project markers in `cwd` and its ancestors (stopping at `$HOME`).
pub fn from_folder(cwd: &Path, env: &EnvMap) -> Option<EnvSpec> {
    let home = paths::home();
    for dir in cwd.ancestors() {
        if dir == home || dir.parent().is_none() {
            break;
        }
        if let Some(spec) = markers(dir, cwd, env) {
            return Some(spec);
        }
    }
    None
}

fn markers(dir: &Path, cwd: &Path, env: &EnvMap) -> Option<EnvSpec> {
    let mise_config = ["mise.toml", ".mise.toml"].iter().any(|f| dir.join(f).is_file());
    if mise_config && super::which_in(env, "mise").is_some() {
        return Some(EnvSpec::Mise);
    }
    if dir.join("uv.lock").is_file() {
        return Some(EnvSpec::Uv { sync: false });
    }
    for cand in [".venv", "venv", "env"] {
        let p = dir.join(cand);
        if venv::is_venv(&p) {
            return Some(EnvSpec::Venv { path: relative_or_tilde(&p, cwd) });
        }
    }
    if dir.join("poetry.lock").is_file() {
        return Some(EnvSpec::Poetry);
    }
    if let Some(name) = environment_yml_name(dir)
        && let Some(base) = conda::find_base(env)
        && conda::resolve_prefix(&base, env, &name).is_ok()
    {
        return Some(EnvSpec::Conda { name: Some(name), prefix: None });
    }
    if let Some(v) = python_version_file(dir)
        && venv::pyenv_root(env).join("versions").join(&v).is_dir()
    {
        return Some(EnvSpec::Pyenv { version: v });
    }
    None
}

/// `name:` from `environment.yml` / `environment.yaml`.
pub fn environment_yml_name(dir: &Path) -> Option<String> {
    ["environment.yml", "environment.yaml"].iter().find_map(|f| {
        let text = fs::read_to_string(dir.join(f)).ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("name:").map(|n| n.trim().trim_matches(|c| c == '"' || c == '\'').to_string()))
    })
}

pub fn python_version_file(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join(".python-version")).ok()?;
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string)
}

/// Runtimes a folder pins through version files (`.nvmrc`, `.java-version`, ...), as `auto`
/// so the bookmark keeps following the project's file. Also picks up an `nvm use` in the
/// calling shell when nothing is pinned.
pub fn runtimes(env: &EnvMap, cwd: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for lang in ["node", "java", "ruby", "go", "php"] {
        if super::runtimes::version_hint(lang, cwd).is_some() {
            out.insert(lang.to_string(), "auto".to_string());
        }
    }
    if !out.contains_key("node")
        && let Some(bin) = get(env, "NVM_BIN")
    {
        // ~/.nvm/versions/node/v22.14.0/bin
        let version = Path::new(bin).parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
        if let Some(v) = version.filter(|v| v.starts_with('v')) {
            out.insert("node".into(), v.trim_start_matches('v').to_string());
        }
    }
    out
}

/// Active env first, then folder markers, else inherit.
pub fn detect(env: &EnvMap, cwd: &Path) -> EnvSpec {
    from_active(env, cwd).or_else(|| from_folder(cwd, env)).unwrap_or_default()
}

fn relative_or_tilde(p: &Path, cwd: &Path) -> String {
    if let Ok(rest) = p.strip_prefix(cwd) {
        return rest.display().to_string();
    }
    // `../.venv` when the venv is in an ancestor project dir.
    for (depth, anc) in cwd.ancestors().enumerate().skip(1).take(4) {
        if let Ok(rest) = p.strip_prefix(anc) {
            return format!("{}{}", "../".repeat(depth), rest.display());
        }
    }
    paths::contract_tilde(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn fake_venv(p: &Path) {
        fs::create_dir_all(p.join("bin")).unwrap();
        fs::write(p.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
    }

    #[test]
    fn folder_markers() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let env = EnvMap::new();
        assert_eq!(from_folder(root, &env), None);
        fake_venv(&root.join(".venv"));
        assert_eq!(from_folder(root, &env), Some(EnvSpec::Venv { path: ".venv".into() }));
        fs::create_dir_all(root.join("src")).unwrap();
        assert_eq!(from_folder(&root.join("src"), &env), Some(EnvSpec::Venv { path: "../.venv".into() }));
        fs::write(root.join("uv.lock"), "").unwrap();
        assert_eq!(from_folder(root, &env), Some(EnvSpec::Uv { sync: false }));
    }

    #[test]
    fn active_env_wins_and_base_is_ignored() {
        let cwd = Path::new("/w/p");
        let env: EnvMap = [("CONDA_DEFAULT_ENV".into(), OsString::from("base"))].into();
        assert_eq!(from_active(&env, cwd), None);
        let env: EnvMap = [("CONDA_DEFAULT_ENV".into(), OsString::from("sam2"))].into();
        assert_eq!(from_active(&env, cwd), Some(EnvSpec::Conda { name: Some("sam2".into()), prefix: None }));
        let env: EnvMap = [("VIRTUAL_ENV".into(), OsString::from("/w/p/.venv"))].into();
        assert_eq!(from_active(&env, cwd), Some(EnvSpec::Venv { path: ".venv".into() }));
    }

    #[test]
    fn environment_yml() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("environment.yml"), "name: 'sam2'\ndependencies:\n  - python\n").unwrap();
        assert_eq!(environment_yml_name(d.path()).as_deref(), Some("sam2"));
    }
}
