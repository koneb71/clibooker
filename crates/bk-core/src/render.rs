//! Render a bookmark as shell text for an interactive shell: the one-liner used for
//! "pop out to terminal" / copy, and the `bk go` snippet that enters the bookmark's context.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::env::{EnvMap, conda, detect, runtimes, venv};
use crate::model::EnvSpec;
use crate::paths;
use crate::store::Entry;
use crate::template;

/// Single-quote when needed. Also quotes words zsh/csh would expand even though POSIX
/// shells don't: a leading `=` (zsh `=cmd`), a leading `~`, and `!` (csh history).
pub fn q(s: &str) -> String {
    let escaped = shell_escape::unix::escape(Cow::Borrowed(s)).into_owned();
    if !escaped.starts_with('\'') && (s.starts_with('=') || s.starts_with('~') || s.contains('!')) {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        escaped
    }
}

/// Quote a path, keeping a leading `~/` unquoted so it still expands.
pub fn q_path(p: &Path) -> String {
    let s = paths::contract_tilde(p);
    match s.strip_prefix("~/") {
        Some(rest) => format!("~/{}", q(rest)),
        None if s == "~" => s,
        None => q(&s),
    }
}

/// Double-quote but keep `${VAR}` expansion working.
fn dq(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '\\' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `cd`, activation, dotenv and vars, as separate commands.
pub fn prelude(entry: &Entry, default_cwd: &Path, base: &EnvMap) -> Vec<String> {
    let b = &entry.bookmark;
    let cwd = entry.cwd().unwrap_or_else(|| default_cwd.to_path_buf());
    let mut parts = vec![format!("cd {}", q_path(&cwd))];
    let spec = match &b.env {
        EnvSpec::Auto => detect::from_folder(&cwd, base).unwrap_or_default(),
        other => other.clone(),
    };
    match &spec {
        EnvSpec::Inherit | EnvSpec::Auto => {}
        EnvSpec::None => parts.push("{ deactivate 2>/dev/null; conda deactivate 2>/dev/null; true; }".into()),
        EnvSpec::Conda { name: Some(n), prefix: None } => parts.push(format!("conda activate {}", q(n))),
        EnvSpec::Conda { prefix: Some(p), .. } => {
            parts.push(format!("conda activate {}", q_path(&paths::resolve(p, &cwd))))
        }
        EnvSpec::Conda { .. } => {}
        EnvSpec::Venv { path } => parts.push(format!(". {}", q_path(&paths::resolve(path, &cwd).join("bin/activate")))),
        EnvSpec::Uv { sync } => {
            if *sync {
                parts.push("uv sync".into());
            }
            parts.push(format!(". {}", q_path(&venv::uv_env_dir(&cwd, base).join("bin/activate"))));
        }
        EnvSpec::Poetry => parts.push(r#". "$(poetry env info --path)/bin/activate""#.into()),
        EnvSpec::Pyenv { version } => parts.push(format!("export PYENV_VERSION={}", q(version))),
        EnvSpec::Mise => parts.push(r#"eval "$(mise env -s bash)""#.into()),
    }
    for (lang, spec) in &b.runtimes {
        match runtimes::resolve(lang, spec, &cwd, base) {
            Ok(Some(r)) => {
                for (k, v) in &r.vars {
                    parts.push(format!("export {k}={}", q(v)));
                }
                if !r.bin_dirs.is_empty() {
                    let dirs: Vec<String> = r.bin_dirs.iter().map(|d| q_path(d)).collect();
                    parts.push(format!("export PATH={}:\"$PATH\"", dirs.join(":")));
                }
            }
            Ok(None) => {}
            Err(e) => parts.push(format!(": 'bk: {}'", format!("{e:#}").replace('\'', ""))),
        }
    }
    if !b.path.is_empty() {
        let dirs: Vec<String> = b.path.iter().map(|p| q_path(&paths::resolve(p, &cwd))).collect();
        parts.push(format!("export PATH={}:\"$PATH\"", dirs.join(":")));
    }
    if let Some(f) = &b.dotenv {
        parts.push(format!("set -a && . {} && set +a", q_path(&paths::resolve(f, &cwd))));
    }
    for (k, v) in &b.vars {
        let val = if v.contains("${") { dq(v) } else { q(v) };
        parts.push(format!("export {k}={val}"));
    }
    parts
}

/// The full workflow as one line, wrapped in a subshell so it doesn't change the caller's
/// directory or env: `( cd X && conda activate Y && { a && b; c; } )`.
pub fn one_liner(
    entry: &Entry,
    params: &BTreeMap<String, String>,
    extra_args: &[String],
    default_cwd: &Path,
    base: &EnvMap,
) -> Result<String> {
    Ok(format!("( {} )", script(entry, params, extra_args, default_cwd, base)?))
}

/// Same as [`one_liner`] without the subshell, for a fresh terminal window where the
/// user should stay in the bookmark's folder and env afterwards.
pub fn script(
    entry: &Entry,
    params: &BTreeMap<String, String>,
    extra_args: &[String],
    default_cwd: &Path,
    base: &EnvMap,
) -> Result<String> {
    let steps = entry.bookmark.effective_steps();
    let last = steps.len().saturating_sub(1);
    let mut rendered = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        let mut cmd = template::render(&s.run, params).map_err(anyhow::Error::msg)?;
        if i == last {
            for a in extra_args {
                cmd.push(' ');
                cmd.push_str(&q(a));
            }
        }
        if let Some(c) = &s.cwd {
            let c = template::render(c, params).map_err(anyhow::Error::msg)?;
            cmd = format!("( cd {} && {cmd} )", q(&c));
        }
        rendered.push((cmd, s.continue_on_error));
    }
    let body = if rendered.len() == 1 {
        rendered.remove(0).0
    } else {
        let mut s = String::from("{ ");
        for (i, (cmd, keep_going)) in rendered.iter().enumerate() {
            s.push_str(cmd);
            if i < rendered.len() - 1 {
                s.push_str(if *keep_going { "; " } else { " && " });
            }
        }
        s.push_str("; }");
        s
    };
    let mut parts = prelude(entry, default_cwd, base);
    parts.push(body);
    Ok(parts.join(" && "))
}

/// Snippet for `bk go`: enter the bookmark's directory and env in the current shell.
pub fn enter_script(entry: &Entry, default_cwd: &Path, base: &EnvMap) -> String {
    prelude(entry, default_cwd, base).join(" && ")
}

/// Which shell language to render for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    PowerShell,
}

impl Dialect {
    /// What an interactive terminal on this OS speaks.
    pub fn native() -> Self {
        if cfg!(windows) { Dialect::PowerShell } else { Dialect::Posix }
    }
}

/// `conda activate` needs conda's shell function, which non-interactive shells (and shells
/// where `conda init` never ran) don't have: load it first when the bookmark uses conda.
pub fn with_conda_hook(entry: &Entry, script: String, base: &EnvMap) -> String {
    if !matches!(entry.bookmark.env, EnvSpec::Conda { .. }) {
        return script;
    }
    let Some(conda_base) = conda::find_base(base) else { return script };
    let sh = q_path(&conda_base.join("etc").join("profile.d").join("conda.sh"));
    format!("case \"$(type conda 2>/dev/null)\" in *function*) ;; *) . {sh} ;; esac && {script}")
}

/// [`enter_script`] in either dialect, ready to `eval` / `Invoke-Expression`.
pub fn enter_script_for(d: Dialect, entry: &Entry, default_cwd: &Path, base: &EnvMap) -> String {
    match d {
        Dialect::Posix => with_conda_hook(entry, enter_script(entry, default_cwd, base), base),
        Dialect::PowerShell => crate::render_pwsh::enter_script(entry, default_cwd, base),
    }
}

/// [`script`] in either dialect (a new terminal window's command).
pub fn script_for(
    d: Dialect,
    entry: &Entry,
    params: &BTreeMap<String, String>,
    extra_args: &[String],
    default_cwd: &Path,
    base: &EnvMap,
) -> Result<String> {
    match d {
        Dialect::Posix => Ok(with_conda_hook(entry, script(entry, params, extra_args, default_cwd, base)?, base)),
        Dialect::PowerShell => crate::render_pwsh::script(entry, params, extra_args, default_cwd, base),
    }
}

/// A program and its arguments as one quoted command line, e.g. an `ssh` invocation:
/// `'ssh' '-e' 'none' …` or PowerShell's `& 'C:\…\ssh.exe' -e none …`.
pub fn command_line(d: Dialect, program: &str, args: &[String]) -> String {
    match d {
        Dialect::Posix => {
            std::iter::once(program).chain(args.iter().map(String::as_str)).map(q).collect::<Vec<_>>().join(" ")
        }
        Dialect::PowerShell => std::iter::once(format!("& {}", crate::shell::pwsh_string(program)))
            .chain(args.iter().map(|a| crate::shell::pwsh_quote(a)))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// [`one_liner`] in either dialect (copy to clipboard).
pub fn one_liner_for(
    d: Dialect,
    entry: &Entry,
    params: &BTreeMap<String, String>,
    extra_args: &[String],
    default_cwd: &Path,
    base: &EnvMap,
) -> Result<String> {
    match d {
        Dialect::Posix => one_liner(entry, params, extra_args, default_cwd, base),
        Dialect::PowerShell => crate::render_pwsh::one_liner(entry, params, extra_args, default_cwd, base),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::model::{Bookmark, Step};
    use crate::store::Scope;

    fn entry(b: Bookmark) -> Entry {
        Entry::new("t", Scope::Global, Path::new("/tmp/b.toml"), b)
    }

    #[test]
    fn one_liner_conda_workflow() {
        let b = Bookmark {
            cwd: Some("/w/my proj".into()),
            env: EnvSpec::Conda { name: Some("sam2".into()), prefix: None },
            steps: vec![
                Step::new("make {{t:build}}"),
                Step { run: "flaky".into(), continue_on_error: true, ..Default::default() },
                Step::new("echo done"),
            ],
            dotenv: Some(".env".into()),
            vars: [("A".to_string(), "x y".to_string()), ("P".to_string(), "${P}:src".to_string())].into(),
            ..Default::default()
        };
        let s = one_liner(&entry(b), &BTreeMap::new(), &["--v".into()], Path::new("/"), &EnvMap::new()).unwrap();
        assert_eq!(
            s,
            "( cd '/w/my proj' && conda activate sam2 && set -a && . '/w/my proj/.env' && set +a && export A='x y' && export P=\"${P}:src\" && { make build && flaky; echo done --v; } )"
        );
    }

    #[test]
    fn quoting_for_zsh_and_csh() {
        assert_eq!(q("=bk-x"), "'=bk-x'");
        assert_eq!(q("~foo"), "'~foo'");
        // shell_escape already escapes `!` outside the quotes, which csh needs.
        assert_eq!(q("hi!"), "'hi'\\!''");
        assert_eq!(q("plain"), "plain");
        assert_eq!(q("it's"), "'it'\\''s'");
    }

    #[test]
    fn single_step_and_home_paths() {
        let home = paths::home();
        let b = Bookmark {
            cwd: Some("~/w".into()),
            env: EnvSpec::Venv { path: ".venv".into() },
            command: Some("python t.py".into()),
            ..Default::default()
        };
        let e = entry(b);
        let s = one_liner(&e, &BTreeMap::new(), &[], &home, &EnvMap::new()).unwrap();
        assert_eq!(s, "( cd ~/w && . ~/w/.venv/bin/activate && python t.py )");
        assert_eq!(enter_script(&e, &home, &EnvMap::new()), "cd ~/w && . ~/w/.venv/bin/activate");
    }

    #[test]
    fn runtimes_and_path_in_one_liner() {
        let b = Bookmark {
            cwd: Some("/w/app".into()),
            runtimes: [("rust".to_string(), "nightly-zzz".to_string())].into(),
            path: vec!["vendor/bin".into()],
            command: Some("make".into()),
            ..Default::default()
        };
        let s = one_liner(&entry(b), &BTreeMap::new(), &[], Path::new("/"), &EnvMap::new()).unwrap();
        // An unresolvable runtime becomes a visible no-op note rather than breaking the line.
        assert!(s.contains(": 'bk: rust nightly-zzz is not installed"), "{s}");
        assert!(s.contains("export PATH=/w/app/vendor/bin:\"$PATH\""), "{s}");
    }
}
