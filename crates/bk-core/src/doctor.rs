//! Health checks for bookmarks: missing folders, envs that no longer exist, etc.

use std::path::Path;

use serde::Serialize;

use crate::env::{self, EnvMap, Mode};
use crate::folders::{Located, Suggester};
use crate::model::{Bookmark, Os, Settings};
use crate::paths;
use crate::plan;
use crate::shell::{self, ShellKind};
use crate::store::{Entry, Library};

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub entry_id: String,
    pub name: String,
    pub file: String,
    pub env: String,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    /// The bookmark's folder isn't on this device (so the app can offer to locate it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_folder: Option<MissingFolder>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingFolder {
    /// As the bookmark names it.
    pub written: String,
    /// The OS the path was written on, when it's clearly another one.
    pub foreign: Option<&'static str>,
    /// A folder with the same name found on this device.
    pub suggestion: Option<String>,
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
        missing_folder: None,
    }
}

pub fn check_entry(e: &Entry, base: &EnvMap, settings: &Settings, default_cwd: &Path) -> Check {
    let b = &e.bookmark;
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    if let Err(msg) = b.validate() {
        problems.push(msg);
    }
    let folder = e.folder();
    let mut missing_folder = None;
    match &folder {
        Some(l @ Located::Missing { written, foreign, .. }) => {
            problems.push(format!("folder {}", l.missing_reason().unwrap_or_default()));
            missing_folder = Some(MissingFolder { written: written.clone(), foreign: *foreign, suggestion: None });
        }
        Some(l) if !l.path().is_dir() => {
            problems.push(format!("folder {} does not exist", paths::contract_tilde(l.path())))
        }
        _ => {}
    }
    let check_dir = folder.map(Located::into_path).unwrap_or_else(|| default_cwd.to_path_buf());
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
    let shell = shell_name_on(b, Os::current(), settings, base);
    let runs_in = match plan::resolve_shell(&shell, base) {
        Ok(found) => {
            warnings.extend(found.note());
            ShellKind::of(&found.path)
        }
        Err(err) => {
            problems.push(format!("{err:#}"));
            ShellKind::of(Path::new(&shell))
        }
    };
    if let Some(m) = shell_mismatch(b, Os::current(), runs_in) {
        warnings.push(m.message(true));
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
        missing_folder,
    }
}

/// A step written for another shell than the one that runs it on some OS, e.g. `export A=1`
/// where PowerShell runs it.
#[derive(Debug, Clone, Serialize)]
pub struct ShellMismatch {
    pub os: Os,
    pub step: String,
    pub written: ShellKind,
    pub runs_in: ShellKind,
}

impl ShellMismatch {
    /// `here`: said on that OS itself, so no need to name it.
    pub fn message(&self, here: bool) -> String {
        let place = if here { "here".to_string() } else { format!("on {}", self.os.label()) };
        let fix = if self.os == Os::Windows && self.written == ShellKind::Posix {
            "add a Windows command, or set shell = \"bash\" (Git Bash)".to_string()
        } else {
            format!("add a {} command", self.os.label())
        };
        format!(
            "`{}` looks like {} syntax, but it runs in {} {place}; {fix}",
            self.step,
            self.written.label(),
            self.runs_in.label()
        )
    }
}

/// The shell that runs `b`'s steps on `os`: the bookmark's own choice; on this OS, the
/// settings or the default; elsewhere, that OS's usual default.
pub fn shell_name_on(b: &Bookmark, os: Os, settings: &Settings, base: &EnvMap) -> String {
    match b.shell_on(os) {
        Some(s) => s.to_string(),
        None if os == Os::current() => settings.shell.clone().unwrap_or_else(|| plan::default_shell(base)),
        None if os == Os::Windows => "powershell".into(),
        None => "sh".into(),
    }
}

/// The first step that's unmistakably written for another shell than `runs_in` on `os`.
pub fn shell_mismatch(b: &Bookmark, os: Os, runs_in: ShellKind) -> Option<ShellMismatch> {
    if b.is_remote() || runs_in == ShellKind::Fish {
        return None;
    }
    b.steps_on(os).iter().find_map(|s| {
        let written = shell::written_for(&s.run)?;
        let line = s.run.lines().next().unwrap_or_default().trim();
        let step = if line.chars().count() > 48 {
            format!("{}…", line.chars().take(47).collect::<String>())
        } else {
            line.into()
        };
        (written != runs_in).then_some(ShellMismatch { os, step, written, runs_in })
    })
}

