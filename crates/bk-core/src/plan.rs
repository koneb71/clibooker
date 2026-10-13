//! Turn a bookmark + run-time inputs into a concrete, executable plan.
//!
//! Both front ends execute the same plan: the CLI with inherited stdio, the desktop app in a PTY.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;

use crate::env::{self, Activated, EnvMap, Mode};
use crate::model::Settings;
use crate::paths;
use crate::store::Entry;
use crate::template;

#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    pub params: BTreeMap<String, String>,
    /// Appended (shell-quoted) to the last step.
    pub extra_args: Vec<String>,
    /// `KEY=VALUE` overrides applied last.
    pub overrides: Vec<(String, String)>,
    /// Used when the bookmark doesn't pin a `cwd` (CLI: current dir, app: home).
    pub default_cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedStep {
    pub label: String,
    pub command: String,
    pub cwd: PathBuf,
    pub continue_on_error: bool,
    /// Added by bk itself (e.g. `uv sync`), not written by the user.
    pub synthetic: bool,
}

#[derive(Debug, Clone)]
pub struct RunPlan {
    pub name: String,
    pub entry_id: String,
    pub cwd: PathBuf,
    pub shell: PathBuf,
    pub shell_args: Vec<String>,
    pub env: EnvMap,
    pub env_label: String,
    pub env_description: String,
    pub env_changes: Vec<String>,
    pub params: BTreeMap<String, String>,
    pub steps: Vec<PlannedStep>,
}

impl RunPlan {
    /// argv for one step: `<shell> <shell_args...> <command>`.
    pub fn argv(&self, step: &PlannedStep) -> Vec<OsString> {
        let mut v = vec![self.shell.clone().into_os_string()];
        v.extend(self.shell_args.iter().map(OsString::from));
        v.push(step.command.clone().into());
        v
    }

    pub fn summary(&self) -> PlanSummary {
        PlanSummary {
            name: self.name.clone(),
            cwd: paths::contract_tilde(&self.cwd),
            shell: self.shell.display().to_string(),
            env_label: self.env_label.clone(),
            env_description: self.env_description.clone(),
            env_changes: self.env_changes.clone(),
            steps: self.steps.clone(),
        }
    }
}

/// Serializable view of a plan (for dry-run and the UI). Never contains env values.
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub name: String,
    pub cwd: String,
    pub shell: String,
    pub env_label: String,
    pub env_description: String,
    pub env_changes: Vec<String>,
    pub steps: Vec<PlannedStep>,
}

pub fn build(entry: &Entry, req: &RunRequest, base: &EnvMap, settings: &Settings, mode: Mode) -> Result<RunPlan> {
    let b = &entry.bookmark;
    b.validate().map_err(anyhow::Error::msg)?;
    if b.is_remote() {
        bail!("`{}` runs on a server; build it with remote::build", entry.name);
    }

    let cwd = entry.cwd().unwrap_or_else(|| req.default_cwd.clone());
    if !cwd.is_dir() {
        bail!("working directory {} does not exist", cwd.display());
    }

    let Activated { mut env, spec, pre_steps, description } = env::activate(&b.env, &cwd, base, mode)?;

    // Language runtimes, then extra PATH entries, go in front of the activated env.
    let runtimes = env::runtimes::apply(&mut env, &b.runtimes, &cwd)?;
    for p in b.path.iter().rev() {
        env::prepend_path(&mut env, &paths::resolve(p, &cwd));
    }
    let mut env_label = spec.label();
    let mut description = description;
    for r in &runtimes {
        env_label = if spec.is_inherit() && env_label == "inherit" {
            r.label.clone()
        } else {
            format!("{env_label} + {}", r.label)
        };
        description = format!("{description} · {}", r.description);
    }

    if let Some(f) = &b.dotenv {
        let path = paths::resolve(f, &cwd);
        let iter = dotenvy::from_path_iter(&path).with_context(|| format!("reading dotenv file {}", path.display()))?;
        for item in iter {
            let (k, v) = item.with_context(|| format!("parsing {}", path.display()))?;
            env.insert(k.into(), v.into());
        }
    }
    for (k, v) in &b.vars {
        let v = expand_vars(v, &env);
        env.insert(k.into(), v.into());
    }
    for (k, v) in &req.overrides {
        env.insert(k.into(), v.into());
    }

    let values = &req.params;
    let mut steps: Vec<PlannedStep> = pre_steps
        .into_iter()
        .map(|c| PlannedStep {
            label: c.clone(),
            command: c,
            cwd: cwd.clone(),
            continue_on_error: false,
            synthetic: true,
        })
        .collect();
    let user_steps = b.effective_steps();
    let last = user_steps.len().saturating_sub(1);
    for (i, s) in user_steps.iter().enumerate() {
        let mut command = template::render(&s.run, values).map_err(anyhow::Error::msg)?;
        if i == last && !req.extra_args.is_empty() {
            for a in &req.extra_args {
                command.push(' ');
                command.push_str(&shell_escape::unix::escape(a.into()));
            }
        }
        let step_cwd = match &s.cwd {
            Some(c) => paths::resolve(&template::render(c, values).map_err(anyhow::Error::msg)?, &cwd),
            None => cwd.clone(),
        };
        let label = s.name.clone().unwrap_or_else(|| command.lines().next().unwrap_or_default().to_string());
        steps.push(PlannedStep {
            label,
            command,
            cwd: step_cwd,
            continue_on_error: s.continue_on_error,
            synthetic: false,
        });
    }

    let shell_name = b.shell.clone().or_else(|| settings.shell.clone()).unwrap_or_else(|| default_shell(base));
    let (shell, shell_args) = resolve_shell(&shell_name, &env)?;
    let env_changes = env_diff(base, &env);
    let mut params = values.clone();
    for p in template::bookmark_params(b) {
        if let (false, Some(d)) = (params.contains_key(&p.name), p.default) {
            params.insert(p.name, d);
        }
    }

    Ok(RunPlan {
        name: entry.name.clone(),
        entry_id: entry.id.clone(),
        cwd,
        shell,
        shell_args,
        env,
        env_label,
        env_description: description,
        env_changes,
        params,
        steps,
    })
}

