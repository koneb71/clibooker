//! Language runtimes layered on top of the Python env: node, java, ruby, go, rust, php.
//!
//! Versions are resolved against what's installed by the usual managers (nvm, fnm, volta,
//! mise, asdf, Homebrew, SDKMAN, rbenv, chruby, rvm, rustup, ...), without running them:
//! activation is just PATH entries plus a few variables (JAVA_HOME, GOTOOLCHAIN, ...).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;

use super::{EnvMap, get, prepend_path, set, which_in};
use crate::paths;

pub const SUPPORTED: &[&str] = &["node", "java", "ruby", "go", "rust", "php"];

/// One installed version of a runtime.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Install {
    pub version: String,
    /// Install directory. Kept verbatim (it may be a path on a server).
    pub home: PathBuf,
    /// Where it came from: nvm, homebrew, sdkman, ...
    pub source: String,
}

/// What activating a runtime does.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub lang: String,
    /// Short label, e.g. `node 22.14.0`.
    pub label: String,
    /// Longer description, e.g. `node 22.14.0 (nvm, from .nvmrc)`.
    pub description: String,
    /// PATH entries to put first, in priority order.
    pub bin_dirs: Vec<PathBuf>,
    pub vars: Vec<(String, String)>,
}

// ---------- versions ----------

/// Numeric components of a version string: `v20.11.1` → [20, 11, 1], `temurin-21.0.2` → [21, 0, 2].
pub fn parse_version(v: &str) -> Vec<u64> {
    let start = v.find(|c: char| c.is_ascii_digit()).unwrap_or(v.len());
    let mut out = Vec::new();
    // Components are separated by `.` or `_` (Java's `1.8.0_392`); anything else (`+13`, `-rc1`) ends it.
    for part in v[start..].split(['.', '_']) {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        match digits.parse() {
            Ok(n) => out.push(n),
            Err(_) => break,
        }
        if digits.len() != part.len() {
            break;
        }
    }
    out
}

/// Java's legacy `1.8` naming means 8.
fn java_norm(mut v: Vec<u64>) -> Vec<u64> {
    if v.len() > 1 && v[0] == 1 {
        v.remove(0);
    }
    v
}

#[derive(Debug, Clone, PartialEq)]
pub enum Want {
    /// Version prefix: `20` matches 20.x.y.
    Prefix(Vec<u64>),
    /// `>=18`: highest installed at or above.
    AtLeast(Vec<u64>),
    /// Highest installed.
    Latest,
    /// Highest installed even major (Node LTS lines).
    Lts,
}

pub fn parse_want(spec: &str, lang: &str) -> Option<Want> {
    let s = spec.trim();
    let lower = s.to_ascii_lowercase();
    if matches!(lower.as_str(), "latest" | "node" | "current" | "stable") && lang != "rust" {
        return Some(Want::Latest);
    }
    if lang == "node" && (lower == "lts" || lower.starts_with("lts/")) {
        return Some(Want::Lts);
    }
    let fix = |v: Vec<u64>| if lang == "java" { java_norm(v) } else { v };
    if let Some(rest) = s.strip_prefix(">=") {
        let v = parse_version(rest);
        return (!v.is_empty()).then(|| Want::AtLeast(fix(v)));
    }
    // `^20`, `~20.1`, `20.x`, `v20` all mean "this prefix".
    let v = parse_version(s.trim_start_matches(['^', '~', '=', 'v']));
    (!v.is_empty()).then(|| Want::Prefix(fix(v)))
}

pub fn pick<'a>(installs: &'a [Install], want: &Want, lang: &str) -> Option<&'a Install> {
    let ver = |i: &Install| {
        let v = parse_version(&i.version);
        if lang == "java" { java_norm(v) } else { v }
    };
    installs
        .iter()
        .filter(|i| match want {
            Want::Prefix(p) => ver(i).starts_with(p),
            Want::AtLeast(min) => ver(i) >= *min,
            Want::Latest => true,
            Want::Lts => ver(i).first().is_some_and(|m| m % 2 == 0),
        })
        .max_by(|a, b| ver(a).cmp(&ver(b)))
}

// ---------- discovery of installs ----------

fn home() -> PathBuf {
    paths::home()
}

