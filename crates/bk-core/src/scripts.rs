//! Discover runnable scripts in a project folder, so they can become bookmarks:
//! package.json (npm/pnpm/yarn/bun), deno.json, composer.json, pyproject.toml,
//! Makefile, justfile, Taskfile.yml and Procfile.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::render::q;
use crate::store::RESERVED_NAMES;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectScript {
    /// File it came from, e.g. `package.json`.
    pub source: String,
    /// Script / target name as written in the file.
    pub name: String,
    /// Command that runs it, e.g. `pnpm run dev`.
    pub command: String,
    /// What it does (the script body or a `##` comment), when known.
    pub detail: Option<String>,
    /// A valid bookmark name for it.
    pub suggested_name: String,
    /// Runtime the script needs (`node`, `php`, ...), so imports can pin it.
    pub runtime: Option<String>,
}

/// Files script discovery looks at (for fetching them from a server in one go).
pub const SCRIPT_FILES: &[&str] = &[
    "package.json",
    "deno.json",
    "deno.jsonc",
    "composer.json",
    "pyproject.toml",
    "Makefile",
    "makefile",
    "GNUmakefile",
    "justfile",
    "Justfile",
    ".justfile",
    "Taskfile.yml",
    "Taskfile.yaml",
    "taskfile.yml",
    "Procfile",
];

/// How discovery sees a folder: file contents, and whether a file exists (lockfiles can be big).
pub struct Folder<'a> {
    pub read: &'a dyn Fn(&str) -> Option<String>,
    pub exists: &'a dyn Fn(&str) -> bool,
}

/// The package manager a JS project uses: `packageManager` field, then lockfiles.
pub fn package_manager(dir: &Path) -> Option<&'static str> {
    package_manager_in(&Folder { read: &|f| fs::read_to_string(dir.join(f)).ok(), exists: &|f| dir.join(f).is_file() })
}

pub fn package_manager_in(dir: &Folder) -> Option<&'static str> {
    let text = (dir.read)("package.json")?;
    let declared = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|j| j.get("packageManager").and_then(|v| v.as_str()).map(str::to_string));
    if let Some(pm) = declared {
        for name in ["pnpm", "yarn", "bun", "npm"] {
            if pm.starts_with(name) {
                return Some(name);
            }
        }
    }
    let lock = |f: &str| (dir.exists)(f);
    Some(if lock("pnpm-lock.yaml") {
        "pnpm"
    } else if lock("yarn.lock") {
        "yarn"
    } else if lock("bun.lockb") || lock("bun.lock") {
        "bun"
    } else {
        "npm"
    })
}

/// Turn a script name into a valid bookmark name (`build:prod` → `build-prod`).
pub fn sanitize_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    let out: String = out.chars().take(56).collect();
    if out.is_empty() || RESERVED_NAMES.contains(&out.as_str()) {
        format!("run-{out}").trim_end_matches('-').to_string()
    } else {
        out
    }
}

fn script(source: &str, name: &str, command: String, detail: Option<String>, runtime: Option<&str>) -> ProjectScript {
    ProjectScript {
        source: source.into(),
        name: name.into(),
        command,
        detail: detail.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
        suggested_name: sanitize_name(name),
        runtime: runtime.map(str::to_string),
    }
}

const NPM_LIFECYCLE: &[&str] = &[
    "preinstall",
    "install",
    "postinstall",
    "prepare",
    "prepublish",
    "prepublishOnly",
    "prepack",
    "postpack",
    "preuninstall",
    "uninstall",
    "postuninstall",
    "dependencies",
];

fn package_json(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some(text) = (dir.read)("package.json") else { return };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else { return };
    let pm = package_manager_in(dir).unwrap_or("npm");
    let names: HashSet<&str> = scripts.keys().map(String::as_str).collect();
    for (name, body) in scripts {
        // Skip install hooks and pre/post hooks of other scripts.
        let hook = ["pre", "post"].iter().any(|p| name.strip_prefix(p).is_some_and(|base| names.contains(base)));
        if NPM_LIFECYCLE.contains(&name.as_str()) || hook {
            continue;
        }
        let command = match pm {
            "yarn" => format!("yarn {}", q(name)),
            other => format!("{other} run {}", q(name)),
        };
        out.push(script("package.json", name, command, body.as_str().map(str::to_string), Some("node")));
    }
}

fn deno_json(dir: &Folder, out: &mut Vec<ProjectScript>) {
    for f in ["deno.json", "deno.jsonc"] {
        let Some(text) = (dir.read)(f) else { continue };
        // Drop `//` comment lines so simple jsonc parses.
        let text: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(tasks) = json.get("tasks").and_then(|t| t.as_object()) {
            for (name, body) in tasks {
                let detail = body
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| body.get("command").and_then(|c| c.as_str()).map(str::to_string));
                out.push(script(f, name, format!("deno task {}", q(name)), detail, None));
            }
        }
        return;
    }
}