/// Mismatches on each of `oses` (the editor checks every OS among your synced devices).
pub fn shell_mismatches(b: &Bookmark, settings: &Settings, base: &EnvMap, oses: &[Os]) -> Vec<ShellMismatch> {
    oses.iter()
        .filter_map(|&os| shell_mismatch(b, os, ShellKind::of(Path::new(&shell_name_on(b, os, settings, base)))))
        .collect()
}

pub fn check_library(lib: &Library, base: &EnvMap, default_cwd: &Path, paths: &crate::Paths) -> Report {
    let mut report = Report {
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
    };
    if report.checks.iter().any(|c| c.missing_folder.is_some()) {
        let suggester = Suggester::new(lib, &project_dirs(paths));
        for m in report.checks.iter_mut().filter_map(|c| c.missing_folder.as_mut()) {
            m.suggestion = suggester.suggest(&m.written).first().map(|p| paths::contract_tilde(p));
        }
    }
    report
}

/// Folders of the projects this device knows about.
pub fn project_dirs(paths: &crate::Paths) -> Vec<std::path::PathBuf> {
    crate::projects::list(paths).iter().filter_map(|f| f.parent().map(Path::to_path_buf)).collect()
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
        let base: EnvMap = crate::env::from_pairs([
            ("PATH", std::env::join_paths(["/usr/bin", "/bin"]).unwrap()),
            ("SHELL", OsString::from("/bin/sh")),
        ]);
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

    #[test]
    fn missing_folder_is_reported_with_a_suggestion() {
        let d = tempfile::tempdir().unwrap();
        let paths = crate::Paths { config_file: d.path().join("b.toml"), data_dir: d.path().join("data") };
        let work = d.path().join("work");
        std::fs::create_dir_all(work.join("api")).unwrap();
        std::fs::create_dir_all(work.join("sam2")).unwrap();
        let bm = |cwd: &str| Bookmark { cwd: Some(cwd.into()), command: Some("ls".into()), ..Default::default() };
        crate::store::upsert(&paths.config_file, "api", &bm(&work.join("api").display().to_string())).unwrap();
        crate::store::upsert(&paths.config_file, "train", &bm(r"Z:\dev\sam2")).unwrap();
        let lib = Library::load(&paths, &[]);
        let base = crate::env::from_pairs([("PATH", std::env::join_paths(["/usr/bin", "/bin"]).unwrap())]);
        let report = check_library(&lib, &base, d.path(), &paths);
        let train = report.checks.iter().find(|c| c.name == "train").unwrap();
        let m = train.missing_folder.as_ref().unwrap();
        assert_eq!(m.written, r"Z:\dev\sam2");
        // Found next to the folder of a bookmark that works here.
        assert_eq!(m.suggestion.as_deref(), Some(paths::contract_tilde(&work.join("sam2")).as_str()));
        assert!(train.problems[0].contains("on this device"), "{:?}", train.problems);
        assert!(report.checks.iter().find(|c| c.name == "api").unwrap().missing_folder.is_none());
    }

    #[test]
    fn shell_language_mismatches() {
        let b = Bookmark { command: Some("export A=1 && make".into()), ..Default::default() };
        let base = EnvMap::new();
        // Classified by the shell's name, even where it isn't installed.
        let m = shell_mismatch(&b, Os::current(), ShellKind::Pwsh).unwrap();
        assert!(m.message(true).contains("runs in PowerShell here"), "{}", m.message(true));
        assert!(shell_mismatch(&b, Os::current(), ShellKind::Posix).is_none());

        // A Mac user learns the bookmark won't run on their Windows PC, until it has a Windows command.
        let on_windows = shell_mismatches(&b, &Settings::default(), &base, &[Os::Windows, Os::Linux]);
        assert_eq!(on_windows.len(), 1);
        assert!(
            on_windows[0].message(false).contains("on Windows; add a Windows command"),
            "{}",
            on_windows[0].message(false)
        );
        let fixed = Bookmark {
            windows: Some(crate::model::OsVariant { command: Some("$env:A = 1; make".into()), ..Default::default() }),
            ..b.clone()
        };
        assert!(shell_mismatches(&fixed, &Settings::default(), &base, &[Os::Windows, Os::Linux]).is_empty());
    }
}