fn var_dir(env: &EnvMap, key: &str) -> Option<PathBuf> {
    get(env, key).map(PathBuf::from)
}

/// `name` as an executable file name on this OS.
fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// Where an install keeps `program`: `<home>/bin` normally, the install root itself for
/// Windows layouts like nvm-windows, Volta or scoop (`<home>/node.exe`).
pub fn tool_dir(home: &Path, program: &str) -> PathBuf {
    let bin = home.join("bin");
    if !bin.join(exe(program)).exists() && home.join(exe(program)).exists() { home.to_path_buf() } else { bin }
}

fn has_tool(home: &Path, program: &str) -> bool {
    tool_dir(home, program).join(exe(program)).exists()
}

/// Windows folders that hold per-user and machine-wide installs.
struct WinDirs {
    program_files: Vec<PathBuf>,
    appdata: Option<PathBuf>,
    local_appdata: Option<PathBuf>,
    scoop_apps: Vec<PathBuf>,
}

fn win_dirs(env: &EnvMap) -> WinDirs {
    let mut program_files: Vec<PathBuf> =
        ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|k| var_dir(env, k)).collect();
    program_files.dedup();
    let scoop = var_dir(env, "SCOOP").unwrap_or_else(|| home().join("scoop"));
    let mut scoop_apps = vec![scoop.join("apps")];
    if let Some(g) = var_dir(env, "SCOOP_GLOBAL").or_else(|| var_dir(env, "ProgramData").map(|p| p.join("scoop"))) {
        scoop_apps.push(g.join("apps"));
    }
    WinDirs { program_files, appdata: var_dir(env, "APPDATA"), local_appdata: var_dir(env, "LOCALAPPDATA"), scoop_apps }
}

/// scoop installs: `<scoop>/apps/<app>/<version>` for apps whose name starts with a prefix.
fn scoop(env: &EnvMap, app_prefixes: &[&str]) -> Vec<Install> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for apps in win_dirs(env).scoop_apps {
        for (app, app_dir) in subdirs(&apps) {
            if !app_prefixes.iter().any(|p| app.starts_with(p)) {
                continue;
            }
            for (version, path) in subdirs(&app_dir) {
                if version != "current" {
                    out.push(Install { version, home: path, source: "scoop".into() });
                }
            }
        }
    }
    out
}

/// First line of a `VERSION` file (Go: `go1.22.3`).
fn version_file(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("VERSION")).ok()?;
    first_line_of(&text).map(|l| l.trim_start_matches("go").to_string())
}

/// Subdirectories of `dir` as (name, path), skipping symlinked aliases.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort();
    out
}

/// Version of a Homebrew formula from its `opt` symlink (`../Cellar/node/26.8.1`).
fn homebrew(formula_prefix: &str, home_suffix: &str) -> Vec<Install> {
    let mut out = Vec::new();
    let roots = [
        PathBuf::from("/opt/homebrew/opt"),
        PathBuf::from("/usr/local/opt"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt"),
        home().join(".linuxbrew/opt"),
    ];
    for root in roots {
        let Ok(rd) = fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_formula = name == formula_prefix || name.starts_with(&format!("{formula_prefix}@"));
            if !is_formula {
                continue;
            }
            let target = dunce::canonicalize(e.path()).unwrap_or(e.path());
            let version = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let home = target.join(home_suffix);
            if home.is_dir() && !parse_version(&version).is_empty() {
                out.push(Install { version, home, source: "homebrew".into() });
            }
        }
    }
    out
}

/// mise and asdf install trees: `<root>/installs/<tool>/<version>`.
fn mise_asdf(env: &EnvMap, tools: &[&str]) -> Vec<Install> {
    let mise = var_dir(env, "MISE_DATA_DIR").unwrap_or_else(|| {
        if cfg!(windows) {
            var_dir(env, "LOCALAPPDATA").unwrap_or_else(|| home().join("AppData/Local")).join("mise")
        } else {
            var_dir(env, "XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share")).join("mise")
        }
    });
    let asdf = var_dir(env, "ASDF_DATA_DIR").unwrap_or_else(|| home().join(".asdf"));
    let mut out = Vec::new();
    for (root, source) in [(mise, "mise"), (asdf, "asdf")] {
        for tool in tools {
            for (name, path) in subdirs(&root.join("installs").join(tool)) {
                if fs::symlink_metadata(&path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                    continue;
                }
                out.push(Install { version: name, home: path, source: source.into() });
            }
        }
    }
    out
}