fn composer_json(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some(text) = (dir.read)("composer.json") else { return };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else { return };
    for (name, body) in scripts {
        if name.starts_with("pre-") || name.starts_with("post-") {
            continue;
        }
        let detail = match body {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Array(a) => Some(a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" && ")),
            _ => None,
        };
        out.push(script("composer.json", name, format!("composer run-script {}", q(name)), detail, Some("php")));
    }
}

fn pyproject(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some(text) = (dir.read)("pyproject.toml") else { return };
    let Ok(doc) = text.parse::<toml::Table>() else { return };
    let runner = if (dir.exists)("uv.lock") {
        "uv run "
    } else if (dir.exists)("poetry.lock") {
        "poetry run "
    } else {
        ""
    };
    let table = |path: &[&str]| -> Option<toml::Table> {
        let mut cur = &doc;
        for key in path {
            cur = cur.get(*key)?.as_table()?;
        }
        Some(cur.clone())
    };
    for path in [&["project", "scripts"][..], &["tool", "poetry", "scripts"][..]] {
        for (name, target) in table(path).unwrap_or_default() {
            out.push(script(
                "pyproject.toml",
                &name,
                format!("{runner}{}", q(&name)),
                target.as_str().map(str::to_string),
                None,
            ));
        }
    }
    for (name, task) in table(&["tool", "poe", "tasks"]).unwrap_or_default() {
        let detail =
            task.as_str().map(str::to_string).or_else(|| task.get("cmd").and_then(|c| c.as_str()).map(str::to_string));
        out.push(script("pyproject.toml", &name, format!("{runner}poe {}", q(&name)), detail, None));
    }
    for (name, cmd) in table(&["tool", "pdm", "scripts"]).unwrap_or_default() {
        if name.starts_with('_') {
            continue;
        }
        out.push(script(
            "pyproject.toml",
            &name,
            format!("pdm run {}", q(&name)),
            cmd.as_str().map(str::to_string),
            None,
        ));
    }
}

static MAKE_TARGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9][A-Za-z0-9_./-]*)\s*:([^=].*)?$").expect("regex"));

fn makefile(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some((file, text)) =
        ["Makefile", "makefile", "GNUmakefile"].iter().find_map(|f| (dir.read)(f).map(|t| (*f, t)))
    else {
        return;
    };
    let mut seen = HashSet::new();
    for line in text.lines() {
        let Some(c) = MAKE_TARGET.captures(line) else { continue };
        let name = &c[1];
        if name.contains('%') || name.contains('/') || name.contains('.') || !seen.insert(name.to_string()) {
            continue;
        }
        // `target: deps ## description`
        let detail = c.get(2).and_then(|rest| rest.as_str().split_once("##").map(|(_, d)| d.to_string()));
        out.push(script(file, name, format!("make {name}"), detail, None));
    }
}

static JUST_RECIPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@?([A-Za-z_][A-Za-z0-9_-]*)((?:\s[^:]*)?):(?:[^=].*)?$").expect("regex"));

fn justfile(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some((file, text)) = ["justfile", "Justfile", ".justfile"].iter().find_map(|f| (dir.read)(f).map(|t| (*f, t)))
    else {
        return;
    };
    let mut comment: Option<String> = None;
    for line in text.lines() {
        if let Some(c) = line.strip_prefix('#') {
            comment = Some(c.trim().to_string());
            continue;
        }
        if let Some(c) = JUST_RECIPE.captures(line) {
            let name = &c[1];
            let keyword = matches!(name, "set" | "alias" | "export" | "import" | "mod");
            if !name.starts_with('_') && !keyword {
                let params = c[2].trim();
                let detail = comment.take().or_else(|| (!params.is_empty()).then(|| format!("params: {params}")));
                out.push(script(file, name, format!("just {name}"), detail, None));
            }
        }
        comment = None;
    }
}

fn taskfile(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some((file, text)) =
        ["Taskfile.yml", "Taskfile.yaml", "taskfile.yml"].iter().find_map(|f| (dir.read)(f).map(|t| (*f, t)))
    else {
        return;
    };
    let mut in_tasks = false;
    for line in text.lines() {
        if line.starts_with("tasks:") {
            in_tasks = true;
            continue;
        }
        if !line.starts_with(' ') && !line.trim().is_empty() {
            in_tasks = false;
        }
        if in_tasks
            && let Some(rest) = line.strip_prefix("  ")
            && !rest.starts_with(' ')
            && let Some((name, _)) = rest.split_once(':')
        {
            let name = name.trim().trim_matches(|c| c == '"' || c == '\'');
            if !name.is_empty() && !name.starts_with('#') && !name.starts_with('_') {
                out.push(script(file, name, format!("task {}", q(name)), None, None));
            }
        }
    }
}

fn procfile(dir: &Folder, out: &mut Vec<ProjectScript>) {
    let Some(text) = (dir.read)("Procfile") else { return };
    for line in text.lines() {
        if let Some((name, cmd)) = line.split_once(':')
            && !name.trim().is_empty()
            && !name.starts_with('#')
            && !cmd.trim().is_empty()
        {
            out.push(script("Procfile", name.trim(), cmd.trim().to_string(), None, None));
        }
    }
}

