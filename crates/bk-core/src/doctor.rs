//! Health checks for bookmarks: missing folders, envs that no longer exist, etc.

use std::path::Path;

use serde::Serialize;

use crate::env::{self, EnvMap, Mode};
use crate::model::Settings;
use crate::paths;
use crate::plan;
use crate::store::{Entry, Library};

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub entry_id: String,
    pub name: String,
    pub file: String,
    pub env: String,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
    pub file_errors: Vec<String>,
}

impl Report {
    pub fn problem_count(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok()).count() + self.file_errors.len()
    }
}

/// Remote bookmarks: the server must resolve; with a cached scan of the server, the env and
/// runtimes are checked too (offline — no connection is made).
fn check_remote(
    e: &Entry,
    servers: &std::collections::BTreeMap<String, crate::model::Server>,
    paths: &crate::Paths,
) -> Check {
    let b = &e.bookmark;
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    if let Err(msg) = b.validate() {
        problems.push(msg);
    }
    let server = b.server.clone().unwrap_or_default();
    match crate::servers::resolve_target(&server, servers) {
        Err(err) => problems.push(format!("{err:#}")),
        Ok(t) => {
            if crate::servers::load_cached(paths, &t.name).is_none() {
                warnings.push(format!("{} hasn't been scanned yet; env and runtimes aren't checked", t.name));
            } else {
                let inp = crate::remote::RemoteInputs {
                    servers,
                    paths,
                    ssh_program: "ssh".into(),
                    base_env: &EnvMap::new(),
                    online: false,
                    markers: false,
                };
                let req = crate::plan::RunRequest {
                    params: crate::template::bookmark_params(b)
                        .into_iter()
                        .map(|p| (p.name.clone(), p.default.unwrap_or_else(|| "x".into())))
                        .collect(),
                    ..Default::default()
                };
                if let Err(err) = crate::remote::build(e, &req, &inp) {
                    problems.push(format!("{err:#}"));
                }
            }
        }
    }
    Check {
        entry_id: e.id.clone(),
        name: e.name.clone(),
        file: paths::contract_tilde(&e.file),
        env: b.env_summary(),
        problems,
        warnings,
    }
}

pub fn check_entry(e: &Entry, base: &EnvMap, settings: &Settings, default_cwd: &Path) -> Check {
    let b = &e.bookmark;
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    if let Err(msg) = b.validate() {
        problems.push(msg);
    }
    let cwd = e.cwd();
    let check_dir = cwd.clone().unwrap_or_else(|| default_cwd.to_path_buf());
    if let Some(c) = &cwd
        && !c.is_dir()
    {
        problems.push(format!("folder {} does not exist", paths::contract_tilde(c)));
    }
    if check_dir.is_dir()
        && let Err(err) = env::activate(&b.env, &check_dir, base, Mode::Check)
    {
        problems.push(format!("{err:#}"));
    }
    for (lang, spec) in &b.runtimes {
        if let Err(err) = crate::env::runtimes::resolve(lang, spec, &check_dir, base) {
            problems.push(format!("{err:#}"));
        }
    }
    if let Some(f) = &b.dotenv {
        let p = paths::resolve(f, &check_dir);
        if !p.is_file() {
            problems.push(format!(".env file {} not found", paths::contract_tilde(&p)));
        }
    }
    let shell = b.shell.clone().or_else(|| settings.shell.clone()).unwrap_or_else(|| plan::default_shell(base));
    if let Err(err) = plan::resolve_shell(&shell, base) {
        problems.push(format!("{err:#}"));
    }
    for s in b.effective_steps() {
        if let Some(c) = s.cwd.as_deref().filter(|c| !c.contains("{{")) {
            let p = paths::resolve(c, &check_dir);
            if !p.is_dir() {
                problems.push(format!("step folder {} does not exist", paths::contract_tilde(&p)));
            }
        }
    }
    if b.env.is_inherit() && b.effective_steps().iter().any(|s| s.run.trim_start().starts_with("python")) {
        warnings.push("runs python without a pinned environment".into());
    }
    Check {
        entry_id: e.id.clone(),
        name: e.name.clone(),
        file: paths::contract_tilde(&e.file),
        env: b.env_summary(),
        problems,
        warnings,
    }
}

pub fn check_library(lib: &Library, base: &EnvMap, default_cwd: &Path, paths: &crate::Paths) -> Report {
    Report {
        checks: lib
            .entries
            .iter()
            .filter(|e| !lib.is_shadowed(e))
            .map(|e| {
                if e.bookmark.is_remote() {
                    check_remote(e, &lib.servers, paths)
                } else {
                    check_entry(e, base, &lib.settings, default_cwd)
                }
            })
            .collect(),
        file_errors: lib.errors.iter().map(|e| e.message.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bookmark, EnvSpec};
    use crate::store::Scope;
    use std::ffi::OsString;

    #[test]
    fn reports_missing_things() {
        let d = tempfile::tempdir().unwrap();
        let base: EnvMap =
            [("PATH".into(), OsString::from("/usr/bin:/bin")), ("SHELL".into(), "/bin/zsh".into())].into();
        let good = Entry::new(
            "good",
            Scope::Global,
            &d.path().join("b.toml"),
            Bookmark { command: Some("ls".into()), cwd: Some(d.path().display().to_string()), ..Default::default() },
        );
        let c = check_entry(&good, &base, &Settings::default(), d.path());
        assert!(c.ok(), "{:?}", c.problems);

        let bad = Entry::new(
            "bad",
            Scope::Global,
            &d.path().join("b.toml"),
            Bookmark {
                command: Some("python x.py".into()),
                cwd: Some(d.path().display().to_string()),
                env: EnvSpec::Venv { path: ".venv".into() },
                dotenv: Some(".env".into()),
                shell: Some("nosuchshell".into()),
                ..Default::default()
            },
        );
        let c = check_entry(&bad, &base, &Settings::default(), d.path());
        assert_eq!(c.problems.len(), 3, "{:?}", c.problems);
    }
}
