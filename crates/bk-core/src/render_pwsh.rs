//! PowerShell versions of [`crate::render`]: `bk go` in a PowerShell session, and "Open in
//! terminal" / copy on Windows. Written for Windows PowerShell 5.1 too (no `&&`, `||`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::env::{EnvMap, conda, detect, runtimes, venv};
use crate::model::EnvSpec;
use crate::paths;
use crate::shell::{pwsh_quote, pwsh_string};
use crate::store::Entry;
use crate::template;

fn qp(p: &Path) -> String {
    pwsh_string(&p.display().to_string())
}

fn sep() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

/// `$env:PATH = 'a;b;' + $env:PATH`
fn prepend_path(dirs: &[std::path::PathBuf]) -> String {
    let joined: String = dirs.iter().map(|d| format!("{}{}", d.display(), sep())).collect();
    format!("$env:PATH = {} + $env:PATH", pwsh_string(&joined))
}

/// A double-quoted PowerShell string in which `${NAME}` reads the environment variable.
fn expanding(value: &str) -> String {
    let mut out = String::from("\"");
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let (before, after) = rest.split_at(start);
        push_escaped(&mut out, before);
        match after[2..].find('}') {
            Some(end) if after[2..2 + end].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && end > 0 => {
                out.push_str(&format!("$($env:{})", &after[2..2 + end]));
                rest = &after[3 + end..];
            }
            _ => {
                push_escaped(&mut out, "${");
                rest = &after[2..];
            }
        }
    }
    push_escaped(&mut out, rest);
    out.push('"');
    out
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        if matches!(c, '`' | '"' | '$' | '\u{201C}' | '\u{201D}' | '\u{201E}') {
            out.push('`');
        }
        out.push(c);
    }
}

/// `Write-Warning '...'` on one line (scripts are split on newlines).
fn warning(msg: &str) -> String {
    format!("Write-Warning {}", pwsh_string(&msg.replace(['\r', '\n'], " ")))
}

fn activate_script(venv_dir: &Path) -> std::path::PathBuf {
    venv::bin_dir(venv_dir).join("Activate.ps1")
}

/// `Set-Location`, activation, runtimes, dotenv and vars, one statement each.
pub fn prelude(entry: &Entry, default_cwd: &Path, base: &EnvMap) -> Vec<String> {
    let b = &entry.bookmark;
    let cwd = entry.cwd().unwrap_or_else(|| default_cwd.to_path_buf());
    let mut parts = vec![format!("Set-Location -LiteralPath {}", qp(&cwd))];
    let spec = match &b.env {
        EnvSpec::Auto => detect::from_folder(&cwd, base).unwrap_or_default(),
        other => other.clone(),
    };
    match &spec {
        EnvSpec::Inherit | EnvSpec::Auto => {}
        EnvSpec::None => parts.push(
            "if (Test-Path Function:\\deactivate) { deactivate }; if (Get-Module -Name Conda) { conda deactivate }"
                .into(),
        ),
        EnvSpec::Conda { name, prefix } => {
            let target = match (name, prefix) {
                (_, Some(p)) => Some(paths::resolve(p, &cwd).display().to_string()),
                (Some(n), None) => Some(n.clone()),
                (None, None) => None,
            };
            if let Some(target) = target {
                if let Some(conda_base) = conda::find_base(base) {
                    let exe = if cfg!(windows) {
                        conda_base.join("Scripts").join("conda.exe")
                    } else {
                        conda_base.join("bin").join("conda")
                    };
                    parts.push(format!(
                        "if (-not (Get-Module -Name Conda)) {{ (& {} shell.powershell hook) | Out-String | Invoke-Expression }}",
                        qp(&exe)
                    ));
                }
                parts.push(format!("conda activate {}", pwsh_quote(&target)));
            }
        }
        EnvSpec::Venv { path } => {
            parts.push(format!("& {}", qp(&activate_script(&venv::root_of(paths::resolve(path, &cwd))))))
        }
        EnvSpec::Uv { sync } => {
            if *sync {
                parts.push("uv sync".into());
            }
            parts.push(format!("& {}", qp(&activate_script(&venv::uv_env_dir(&cwd, base)))));
        }
        EnvSpec::Poetry => {
            let sub = if cfg!(windows) { "Scripts" } else { "bin" };
            parts.push(format!("& (Join-Path (poetry env info --path) '{sub}/Activate.ps1')"));
        }
        EnvSpec::Pyenv { version } => parts.push(format!("$env:PYENV_VERSION = {}", pwsh_string(version))),
        EnvSpec::Mise => parts.push("mise env -s pwsh | Out-String | Invoke-Expression".into()),
    }
    for (lang, spec) in &b.runtimes {
        match runtimes::resolve(lang, spec, &cwd, base) {
            Ok(Some(r)) => {
                for (k, v) in &r.vars {
                    parts.push(format!("$env:{k} = {}", pwsh_string(v)));
                }
                if !r.bin_dirs.is_empty() {
                    parts.push(prepend_path(&r.bin_dirs));
                }
            }
            Ok(None) => {}
            Err(e) => parts.push(warning(&format!("bk: {e:#}"))),
        }
    }
    if !b.path.is_empty() {
        let dirs: Vec<_> = b.path.iter().map(|p| paths::resolve(p, &cwd)).collect();
        parts.push(prepend_path(&dirs));
    }
    if let Some(f) = &b.dotenv {
        let file = paths::resolve(f, &cwd);
        match dotenvy::from_path_iter(&file) {
            Ok(iter) => {
                for item in iter {
                    match item {
                        Ok((k, v)) => parts.push(format!("$env:{k} = {}", pwsh_string(&v))),
                        Err(e) => parts.push(warning(&format!("bk: {}: {e}", file.display()))),
                    }
                }
            }
            Err(e) => parts.push(warning(&format!("bk: {}: {e}", file.display()))),
        }
    }
    for (k, v) in &b.vars {
        let val = if v.contains("${") { expanding(v) } else { pwsh_string(v) };
        parts.push(format!("$env:{k} = {val}"));
    }
    parts
}