/// `$SHELL` if it is zsh or bash, else `sh`.
pub fn default_shell(env: &EnvMap) -> String {
    let shell = env::get_string(env, "SHELL").unwrap_or_default();
    let name = Path::new(&shell).file_name().and_then(OsStr::to_str).unwrap_or("");
    match name {
        "zsh" | "bash" => name.to_string(),
        _ => "sh".to_string(),
    }
}

/// Resolve a shell name/path and the flags that make it skip rc files.
pub fn resolve_shell(name: &str, env: &EnvMap) -> Result<(PathBuf, Vec<String>)> {
    let path = if name.contains('/') {
        paths::expand_tilde(name)
    } else {
        env::which_in(env, name)
            .or_else(|| {
                ["/bin", "/usr/bin", "/opt/homebrew/bin"].iter().map(|d| Path::new(d).join(name)).find(|p| p.exists())
            })
            .with_context(|| format!("shell `{name}` not found"))?
    };
    if !path.exists() {
        bail!("shell {} not found", path.display());
    }
    let base = path.file_name().and_then(OsStr::to_str).unwrap_or("");
    let args: &[&str] = match base {
        "zsh" => &["-f", "-c"],
        "bash" => &["--noprofile", "--norc", "-c"],
        "fish" => &["--no-config", "-c"],
        _ => &["-c"],
    };
    Ok((path, args.iter().map(|s| s.to_string()).collect()))
}

static VAR_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex"));

/// Expand `${NAME}` references against the env built so far (e.g. `PYTHONPATH = "${PYTHONPATH}:src"`).
pub fn expand_vars(value: &str, env: &EnvMap) -> String {
    VAR_REF.replace_all(value, |c: &regex::Captures| env::get_string(env, &c[1]).unwrap_or_default()).into_owned()
}

