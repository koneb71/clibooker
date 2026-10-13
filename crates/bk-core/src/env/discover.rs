//! Filesystem-only inventory of available environments (fast enough for UI dropdowns).

use std::fs;
use std::path::Path;

use serde::Serialize;

use std::collections::BTreeMap;

use super::{EnvMap, conda, detect, runtimes, venv, which_in};
use crate::model::EnvSpec;
use crate::paths;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Tools {
    pub conda: bool,
    pub uv: bool,
    pub poetry: bool,
    pub pyenv: bool,
    pub mise: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Inventory {
    pub conda_base: Option<String>,
    pub conda_envs: Vec<conda::CondaEnv>,
    pub pyenv_versions: Vec<String>,
    pub tools: Tools,
    /// Installed versions per language runtime (node, java, ruby, go, rust, php), newest first.
    pub runtimes: BTreeMap<String, Vec<runtimes::Install>>,
}

pub fn inventory(env: &EnvMap) -> Inventory {
    let base = conda::find_base(env);
    let conda_envs = base.as_deref().map(|b| conda::list_envs(b, env)).unwrap_or_default();
    let pyenv_root = venv::pyenv_root(env);
    let mut pyenv_versions: Vec<String> = fs::read_dir(pyenv_root.join("versions"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    pyenv_versions.sort();
    Inventory {
        conda_base: base.as_deref().map(paths::contract_tilde),
        conda_envs,
        pyenv_versions,
        tools: Tools {
            conda: base.is_some(),
            uv: which_in(env, "uv").is_some(),
            poetry: which_in(env, "poetry").is_some(),
            pyenv: pyenv_root.join("shims").is_dir(),
            mise: which_in(env, "mise").is_some(),
        },
        runtimes: runtimes::SUPPORTED.iter().map(|l| (l.to_string(), runtimes::installed(l, env))).collect(),
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FolderScan {
    pub exists: bool,
    /// Best guess from folder markers (ignores the calling shell's active env).
    pub detected: Option<EnvSpec>,
    /// Virtualenv dirs found directly in the folder (`.venv`, `venv`, `env`).
    pub venvs: Vec<String>,
    pub has_uv_lock: bool,
    pub has_poetry_lock: bool,
    pub dotenv_files: Vec<String>,
    pub conda_env_file: Option<String>,
    pub python_version: Option<String>,
    /// Runtime versions the folder pins: lang → (version, file).
    pub runtime_hints: BTreeMap<String, (String, String)>,
    /// npm / pnpm / yarn / bun, when there's a package.json.
    pub package_manager: Option<String>,
    /// How many project scripts (package.json, Makefile, ...) were found.
    pub script_count: usize,
}

pub fn scan_folder(dir: &Path, env: &EnvMap) -> FolderScan {
    if !dir.is_dir() {
        return FolderScan::default();
    }
    let mut dotenv_files: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n == ".env" || n.starts_with(".env."))
                .collect()
        })
        .unwrap_or_default();
    dotenv_files.sort();
    FolderScan {
        exists: true,
        detected: detect::from_folder(dir, env),
        venvs: [".venv", "venv", "env"].iter().filter(|c| venv::is_venv(&dir.join(c))).map(|c| c.to_string()).collect(),
        has_uv_lock: dir.join("uv.lock").is_file(),
        has_poetry_lock: dir.join("poetry.lock").is_file(),
        dotenv_files,
        conda_env_file: detect::environment_yml_name(dir),
        python_version: detect::python_version_file(dir),
        runtime_hints: runtimes::SUPPORTED
            .iter()
            .filter_map(|l| runtimes::version_hint(l, dir).map(|h| (l.to_string(), h)))
            .collect(),
        package_manager: crate::scripts::package_manager(dir).map(str::to_string),
        script_count: crate::scripts::discover(dir).len(),
    }
}