/// The steps after the prelude. A failing step stops the rest (`throw`), like `&&` would.
pub fn script(
    entry: &Entry,
    params: &BTreeMap<String, String>,
    extra_args: &[String],
    default_cwd: &Path,
    base: &EnvMap,
) -> Result<String> {
    let steps = entry.bookmark.effective_steps();
    let last = steps.len().saturating_sub(1);
    let mut lines = prelude(entry, default_cwd, base);
    for (i, s) in steps.iter().enumerate() {
        let mut cmd = template::render(&s.run, params).map_err(anyhow::Error::msg)?;
        if i == last {
            for a in extra_args {
                cmd.push(' ');
                cmd.push_str(&pwsh_quote(a));
            }
        }
        if let Some(c) = &s.cwd {
            let c = template::render(c, params).map_err(anyhow::Error::msg)?;
            cmd = format!("Push-Location -LiteralPath {}; {cmd}; Pop-Location", pwsh_string(&c));
        }
        lines.push(cmd);
        if !s.continue_on_error && i < last {
            lines.push(format!("if (-not $?) {{ throw 'bk: step {} failed' }}", i + 1));
        }
    }
    Ok(lines.join("\n"))
}

/// [`script`] on one line, in a script block that puts the location back afterwards.
pub fn one_liner(
    entry: &Entry,
    params: &BTreeMap<String, String>,
    extra_args: &[String],
    default_cwd: &Path,
    base: &EnvMap,
) -> Result<String> {
    let body = script(entry, params, extra_args, default_cwd, base)?.replace('\n', "; ");
    Ok(format!("& {{ $bkHere = Get-Location; try {{ {body} }} finally {{ Set-Location $bkHere }} }}"))
}

/// `bk go` in PowerShell: enter the bookmark's folder and env in the current session.
pub fn enter_script(entry: &Entry, default_cwd: &Path, base: &EnvMap) -> String {
    prelude(entry, default_cwd, base).join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bookmark, Step};
    use crate::store::Scope;

    fn entry(b: Bookmark, dir: &Path) -> Entry {
        let mut e = Entry::new("t", Scope::Global, &dir.join("b.toml"), b);
        e.base_dir = dir.to_path_buf();
        e
    }

    #[test]
    fn venv_dotenv_vars_and_steps() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".env"), "TOKEN=\"it's\"\n").unwrap();
        let b = Bookmark {
            cwd: Some(".".into()),
            env: EnvSpec::Venv { path: ".venv".into() },
            dotenv: Some(".env".into()),
            vars: [("P".to_string(), "${HOME}/x \"y\"".to_string())].into(),
            steps: vec![
                Step::new("npm run {{s}}"),
                Step { run: "make".into(), cwd: Some("web".into()), ..Default::default() },
            ],
            ..Default::default()
        };
        let params = [("s".to_string(), "dev".to_string())].into();
        let s = script(&entry(b, d.path()), &params, &["a b".into()], d.path(), &EnvMap::new()).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("Set-Location -LiteralPath '"), "{s}");
        assert!(lines[1].starts_with("& '") && lines[1].ends_with("Activate.ps1'"), "{s}");
        assert!(lines.contains(&"$env:TOKEN = 'it''s'"), "{s}");
        assert!(lines.contains(&"$env:P = \"$($env:HOME)/x `\"y`\"\""), "{s}");
        assert!(lines.contains(&"npm run dev"), "{s}");
        assert!(lines.contains(&"if (-not $?) { throw 'bk: step 1 failed' }"), "{s}");
        assert!(lines.contains(&"Push-Location -LiteralPath 'web'; make 'a b'; Pop-Location"), "{s}");
        let one = one_liner(
            &entry(Bookmark { command: Some("ls".into()), ..Default::default() }, d.path()),
            &BTreeMap::new(),
            &[],
            d.path(),
            &EnvMap::new(),
        )
        .unwrap();
        assert!(one.starts_with("& { $bkHere = Get-Location; try { Set-Location") && !one.contains('\n'), "{one}");
    }
}
