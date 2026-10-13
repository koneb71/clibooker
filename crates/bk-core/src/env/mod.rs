//! Environment activation.
//!
//! Final env = clean(base) → activation → dotenv → vars → overrides (see `plan`).
//! `Inherit` skips the cleaning step and uses the caller's environment unchanged.

pub mod conda;
pub mod detect;
pub mod discover;
pub mod login;
pub mod runtimes;
pub mod venv;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::model::EnvSpec;
use crate::paths;

pub type EnvMap = BTreeMap<OsString, OsString>;

pub fn current() -> EnvMap {
    std::env::vars_os().collect()
}

pub fn get<'a>(env: &'a EnvMap, key: &str) -> Option<&'a OsStr> {
    env.get(OsStr::new(key)).map(OsString::as_os_str).filter(|v| !v.is_empty())
}

pub fn get_string(env: &EnvMap, key: &str) -> Option<String> {
    get(env, key).map(|v| v.to_string_lossy().into_owned())
}

pub fn set(env: &mut EnvMap, key: &str, value: impl Into<OsString>) {
    env.insert(key.into(), value.into());
}

pub fn unset(env: &mut EnvMap, key: &str) {
    env.remove(OsStr::new(key));
}

pub fn path_entries(env: &EnvMap) -> Vec<PathBuf> {
    get(env, "PATH").map(|p| std::env::split_paths(p).collect()).unwrap_or_default()
}

fn set_path_entries(env: &mut EnvMap, entries: &[PathBuf]) {
    if let Ok(joined) = std::env::join_paths(entries) {
        env.insert("PATH".into(), joined);
    }
}

/// Put `dir` first on PATH (removing any later duplicate).
pub fn prepend_path(env: &mut EnvMap, dir: &Path) {
    let mut entries = path_entries(env);
    entries.retain(|e| e != dir);
    entries.insert(0, dir.to_path_buf());
    set_path_entries(env, &entries);
}

/// Find an executable on the env's PATH.
pub fn which_in(env: &EnvMap, program: &str) -> Option<PathBuf> {
    let path = get(env, "PATH")?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    which::which_in(program, Some(path), cwd).ok()
}

/// Keys never passed to children.
fn is_internal(key: &OsStr) -> bool {
    key.as_bytes().starts_with(b"BK_")
}