/// All scripts found directly in `dir`.
pub fn discover(dir: &Path) -> Vec<ProjectScript> {
    discover_in(&Folder { read: &|f| fs::read_to_string(dir.join(f)).ok(), exists: &|f| dir.join(f).is_file() })
}

/// Scripts from file contents fetched elsewhere (e.g. from a server).
pub fn discover_with(read: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&str) -> bool) -> Vec<ProjectScript> {
    discover_in(&Folder { read, exists })
}

pub fn discover_in(dir: &Folder) -> Vec<ProjectScript> {
    let mut out = Vec::new();
    package_json(dir, &mut out);
    deno_json(dir, &mut out);
    composer_json(dir, &mut out);
    pyproject(dir, &mut out);
    makefile(dir, &mut out);
    justfile(dir, &mut out);
    taskfile(dir, &mut out);
    procfile(dir, &mut out);
    // Same suggested name from two sources: suffix with the source.
    let mut seen = HashSet::new();
    for s in &mut out {
        if !seen.insert(s.suggested_name.clone()) {
            let src = sanitize_name(s.source.split('.').next().unwrap_or("script")).to_lowercase();
            s.suggested_name = format!("{}-{src}", s.suggested_name);
            seen.insert(s.suggested_name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_json_with_pnpm_skips_hooks() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("package.json"),
            r#"{"scripts":{"dev":"vite","predev":"echo hi","build:prod":"vite build","postinstall":"x","test":"vitest"}}"#,
        )
        .unwrap();
        fs::write(d.path().join("pnpm-lock.yaml"), "").unwrap();
        let s = discover(d.path());
        let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
        // File order is kept.
        assert_eq!(names, vec!["dev", "build:prod", "test"]);
        assert_eq!(s[1].command, "pnpm run 'build:prod'");
        assert_eq!(s[1].suggested_name, "build-prod");
        assert_eq!(s[0].detail.as_deref(), Some("vite"));
        assert_eq!(s[0].runtime.as_deref(), Some("node"));
    }

    #[test]
    fn package_manager_detection() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(package_manager(d.path()), None);
        fs::write(d.path().join("package.json"), r#"{"packageManager":"yarn@4.1.0"}"#).unwrap();
        assert_eq!(package_manager(d.path()), Some("yarn"));
        fs::write(d.path().join("package.json"), "{}").unwrap();
        fs::write(d.path().join("bun.lock"), "").unwrap();
        assert_eq!(package_manager(d.path()), Some("bun"));
    }

    #[test]
    fn makefile_justfile_procfile_taskfile() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("Makefile"),
            ".PHONY: test\nVAR := 1\ntest: build ## Run the tests\n\tpytest\nbuild:\n\tcc\n%.o: %.c\n\tcc\nout/app: x\n",
        )
        .unwrap();
        fs::write(
            d.path().join("justfile"),
            "set dotenv-load\n# Start the server\nserve port='8000':\n  uvicorn\n_private:\n  x\nx := 'y'\n",
        )
        .unwrap();
        fs::write(d.path().join("Procfile"), "web: gunicorn app:app\nworker: celery -A app worker\n").unwrap();
        fs::write(
            d.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  lint:\n    cmds: [ruff .]\n  test:\n    cmds: []\n",
        )
        .unwrap();
        let s = discover(d.path());
        let got: Vec<(&str, &str)> = s.iter().map(|x| (x.source.as_str(), x.command.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("Makefile", "make test"),
                ("Makefile", "make build"),
                ("justfile", "just serve"),
                ("Taskfile.yml", "task lint"),
                ("Taskfile.yml", "task test"),
                ("Procfile", "gunicorn app:app"),
                ("Procfile", "celery -A app worker"),
            ]
        );
        assert_eq!(s[0].detail.as_deref(), Some("Run the tests"));
        assert_eq!(s[2].detail.as_deref(), Some("Start the server"));
        // `test` from the Taskfile collides with the Makefile's.
        assert_eq!(s[4].suggested_name, "test-taskfile");
    }

    #[test]
    fn pyproject_and_composer() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("pyproject.toml"),
            "[project.scripts]\nserve = 'app.cli:main'\n[tool.poe.tasks]\nlint = 'ruff check .'\n",
        )
        .unwrap();
        fs::write(d.path().join("uv.lock"), "").unwrap();
        fs::write(
            d.path().join("composer.json"),
            r#"{"scripts":{"test":["phpunit","phpstan"],"post-install-cmd":"x"}}"#,
        )
        .unwrap();
        let s = discover(d.path());
        let cmds: Vec<&str> = s.iter().map(|x| x.command.as_str()).collect();
        assert_eq!(cmds, vec!["composer run-script test", "uv run serve", "uv run poe lint"]);
        assert_eq!(s[0].detail.as_deref(), Some("phpunit && phpstan"));
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("build:prod"), "build-prod");
        assert_eq!(sanitize_name("run"), "run-run");
        assert_eq!(sanitize_name("::"), "run");
        assert_eq!(sanitize_name("db/migrate up"), "db-migrate-up");
    }
}