fn node_installs(env: &EnvMap) -> Vec<Install> {
    let mut out = Vec::new();
    let nvm = var_dir(env, "NVM_DIR").unwrap_or_else(|| home().join(".nvm"));
    for (name, path) in subdirs(&nvm.join("versions/node")) {
        out.push(Install { version: name.trim_start_matches('v').into(), home: path, source: "nvm".into() });
    }
    let w = win_dirs(env);
    if cfg!(windows) {
        // nvm-windows: <NVM_HOME>/v20.11.1/node.exe
        let nvm_home = var_dir(env, "NVM_HOME").or_else(|| w.appdata.as_ref().map(|a| a.join("nvm")));
        for (name, path) in nvm_home.map(|h| subdirs(&h)).unwrap_or_default() {
            if name.starts_with('v') && parse_version(&name).len() >= 2 {
                out.push(Install {
                    version: name.trim_start_matches('v').into(),
                    home: path,
                    source: "nvm-windows".into(),
                });
            }
        }
    }
    let fnm_roots = [
        var_dir(env, "FNM_DIR"),
        Some(home().join(".local/share/fnm")),
        Some(home().join("Library/Application Support/fnm")),
        Some(home().join(".fnm")),
        w.appdata.as_ref().map(|a| a.join("fnm")),
        w.local_appdata.as_ref().map(|a| a.join("fnm")),
    ];
    for root in fnm_roots.into_iter().flatten() {
        for (name, path) in subdirs(&root.join("node-versions")) {
            out.push(Install {
                version: name.trim_start_matches('v').into(),
                home: path.join("installation"),
                source: "fnm".into(),
            });
        }
    }
    let volta = var_dir(env, "VOLTA_HOME").unwrap_or_else(|| match (&w.local_appdata, cfg!(windows)) {
        (Some(l), true) => l.join("Volta"),
        _ => home().join(".volta"),
    });
    for (name, path) in subdirs(&volta.join("tools/image/node")) {
        out.push(Install { version: name, home: path, source: "volta".into() });
    }
    out.extend(mise_asdf(env, &["node", "nodejs"]));
    out.extend(homebrew("node", ""));
    out.extend(scoop(env, &["nodejs"]));
    dedupe(out.into_iter().filter(|i| has_tool(&i.home, "node")).collect())
}

fn java_release_version(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("release")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("JAVA_VERSION=").map(|v| v.trim_matches('"').to_string()))
}

fn java_installs(env: &EnvMap) -> Vec<Install> {
    let mut homes: Vec<(PathBuf, String)> = Vec::new();
    for root in [PathBuf::from("/Library/Java/JavaVirtualMachines"), home().join("Library/Java/JavaVirtualMachines")] {
        for (_, p) in subdirs(&root) {
            homes.push((p.join("Contents/Home"), "system".into()));
        }
    }
    // Linux distro JDKs (/usr/lib/jvm/java-21-openjdk-amd64) and Windows vendor installers.
    let mut roots = vec![PathBuf::from("/usr/lib/jvm"), PathBuf::from("/usr/java")];
    if cfg!(windows) {
        for pf in win_dirs(env).program_files {
            for vendor in ["Java", "Eclipse Adoptium", "Microsoft", "Zulu", "Amazon Corretto", "BellSoft", "Semeru"] {
                roots.push(pf.join(vendor));
            }
        }
    }
    for root in roots {
        for (_, p) in subdirs(&root) {
            if !fs::symlink_metadata(&p).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                homes.push((p, "system".into()));
            }
        }
    }
    for i in scoop(env, &["openjdk", "temurin", "zulu", "corretto", "microsoft"]) {
        homes.push((i.home, i.source));
    }
    let sdkman = var_dir(env, "SDKMAN_DIR").unwrap_or_else(|| home().join(".sdkman"));
    for (name, p) in subdirs(&sdkman.join("candidates/java")) {
        if name != "current" {
            homes.push((p, "sdkman".into()));
        }
    }
    for i in homebrew("openjdk", "libexec/openjdk.jdk/Contents/Home") {
        homes.push((i.home, "homebrew".into()));
    }
    for i in mise_asdf(env, &["java"]) {
        let h = if i.home.join("Contents/Home").is_dir() { i.home.join("Contents/Home") } else { i.home };
        homes.push((h, i.source));
    }
    let out = homes
        .into_iter()
        .filter(|(h, _)| h.join("bin").join(exe("java")).exists())
        .filter_map(|(home, source)| {
            let version = java_release_version(&home)
                .or_else(|| home.parent()?.parent()?.file_name().map(|n| n.to_string_lossy().into_owned()))?;
            Some(Install { version, home, source })
        })
        .collect();
    dedupe(out)
}