/// Strip any active conda env / venv so activation starts from a neutral state.
/// Keeps `condabin` and `CONDA_EXE` so `conda` itself stays usable.
pub fn clean(base: &EnvMap) -> EnvMap {
    let mut drop_dirs: Vec<PathBuf> = Vec::new();
    for (k, v) in base {
        let kb = k.as_bytes();
        if (kb.starts_with(b"CONDA_PREFIX") || kb == b"VIRTUAL_ENV") && !v.is_empty() {
            drop_dirs.push(Path::new(v).join("bin"));
        }
    }
    let mut env: EnvMap = base
        .iter()
        .filter(|(k, _)| {
            let kb = k.as_bytes();
            !(kb.starts_with(b"CONDA_PREFIX")
                || is_internal(k)
                || matches!(
                    kb,
                    b"CONDA_SHLVL"
                        | b"CONDA_DEFAULT_ENV"
                        | b"CONDA_PROMPT_MODIFIER"
                        | b"VIRTUAL_ENV"
                        | b"VIRTUAL_ENV_PROMPT"
                        | b"PYTHONHOME"
                ))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut entries = path_entries(&env);
    entries.retain(|e| !drop_dirs.contains(e));
    set_path_entries(&mut env, &entries);
    env
}

/// Outcome of activating an environment.
#[derive(Debug, Clone)]
pub struct Activated {
    pub env: EnvMap,
    /// Concrete spec (`Auto` resolved).
    pub spec: EnvSpec,
    /// Commands to run before the bookmark's steps (e.g. `uv sync`).
    pub pre_steps: Vec<String>,
    /// Human description, e.g. `conda env sam2 (/opt/miniconda3/envs/sam2)`.
    pub description: String,
}

/// How far to go when activating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fully activate (runs conda activation hooks).
    Run,
    /// Resolve and validate only; never runs hooks. Used by dry-run and doctor.
    Check,
}

pub fn activate(spec: &EnvSpec, cwd: &Path, base: &EnvMap, mode: Mode) -> Result<Activated> {
    let mut pre_steps = Vec::new();
    let (env, description, spec) = match spec {
        EnvSpec::Inherit => {
            let env = base.iter().filter(|(k, _)| !is_internal(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
            (env, "inherited environment".to_string(), spec.clone())
        }
        EnvSpec::None => (clean(base), "clean environment (no conda/venv)".to_string(), spec.clone()),
        EnvSpec::Conda { name, prefix } => {
            let conda_base = conda::find_base(base).context("conda not found (set CONDA_EXE or install conda)")?;
            let target_prefix = match (name, prefix) {
                (_, Some(p)) => paths::resolve(p, cwd),
                (Some(n), None) => conda::resolve_prefix(&conda_base, base, n)?,
                (None, None) => bail!("conda env needs a `name` or `prefix`"),
            };
            if !target_prefix.join("conda-meta").is_dir() {
                bail!("conda env not found at {}", target_prefix.display());
            }
            let label = name.clone().unwrap_or_else(|| paths::contract_tilde(&target_prefix));
            let cleaned = clean(base);
            let env = match mode {
                Mode::Run => {
                    let arg = name.clone().unwrap_or_else(|| target_prefix.display().to_string());
                    conda::snapshot(&conda_base, &arg, &cleaned)?
                }
                Mode::Check => {
                    let mut e = cleaned;
                    prepend_path(&mut e, &target_prefix.join("bin"));
                    set(&mut e, "CONDA_PREFIX", target_prefix.as_os_str());
                    e
                }
            };
            (env, format!("conda env {label} ({})", paths::contract_tilde(&target_prefix)), spec.clone())
        }
        EnvSpec::Venv { path } => {
            let dir = paths::resolve(path, cwd);
            if !venv::is_venv(&dir) {
                bail!("no virtualenv at {} (create it with `python -m venv {path}`)", dir.display());
            }
            let mut env = clean(base);
            venv::apply(&mut env, &dir);
            (env, format!("venv {}", paths::contract_tilde(&dir)), spec.clone())
        }
        EnvSpec::Uv { sync } => {
            let dir = venv::uv_env_dir(cwd, base);
            if *sync {
                pre_steps.push("uv sync".to_string());
            } else if !venv::is_venv(&dir) {
                bail!("no uv environment at {} (run `uv sync`, or set sync = true)", dir.display());
            }
            let mut env = clean(base);
            venv::apply(&mut env, &dir);
            (env, format!("uv project env {}", paths::contract_tilde(&dir)), spec.clone())
        }
        EnvSpec::Poetry => {
            let mut env = clean(base);
            let dir = venv::poetry_env_dir(cwd, &env)?;
            venv::apply(&mut env, &dir);
            (env, format!("poetry env {}", paths::contract_tilde(&dir)), spec.clone())
        }
        EnvSpec::Pyenv { version } => {
            let mut env = clean(base);
            let root = venv::pyenv_apply(&mut env, version)?;
            (env, format!("pyenv {version} ({})", paths::contract_tilde(&root)), spec.clone())
        }
        EnvSpec::Mise => {
            let mut env = clean(base);
            let vars = mise_env(cwd, &env)?;
            let n = vars.len();
            for (k, v) in vars {
                env.insert(k.into(), v.into());
            }
            (env, format!("mise tools for this folder ({n} variables)"), spec.clone())
        }
        EnvSpec::Auto => {
            let found = detect::from_folder(cwd, base);
            return match found {
                Some(concrete) => {
                    let mut a = activate(&concrete, cwd, base, mode)?;
                    a.description = format!("{} (auto-detected)", a.description);
                    Ok(a)
                }
                None => {
                    let mut a = activate(&EnvSpec::Inherit, cwd, base, mode)?;
                    a.description = "inherited environment (auto: nothing detected)".into();
                    Ok(a)
                }
            };
        }
    };
    Ok(Activated { env, spec, pre_steps, description })
}

/// `mise env --json` in `cwd`: the variables mise would set for the folder's tools.
fn mise_env(cwd: &Path, env: &EnvMap) -> Result<BTreeMap<String, String>> {
    let mise = which_in(env, "mise").context("mise not found on PATH")?;
    let out = std::process::Command::new(mise)
        .args(["env", "--json"])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::null())
        .output()
        .context("running `mise env`")?;
    if !out.status.success() {
        bail!("`mise env` failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).context("parsing `mise env --json`")
}

/// Parse `env -0` output: NUL-separated `KEY=VALUE` records, split at the first `=`.
pub fn parse_env0(bytes: &[u8]) -> EnvMap {
    bytes
        .split(|b| *b == 0)
        .filter_map(|rec| {
            let eq = rec.iter().position(|b| *b == b'=')?;
            if eq == 0 {
                return None;
            }
            Some((OsString::from_vec(rec[..eq].to_vec()), OsString::from_vec(rec[eq + 1..].to_vec())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> EnvMap {
        pairs.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
    }

    #[test]
    fn clean_strips_active_envs() {
        let base = map(&[
            ("PATH", "/opt/c/envs/x/bin:/v/bin:/opt/c/condabin:/usr/bin"),
            ("CONDA_PREFIX", "/opt/c/envs/x"),
            ("CONDA_PREFIX_1", "/opt/c"),
            ("CONDA_DEFAULT_ENV", "x"),
            ("CONDA_SHLVL", "2"),
            ("CONDA_EXE", "/opt/c/bin/conda"),
            ("VIRTUAL_ENV", "/v"),
            ("BK_LAST_CMD", "ls"),
            ("HOME", "/h"),
        ]);
        let c = clean(&base);
        assert_eq!(get_string(&c, "PATH").unwrap(), "/opt/c/condabin:/usr/bin");
        assert!(get(&c, "CONDA_PREFIX").is_none());
        assert!(get(&c, "CONDA_PREFIX_1").is_none());
        assert!(get(&c, "VIRTUAL_ENV").is_none());
        assert!(get(&c, "BK_LAST_CMD").is_none());
        assert_eq!(get_string(&c, "CONDA_EXE").unwrap(), "/opt/c/bin/conda");
        assert_eq!(get_string(&c, "HOME").unwrap(), "/h");
    }

    #[test]
    fn env0_parsing() {
        let e = parse_env0(b"A=1\0B=x=y\0MULTI=l1\nl2\0BASH_FUNC_f%%=() { :; }\0\0");
        assert_eq!(get_string(&e, "B").unwrap(), "x=y");
        assert_eq!(get_string(&e, "MULTI").unwrap(), "l1\nl2");
        assert_eq!(e.len(), 4);
    }

    #[test]
    fn prepend_dedupes() {
        let mut e = map(&[("PATH", "/a:/b")]);
        prepend_path(&mut e, Path::new("/b"));
        assert_eq!(get_string(&e, "PATH").unwrap(), "/b:/a");
    }
}