/// Human-readable list of env changes, never including values (they may be secrets).
pub fn env_diff(before: &EnvMap, after: &EnvMap) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in after {
        let key = k.to_string_lossy();
        match before.get(k) {
            None => out.push(format!("+{key}")),
            Some(old) if old != v => {
                if key == "PATH" {
                    let old_entries: Vec<PathBuf> = std::env::split_paths(old).collect();
                    let added: Vec<String> = std::env::split_paths(v)
                        .filter(|p| !old_entries.contains(p))
                        .map(|p| paths::contract_tilde(&p))
                        .collect();
                    if added.is_empty() {
                        out.push("~PATH".into());
                    } else {
                        out.push(format!("~PATH (+{})", added.join(", +")));
                    }
                } else {
                    out.push(format!("~{key}"));
                }
            }
            _ => {}
        }
    }
    for k in before.keys() {
        if !after.contains_key(k) && !k.as_encoded_bytes().starts_with(b"BK_") {
            out.push(format!("-{}", k.to_string_lossy()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bookmark, EnvSpec, Step};
    use crate::store::Scope;

    fn entry(b: Bookmark, dir: &Path) -> Entry {
        let mut e = Entry::new("t", Scope::Global, &dir.join("b.toml"), b);
        e.base_dir = dir.to_path_buf();
        e
    }

    fn base() -> EnvMap {
        [
            ("PATH".into(), OsString::from("/usr/bin:/bin")),
            ("SHELL".into(), "/bin/zsh".into()),
            ("VIRTUAL_ENV".into(), "/old".into()),
        ]
        .into()
    }

    #[test]
    fn builds_steps_params_and_extra_args() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("web")).unwrap();
        let b = Bookmark {
            cwd: Some(".".into()),
            steps: vec![
                Step::new("echo {{a}}"),
                Step { run: "npm run {{script:dev}}".into(), cwd: Some("web".into()), ..Default::default() },
            ],
            ..Default::default()
        };
        let req = RunRequest {
            params: [("a".to_string(), "1".to_string())].into(),
            extra_args: vec!["--port".into(), "80 80".into()],
            ..Default::default()
        };
        let plan = build(&entry(b, d.path()), &req, &base(), &Settings::default(), Mode::Check).unwrap();
        assert_eq!(plan.steps[0].command, "echo 1");
        assert_eq!(plan.steps[1].command, "npm run dev --port '80 80'");
        assert_eq!(plan.steps[1].cwd, d.path().join("web"));
        assert_eq!(plan.shell_args, vec!["-f", "-c"]);
        assert_eq!(plan.params["script"], "dev");
        // Inherit keeps the caller's env.
        assert_eq!(env::get_string(&plan.env, "VIRTUAL_ENV").unwrap(), "/old");
    }

    #[test]
    fn missing_param_is_error() {
        let d = tempfile::tempdir().unwrap();
        let b = Bookmark { command: Some("echo {{x}}".into()), cwd: Some(".".into()), ..Default::default() };
        let err = build(&entry(b, d.path()), &RunRequest::default(), &base(), &Settings::default(), Mode::Check)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`x`"), "{err}");
    }

    #[test]
    fn layering_venv_dotenv_vars_overrides() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join(".venv");
        std::fs::create_dir_all(v.join("bin")).unwrap();
        std::fs::write(v.join("pyvenv.cfg"), "").unwrap();
        std::fs::write(d.path().join(".env"), "SECRET=s3\nMODE=from-dotenv\n").unwrap();
        let b = Bookmark {
            command: Some("python x.py".into()),
            cwd: Some(".".into()),
            env: EnvSpec::Venv { path: ".venv".into() },
            dotenv: Some(".env".into()),
            vars: [("MODE".to_string(), "from-vars".to_string()), ("P".to_string(), "${SECRET}:x".to_string())].into(),
            ..Default::default()
        };
        let req = RunRequest { overrides: vec![("Z".into(), "1".into())], ..Default::default() };
        let plan = build(&entry(b, d.path()), &req, &base(), &Settings::default(), Mode::Run).unwrap();
        let get = |k| env::get_string(&plan.env, k).unwrap();
        assert_eq!(get("VIRTUAL_ENV"), v.display().to_string());
        assert!(get("PATH").starts_with(&format!("{}/bin:", v.display())));
        assert_eq!(get("MODE"), "from-vars");
        assert_eq!(get("P"), "s3:x");
        assert_eq!(get("Z"), "1");
        assert!(plan.env_changes.iter().any(|c| c.starts_with("~PATH (+")));
        assert!(plan.env_changes.iter().all(|c| !c.contains("s3")));
    }

    #[test]
    fn runtimes_and_extra_path_come_first() {
        let d = tempfile::tempdir().unwrap();
        let nvm = d.path().join("nvm");
        std::fs::create_dir_all(nvm.join("versions/node/v22.14.0/bin")).unwrap();
        std::fs::create_dir_all(d.path().join("vendor/bin")).unwrap();
        let b = Bookmark {
            command: Some("node -v".into()),
            cwd: Some(".".into()),
            runtimes: [("node".to_string(), "22".to_string())].into(),
            path: vec!["vendor/bin".into()],
            ..Default::default()
        };
        let mut base = base();
        base.insert("NVM_DIR".into(), nvm.clone().into_os_string());
        let plan = build(&entry(b, d.path()), &RunRequest::default(), &base, &Settings::default(), Mode::Run).unwrap();
        let path = env::get_string(&plan.env, "PATH").unwrap();
        let expected =
            format!("{}:{}:", d.path().join("vendor/bin").display(), nvm.join("versions/node/v22.14.0/bin").display());
        assert!(path.starts_with(&expected), "{path}");
        assert_eq!(plan.env_label, "node 22.14.0");
        assert!(plan.env_description.contains("node 22.14.0 (nvm)"), "{}", plan.env_description);
    }

    #[test]
    fn missing_cwd_is_error() {
        let d = tempfile::tempdir().unwrap();
        let b = Bookmark { command: Some("ls".into()), cwd: Some("nope".into()), ..Default::default() };
        assert!(
            build(&entry(b, d.path()), &RunRequest::default(), &base(), &Settings::default(), Mode::Check).is_err()
        );
    }
}