fn ruby_installs(env: &EnvMap) -> Vec<Install> {
    let mut out = Vec::new();
    let rbenv = var_dir(env, "RBENV_ROOT").unwrap_or_else(|| home().join(".rbenv"));
    for (name, path) in subdirs(&rbenv.join("versions")) {
        out.push(Install { version: name, home: path, source: "rbenv".into() });
    }
    for (root, source) in [
        (home().join(".rubies"), "chruby"),
        (PathBuf::from("/opt/rubies"), "chruby"),
        (home().join(".rvm/rubies"), "rvm"),
    ] {
        for (name, path) in subdirs(&root) {
            out.push(Install { version: name.trim_start_matches("ruby-").into(), home: path, source: source.into() });
        }
    }
    out.extend(mise_asdf(env, &["ruby"]));
    out.extend(homebrew("ruby", ""));
    out.extend(scoop(env, &["ruby"]));
    if cfg!(windows) {
        // RubyInstaller: C:\Ruby33-x64
        for (name, path) in subdirs(Path::new("C:\\")) {
            if let Some(v) = name.strip_prefix("Ruby").and_then(|r| r.split('-').next())
                && v.len() >= 2
                && v.chars().all(|c| c.is_ascii_digit())
            {
                out.push(Install {
                    version: format!("{}.{}", &v[..1], &v[1..]),
                    home: path,
                    source: "rubyinstaller".into(),
                });
            }
        }
    }
    dedupe(out.into_iter().filter(|i| has_tool(&i.home, "ruby")).collect())
}

fn php_installs(env: &EnvMap) -> Vec<Install> {
    let mut out = homebrew("php", "");
    out.extend(mise_asdf(env, &["php"]));
    out.extend(scoop(env, &["php"]));
    if cfg!(windows) {
        // C:\php-8.3.4 or C:\tools\php83 (Chocolatey)
        for root in [Path::new("C:\\"), Path::new("C:\\tools")] {
            for (name, path) in subdirs(root) {
                let lower = name.to_ascii_lowercase();
                if let Some(rest) = lower.strip_prefix("php") {
                    let v = rest.trim_start_matches(['-', '_']);
                    let version =
                        if v.contains('.') || v.len() < 2 { v.to_string() } else { format!("{}.{}", &v[..1], &v[1..]) };
                    if !parse_version(&version).is_empty() {
                        out.push(Install { version, home: path, source: "php".into() });
                    }
                }
            }
        }
    }
    dedupe(out.into_iter().filter(|i| has_tool(&i.home, "php")).collect())
}

fn go_installs(env: &EnvMap) -> Vec<Install> {
    let mut out = Vec::new();
    // golang.org/dl installs: ~/sdk/go1.22.3
    for (name, path) in subdirs(&home().join("sdk")) {
        if name.starts_with("go") {
            out.push(Install { version: name.trim_start_matches("go").into(), home: path, source: "go sdk".into() });
        }
    }
    let goenv = var_dir(env, "GOENV_ROOT").unwrap_or_else(|| home().join(".goenv"));
    for (name, path) in subdirs(&goenv.join("versions")) {
        out.push(Install { version: name, home: path, source: "goenv".into() });
    }
    out.extend(mise_asdf(env, &["go", "golang"]));
    out.extend(homebrew("go", "libexec"));
    out.extend(scoop(env, &["go"]));
    // System installs: /usr/local/go, Debian's /usr/lib/go-1.22, C:\Program Files\Go.
    let mut system = vec![PathBuf::from("/usr/local/go")];
    system.extend(subdirs(Path::new("/usr/lib")).into_iter().filter(|(n, _)| n.starts_with("go-")).map(|(_, p)| p));
    if cfg!(windows) {
        system.extend(win_dirs(env).program_files.into_iter().map(|p| p.join("Go")));
    }
    for home in system {
        if let Some(version) = version_file(&home) {
            out.push(Install { version, home, source: "system".into() });
        }
    }
    dedupe(out.into_iter().filter(|i| has_tool(&i.home, "go")).collect())
}

