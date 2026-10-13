//! Conda discovery and activation snapshots.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{EnvMap, get, which_in};
use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct CondaEnv {
    pub name: String,
    pub prefix: String,
}

fn is_base(dir: &Path) -> bool {
    if cfg!(windows) {
        dir.join("condabin").join("conda.bat").is_file()
    } else {
        dir.join("etc/profile.d/conda.sh").is_file()
    }
}

const INSTALL_NAMES: &[&str] = &["miniconda3", "anaconda3", "miniforge3", "mambaforge"];

/// Locate the conda installation root: `$CONDA_EXE`, then `conda` on PATH, then common install dirs.
pub fn find_base(env: &EnvMap) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // <base>/bin/conda, <base>\Scripts\conda.exe and <base>\condabin\conda.bat are all two levels down.
    if let Some(exe) = get(env, "CONDA_EXE") {
        candidates.extend(Path::new(exe).parent().and_then(Path::parent).map(Path::to_path_buf));
    }
    if let Some(exe) = which_in(env, "conda") {
        let exe = dunce::canonicalize(&exe).unwrap_or(exe);
        candidates.extend(exe.parent().and_then(Path::parent).map(Path::to_path_buf));
    }
    // GitHub's runner images and some installers set $CONDA to the root.
    if let Some(root) = get(env, "CONDA") {
        candidates.push(PathBuf::from(root));
    }
    let home = paths::home();
    for d in INSTALL_NAMES.iter().chain(&["opt/miniconda3", "opt/anaconda3"]) {
        candidates.push(home.join(d));
    }
    if cfg!(windows) {
        for var in ["LOCALAPPDATA", "ProgramData"] {
            if let Some(root) = get(env, var) {
                candidates.extend(INSTALL_NAMES.iter().map(|d| Path::new(root).join(d)));
            }
        }
        candidates.extend(INSTALL_NAMES.iter().map(|d| Path::new("C:\\tools").join(d)));
    } else {
        for d in [
            "/opt/conda",
            "/opt/miniconda3",
            "/opt/anaconda3",
            "/opt/miniforge3",
            "/opt/homebrew/Caskroom/miniconda/base",
            "/opt/homebrew/Caskroom/miniforge/base",
            "/opt/homebrew/anaconda3",
            "/usr/local/Caskroom/miniconda/base",
            "/usr/local/anaconda3",
        ] {
            candidates.push(PathBuf::from(d));
        }
    }
    candidates.into_iter().find(|c| is_base(c))
}

/// Directories that may contain named envs.
fn envs_dirs(base: &Path, env: &EnvMap) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = get(env, "CONDA_ENVS_PATH") {
        dirs.extend(std::env::split_paths(p));
    }
    for rc in [paths::home().join(".condarc"), base.join(".condarc")] {
        if let Ok(text) = fs::read_to_string(rc) {
            dirs.extend(condarc_envs_dirs(&text).into_iter().map(|s| paths::expand_tilde(&s)));
        }
    }
    dirs.push(base.join("envs"));
    dirs.push(paths::home().join(".conda/envs"));
    dirs
}

/// Minimal parse of the `envs_dirs:` list in a `.condarc`.
fn condarc_envs_dirs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("envs_dirs:") {
            in_list = true;
            continue;
        }
        if in_list {
            if let Some(item) = trimmed.strip_prefix("- ") {
                out.push(item.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                in_list = false;
            }
        }
    }
    out
}

/// All known envs: base, envs dirs, and `~/.conda/environments.txt`.
pub fn list_envs(base: &Path, env: &EnvMap) -> Vec<CondaEnv> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut push = |name: String, prefix: PathBuf, out: &mut Vec<CondaEnv>| {
        if prefix.join("conda-meta").is_dir() && seen.insert(prefix.clone()) {
            out.push(CondaEnv { name, prefix: prefix.display().to_string() });
        }
    };
    push("base".into(), base.to_path_buf(), &mut out);
    for dir in envs_dirs(base, env) {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut names: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        names.sort();
        for p in names {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            push(name, p, &mut out);
        }
    }
    if let Ok(text) = fs::read_to_string(paths::home().join(".conda/environments.txt")) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let p = PathBuf::from(line);
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            push(name, p, &mut out);
        }
    }
    out
}

/// Turn an env name into its prefix.
pub fn resolve_prefix(base: &Path, env: &EnvMap, name: &str) -> Result<PathBuf> {
    if name == "base" || name == "root" {
        return Ok(base.to_path_buf());
    }
    for dir in envs_dirs(base, env) {
        let p = dir.join(name);
        if p.join("conda-meta").is_dir() {
            return Ok(p);
        }
    }
    if let Some(e) = list_envs(base, env).into_iter().find(|e| e.name == name) {
        return Ok(PathBuf::from(e.prefix));
    }
    bail!("conda env `{name}` not found (see `conda env list`)")
}

/// Activate in a throwaway shell and capture the resulting environment, so `activate.d`
/// hooks are honored. Hook output goes to stderr and is shown only on failure.
pub fn snapshot(base: &Path, target: &str, clean_env: &EnvMap) -> Result<EnvMap> {
    let out = activation_command(base, target)
        .env_clear()
        .envs(clean_env)
        .stdin(Stdio::null())
        .output()
        .context("running conda activation")?;
    if !out.status.success() {
        bail!("`conda activate {target}` failed:\n{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut env = parse_activated(&out.stdout);
    for k in ["_", "PWD", "OLDPWD", "SHLVL"] {
        crate::env::unset(&mut env, k);
    }
    Ok(env)
}

#[cfg(not(windows))]
fn activation_command(base: &Path, target: &str) -> std::process::Command {
    let script = r#"{ . "$1/etc/profile.d/conda.sh" && conda activate "$2"; } 1>&2 && exec /usr/bin/env -0"#;
    let mut c = crate::proc::background("/bin/sh");
    c.arg("-c").arg(script).arg("bk-conda").arg(base).arg(target);
    c
}

#[cfg(not(windows))]
fn parse_activated(stdout: &[u8]) -> EnvMap {
    super::parse_env0(stdout)
}

/// `conda.bat activate` in cmd.exe, then `set`. `/u` makes cmd write UTF-16, so paths with
/// non-ASCII characters survive whatever the console code page is.
#[cfg(windows)]
fn activation_command(base: &Path, target: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let bat = base.join("condabin").join("conda.bat");
    let mut c = crate::proc::background(comspec);
    // `/s /c "..."` strips the outer quotes and runs the rest verbatim.
    c.raw_arg(format!("/d /u /s /c \"\"{}\" activate \"{target}\" 1>&2 && set\"", bat.display()));
    c
}

#[cfg(windows)]
fn parse_activated(stdout: &[u8]) -> EnvMap {
    let units: Vec<u16> = stdout.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    super::parse_set_output(&String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condarc_list() {
        let rc = "channels:\n  - conda-forge\nenvs_dirs:\n  - ~/my-envs\n  - '/data/envs'\npkgs_dirs:\n  - /x\n";
        assert_eq!(condarc_envs_dirs(rc), vec!["~/my-envs".to_string(), "/data/envs".to_string()]);
    }

    #[test]
    fn real_conda_if_present() {
        let env = super::super::current();
        let Some(base) = find_base(&env) else { return };
        let envs = list_envs(&base, &env);
        assert_eq!(envs[0].name, "base");
        let snap = snapshot(&base, "base", &super::super::clean(&env)).unwrap();
        assert_eq!(super::super::get_string(&snap, "CONDA_PREFIX").unwrap(), base.display().to_string());
    }
}
