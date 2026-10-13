//! Native activation for virtualenvs (venv, uv, poetry) and pyenv.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use super::{EnvMap, get, prepend_path, set, unset};
use crate::paths;

pub fn is_venv(dir: &Path) -> bool {
    dir.join("pyvenv.cfg").is_file()
        || dir.join("bin").join("python").exists()
        || dir.join("Scripts").join("python.exe").exists()
}

/// A venv's executables folder: `Scripts` for Windows Python, `bin` elsewhere (and for
/// MSYS2/Cygwin Python on Windows).
pub fn bin_dir(venv: &Path) -> PathBuf {
    let scripts = venv.join("Scripts");
    let bin = venv.join("bin");
    if scripts.is_dir() || (cfg!(windows) && !bin.is_dir()) { scripts } else { bin }
}

/// What `source <venv>/bin/activate` does, minus the prompt.
pub fn apply(env: &mut EnvMap, venv: &Path) {
    set(env, "VIRTUAL_ENV", venv.as_os_str());
    unset(env, "PYTHONHOME");
    prepend_path(env, &bin_dir(venv));
}

/// The project root uv would use: nearest ancestor with a `pyproject.toml`, else `cwd`.
pub fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors().find(|d| d.join("pyproject.toml").is_file()).unwrap_or(cwd).to_path_buf()
}

/// Env dir uv uses for the project: `$UV_PROJECT_ENVIRONMENT` or `<root>/.venv`.
pub fn uv_env_dir(cwd: &Path, env: &EnvMap) -> PathBuf {
    let root = project_root(cwd);
    match get(env, "UV_PROJECT_ENVIRONMENT") {
        Some(p) => paths::resolve(&p.to_string_lossy(), &root),
        None => root.join(".venv"),
    }
}

/// Ask poetry where the project's env lives.
pub fn poetry_env_dir(cwd: &Path, env: &EnvMap) -> Result<PathBuf> {
    let in_project = cwd.join(".venv");
    let poetry = super::which_in(env, "poetry").context("poetry not found on PATH")?;
    let out = crate::proc::background(poetry)
        .args(["env", "info", "--path"])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .output()
        .context("running `poetry env info --path`")?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !path.is_empty() {
        return Ok(PathBuf::from(path));
    }
    if is_venv(&in_project) {
        return Ok(in_project);
    }
    bail!("poetry has no environment for {} yet (run `poetry install`)", cwd.display())
}

pub fn pyenv_root(env: &EnvMap) -> PathBuf {
    get(env, "PYENV_ROOT").map(PathBuf::from).unwrap_or_else(|| paths::home().join(".pyenv"))
}

/// Select a pyenv version for the child via `PYENV_VERSION` + shims.
pub fn pyenv_apply(env: &mut EnvMap, version: &str) -> Result<PathBuf> {
    let root = pyenv_root(env);
    if !root.join("shims").is_dir() {
        bail!("pyenv not found at {}", root.display());
    }
    if version != "system" && !root.join("versions").join(version).is_dir() {
        bail!("pyenv version {version} is not installed (run `pyenv install {version}`)");
    }
    set(env, "PYENV_VERSION", version);
    prepend_path(env, &root.join("shims"));
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn apply_sets_path_and_virtual_env() {
        let mut env: EnvMap = [("PATH".into(), OsString::from("/usr/bin")), ("PYTHONHOME".into(), "/x".into())].into();
        apply(&mut env, Path::new("/p/.venv"));
        let expected = std::env::join_paths([bin_dir(Path::new("/p/.venv")), "/usr/bin".into()]).unwrap();
        assert_eq!(super::super::get(&env, "PATH").unwrap(), expected);
        assert_eq!(super::super::get_string(&env, "VIRTUAL_ENV").unwrap(), "/p/.venv");
        assert!(super::super::get(&env, "PYTHONHOME").is_none());
    }

    #[test]
    fn uv_dir_uses_project_root() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("pyproject.toml"), "").unwrap();
        std::fs::create_dir_all(d.path().join("src/pkg")).unwrap();
        let env = EnvMap::new();
        assert_eq!(uv_env_dir(&d.path().join("src/pkg"), &env), d.path().join(".venv"));
        let env: EnvMap = [("UV_PROJECT_ENVIRONMENT".into(), OsString::from("envs/dev"))].into();
        assert_eq!(uv_env_dir(d.path(), &env), d.path().join("envs/dev"));
    }
}