fn rust_toolchains(env: &EnvMap) -> Vec<Install> {
    let rustup = var_dir(env, "RUSTUP_HOME").unwrap_or_else(|| home().join(".rustup"));
    subdirs(&rustup.join("toolchains"))
        .into_iter()
        .map(|(name, path)| Install { version: name, home: path, source: "rustup".into() })
        .collect()
}

fn dedupe(mut v: Vec<Install>) -> Vec<Install> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|i| seen.insert(i.version.clone()));
    v.sort_by_key(|i| std::cmp::Reverse(parse_version(&i.version)));
    v
}

/// Installed versions of a runtime, newest first.
pub fn installed(lang: &str, env: &EnvMap) -> Vec<Install> {
    match lang {
        "node" => node_installs(env),
        "java" => java_installs(env),
        "ruby" => ruby_installs(env),
        "php" => php_installs(env),
        "go" => go_installs(env),
        "rust" => rust_toolchains(env),
        _ => Vec::new(),
    }
}

// ---------- version files ----------

/// Directories to search for version files: `cwd` up to (and including) the project root.
fn search_dirs(cwd: &Path) -> Vec<&Path> {
    let home = paths::home();
    let mut out = Vec::new();
    for d in cwd.ancestors() {
        if d == home || d.parent().is_none() {
            break;
        }
        out.push(d);
        if d.join(".git").exists() {
            break;
        }
    }
    out
}

fn first_line_of(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string)
}

fn tool_versions_of(text: &str, names: &[&str]) -> Option<String> {
    text.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let tool = parts.next()?;
        names.contains(&tool).then(|| parts.next().map(str::to_string)).flatten()
    })
}

fn package_json_node_of(text: &str) -> Option<(String, &'static str)> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    if let Some(v) = json.pointer("/volta/node").and_then(|v| v.as_str()) {
        return Some((v.to_string(), "package.json (volta)"));
    }
    json.pointer("/engines/node").and_then(|v| v.as_str()).map(|v| (v.to_string(), "package.json (engines)"))
}

/// Files that can pin a runtime version (relative to a project folder).
pub const VERSION_FILES: &[&str] = &[
    ".nvmrc",
    ".node-version",
    ".tool-versions",
    "package.json",
    ".java-version",
    ".sdkmanrc",
    ".ruby-version",
    ".go-version",
    ".php-version",
];

/// The version one folder pins for `lang`, reading files through `read` (local disk or fetched
/// from a server).
pub fn hint_in(lang: &str, read: &dyn Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let line = |f: &str| read(f).and_then(|t| first_line_of(&t));
    let tv = |names: &[&str]| read(".tool-versions").and_then(|t| tool_versions_of(&t, names));
    let found: Option<(String, &str)> = match lang {
        "node" => line(".nvmrc")
            .map(|v| (v, ".nvmrc"))
            .or_else(|| line(".node-version").map(|v| (v, ".node-version")))
            .or_else(|| tv(&["nodejs", "node"]).map(|v| (v, ".tool-versions")))
            .or_else(|| read("package.json").and_then(|t| package_json_node_of(&t))),
        "java" => line(".java-version")
            .map(|v| (v, ".java-version"))
            .or_else(|| {
                read(".sdkmanrc")
                    .and_then(|t| t.lines().find_map(|l| l.trim().strip_prefix("java=").map(str::to_string)))
                    .map(|v| (v, ".sdkmanrc"))
            })
            .or_else(|| tv(&["java"]).map(|v| (v, ".tool-versions"))),
        "ruby" => line(".ruby-version")
            .map(|v| (v.trim_start_matches("ruby-").to_string(), ".ruby-version"))
            .or_else(|| tv(&["ruby"]).map(|v| (v, ".tool-versions"))),
        "go" => line(".go-version")
            .map(|v| (v, ".go-version"))
            .or_else(|| tv(&["golang", "go"]).map(|v| (v, ".tool-versions"))),
        "php" => {
            line(".php-version").map(|v| (v, ".php-version")).or_else(|| tv(&["php"]).map(|v| (v, ".tool-versions")))
        }
        _ => None,
    };
    found.map(|(v, f)| (v, f.to_string()))
}

/// The version a project pins for `lang`, and the file it came from.
pub fn version_hint(lang: &str, cwd: &Path) -> Option<(String, String)> {
    search_dirs(cwd).into_iter().find_map(|dir| hint_in(lang, &|f| fs::read_to_string(dir.join(f)).ok()))
}

/// Nearest ancestor of `cwd` (within the project) containing `marker`.
fn project_dir_with(cwd: &Path, marker: &str) -> Option<PathBuf> {
    search_dirs(cwd).into_iter().find(|d| d.join(marker).is_file()).map(Path::to_path_buf)
}

// ---------- resolution ----------

fn not_installed(lang: &str, spec: &str, installs: &[Install], hint: &str) -> anyhow::Error {
    let have: Vec<&str> = installs.iter().map(|i| i.version.as_str()).take(8).collect();
    let have = if have.is_empty() { "none found".to_string() } else { format!("installed: {}", have.join(", ")) };
    anyhow::anyhow!("{lang} {spec} is not installed ({have}){hint}")
}

/// Resolve one runtime. `Ok(None)` means "nothing to do" (`system`, or `auto` without a pin).
pub fn resolve(lang: &str, spec: &str, cwd: &Path, env: &EnvMap) -> Result<Option<Resolved>> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("system") {
        return Ok(None);
    }
    let (wanted, from) = if spec.eq_ignore_ascii_case("auto") {
        match version_hint(lang, cwd) {
            Some((v, file)) => (v, Some(file)),
            None => return Ok(None),
        }
    } else {
        (spec.to_string(), None)
    };
    let from_note = from.as_deref().map(|f| format!(", from {f}")).unwrap_or_default();

    match lang {
        "rust" => {
            let toolchains = rust_toolchains(env);
            if !toolchains.iter().any(|t| t.version == wanted || t.version.starts_with(&format!("{wanted}-"))) {
                bail!(not_installed(lang, &wanted, &toolchains, &format!("; run `rustup toolchain install {wanted}`")));
            }
            Ok(Some(Resolved {
                lang: lang.into(),
                label: format!("rust {wanted}"),
                description: format!("rust toolchain {wanted} (rustup{from_note})"),
                bin_dirs: vec![],
                vars: vec![("RUSTUP_TOOLCHAIN".into(), wanted)],
            }))
        }
        "go" => {
            let installs = go_installs(env);
            let want = parse_want(&wanted, lang);
            if let Some(i) = want.as_ref().and_then(|w| pick(&installs, w, lang)) {
                return Ok(Some(Resolved {
                    lang: lang.into(),
                    label: format!("go {}", i.version),
                    description: format!("go {} ({}{from_note})", i.version, i.source),
                    bin_dirs: vec![tool_dir(&i.home, "go")],
                    vars: vec![("GOROOT".into(), i.home.display().to_string()), ("GOTOOLCHAIN".into(), "local".into())],
                }));
            }
            // Go ≥ 1.21 downloads the exact toolchain on demand.
            if parse_version(&wanted).len() < 3 {
                bail!(not_installed(
                    lang,
                    &wanted,
                    &installs,
                    "; use a full version like 1.22.3 to let go download it"
                ));
            }
            let tc = format!("go{}", wanted.trim_start_matches("go"));
            Ok(Some(Resolved {
                lang: lang.into(),
                label: format!("go {}", tc.trim_start_matches("go")),
                description: format!("go toolchain {tc} (downloaded by go on first use{from_note})"),
                bin_dirs: vec![],
                vars: vec![("GOTOOLCHAIN".into(), tc)],
            }))
        }
        "node" | "java" | "ruby" | "php" => {
            let installs = installed(lang, env);
            let want =
                parse_want(&wanted, lang).ok_or_else(|| anyhow::anyhow!("can't read {lang} version `{wanted}`"))?;
            let hint = match (lang, std::env::consts::OS) {
                ("node", _) => format!("; install it with `nvm install {wanted}` or `fnm install {wanted}`"),
                ("java", "macos") => "; install a JDK (e.g. `brew install openjdk@NN` or SDKMAN)".into(),
                ("java", "windows") => "; install a JDK (e.g. `winget install EclipseAdoptium.Temurin.21.JDK`)".into(),
                ("java", _) => "; install a JDK (e.g. `sudo apt install openjdk-21-jdk` or SDKMAN)".into(),
                ("ruby", "windows") => "; install it with RubyInstaller or `scoop install ruby`".into(),
                ("ruby", _) => format!("; install it with `rbenv install {wanted}`"),
                (_, "macos") => format!("; install it with `brew install php@{wanted}`"),
                (_, "windows") => "; install it with `scoop install php`".into(),
                _ => "; install it with your package manager".into(),
            };
            let Some(i) = pick(&installs, &want, lang) else { bail!(not_installed(lang, &wanted, &installs, &hint)) };
            let mut bin_dirs = Vec::new();
            let mut vars = Vec::new();
            match lang {
                "node" => {
                    if let Some(p) = project_dir_with(cwd, "package.json") {
                        bin_dirs.push(p.join("node_modules").join(".bin"));
                    }
                    bin_dirs.push(tool_dir(&i.home, "node"));
                }
                "java" => {
                    bin_dirs.push(i.home.join("bin"));
                    vars.push(("JAVA_HOME".into(), i.home.display().to_string()));
                }
                "php" => {
                    if let Some(p) = project_dir_with(cwd, "composer.json") {
                        bin_dirs.push(p.join("vendor").join("bin"));
                    }
                    bin_dirs.push(tool_dir(&i.home, "php"));
                    if !cfg!(windows) {
                        bin_dirs.push(i.home.join("sbin"));
                    }
                }
                _ => bin_dirs.push(tool_dir(&i.home, lang)),
            }
            Ok(Some(Resolved {
                lang: lang.into(),
                label: format!("{lang} {}", i.version),
                description: format!("{lang} {} ({}{from_note})", i.version, i.source),
                bin_dirs,
                vars,
            }))
        }
        other => bail!("unknown runtime `{other}` (supported: {})", SUPPORTED.join(", ")),
    }
}

/// Resolve every runtime first (so lookups see the unmodified env), then apply them.
pub fn apply(env: &mut EnvMap, runtimes: &BTreeMap<String, String>, cwd: &Path) -> Result<Vec<Resolved>> {
    let mut resolved = Vec::new();
    for (lang, spec) in runtimes {
        if let Some(r) = resolve(lang, spec, cwd, env)? {
            resolved.push(r);
        }
    }
    for r in &resolved {
        for dir in r.bin_dirs.iter().rev() {
            prepend_path(env, dir);
        }
        for (k, v) in &r.vars {
            set(env, k, v.as_str());
        }
    }
    Ok(resolved)
}

/// Is a tool for this runtime on PATH at all (for the editor's hints)?
pub fn available(lang: &str, env: &EnvMap) -> bool {
    let bin = match lang {
        "rust" => "rustup",
        other => other,
    };
    which_in(env, bin).is_some() || !installed(lang, env).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn inst(v: &str) -> Install {
        Install { version: v.into(), home: PathBuf::from(format!("/x/{v}")), source: "t".into() }
    }

    #[test]
    fn version_parsing_and_matching() {
        assert_eq!(parse_version("v20.11.1"), vec![20, 11, 1]);
        assert_eq!(parse_version("temurin-21.0.2+13"), vec![21, 0, 2]);
        let installs = vec![inst("18.17.1"), inst("22.14.0"), inst("23.8.0"), inst("23.10.0")];
        let p = |spec| pick(&installs, &parse_want(spec, "node").unwrap(), "node").map(|i| i.version.clone());
        assert_eq!(p("23").as_deref(), Some("23.10.0"));
        assert_eq!(p("v22").as_deref(), Some("22.14.0"));
        assert_eq!(p("^18.17").as_deref(), Some("18.17.1"));
        assert_eq!(p(">=20").as_deref(), Some("23.10.0"));
        assert_eq!(p("lts/*").as_deref(), Some("22.14.0"));
        assert_eq!(p("latest").as_deref(), Some("23.10.0"));
        assert_eq!(p("20"), None);
        let jdks = vec![inst("1.8.0_392"), inst("21.0.2"), inst("23.0.1")];
        let j = |spec| pick(&jdks, &parse_want(spec, "java").unwrap(), "java").map(|i| i.version.clone());
        assert_eq!(j("8").as_deref(), Some("1.8.0_392"));
        assert_eq!(j("1.8").as_deref(), Some("1.8.0_392"));
        assert_eq!(j("21").as_deref(), Some("21.0.2"));
    }

    #[test]
    fn version_files() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proj");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("web/src")).unwrap();
        fs::write(root.join(".nvmrc"), "v20.11\n").unwrap();
        fs::write(root.join(".tool-versions"), "ruby 3.3.0\njava temurin-21.0.2\n").unwrap();
        fs::write(root.join("web/package.json"), r#"{"engines":{"node":">=18"}}"#).unwrap();
        let src = root.join("web/src");
        // package.json engines in a nearer dir wins over the root .nvmrc
        assert_eq!(version_hint("node", &src).unwrap(), (">=18".to_string(), "package.json (engines)".to_string()));
        assert_eq!(version_hint("node", &root).unwrap().0, "v20.11");
        assert_eq!(version_hint("ruby", &src).unwrap().0, "3.3.0");
        assert_eq!(version_hint("java", &src).unwrap().0, "temurin-21.0.2");
        assert_eq!(version_hint("go", &src), None);
    }

    #[test]
    fn auto_without_pin_is_noop_and_system_is_noop() {
        let d = tempfile::tempdir().unwrap();
        let env = EnvMap::new();
        assert_eq!(resolve("node", "auto", d.path(), &env).unwrap(), None);
        assert_eq!(resolve("java", "system", d.path(), &env).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn node_from_nvm_tree_with_node_modules_bin() {
        let d = tempfile::tempdir().unwrap();
        let nvm = d.path().join("nvm");
        for v in ["v18.17.1", "v22.14.0"] {
            let bin = nvm.join("versions/node").join(v).join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("node"), "").unwrap();
        }
        let proj = d.path().join("app");
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::write(proj.join("package.json"), "{}").unwrap();
        fs::write(proj.join(".nvmrc"), "22\n").unwrap();
        let mut env: EnvMap =
            [("NVM_DIR".into(), OsString::from(nvm.as_os_str())), ("PATH".into(), OsString::from("/usr/bin"))].into();
        let rts: BTreeMap<String, String> = [("node".to_string(), "auto".to_string())].into();
        let r = apply(&mut env, &rts, &proj).unwrap();
        assert_eq!(r[0].label, "node 22.14.0");
        assert!(r[0].description.contains("from .nvmrc"), "{}", r[0].description);
        let path = super::super::get_string(&env, "PATH").unwrap();
        let expected = format!(
            "{}:{}:/usr/bin",
            proj.join("node_modules/.bin").display(),
            nvm.join("versions/node/v22.14.0/bin").display()
        );
        assert_eq!(path, expected);
        let rts: BTreeMap<String, String> = [("node".to_string(), "20".to_string())].into();
        let err = apply(&mut env, &rts, &proj).unwrap_err().to_string();
        assert!(err.contains("not installed") && err.contains("22.14.0") && err.contains("18.17.1"), "{err}");
    }

    #[test]
    fn rust_and_go_toolchains() {
        let d = tempfile::tempdir().unwrap();
        let rustup = d.path().join("rustup");
        fs::create_dir_all(rustup.join("toolchains/nightly-aarch64-apple-darwin")).unwrap();
        let env: EnvMap = [("RUSTUP_HOME".into(), OsString::from(rustup.as_os_str()))].into();
        let r = resolve("rust", "nightly", d.path(), &env).unwrap().unwrap();
        assert_eq!(r.vars, vec![("RUSTUP_TOOLCHAIN".to_string(), "nightly".to_string())]);
        assert!(resolve("rust", "beta", d.path(), &env).is_err());
        let g = resolve("go", "1.22.3", d.path(), &EnvMap::new()).unwrap().unwrap();
        assert!(g.vars.contains(&("GOTOOLCHAIN".to_string(), "go1.22.3".to_string())) || !g.bin_dirs.is_empty());
    }
}
