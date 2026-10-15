//! Bookmark data model as stored in `bookmarks.toml` / `.bk.toml`.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One saved command (or workflow) plus the context it runs in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Working directory as written by the user (`~` and relative paths allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "EnvSpec::is_inherit", deserialize_with = "de_env_spec")]
    pub env: EnvSpec,
    /// Language runtimes layered on top of `env`, e.g. `{ node = "20", java = "21" }`.
    /// A version prefix, an exact version, or `auto` (follow `.nvmrc`, `.java-version`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtimes: BTreeMap<String, String>,
    /// Extra PATH entries (relative to `cwd`), searched before everything else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// Single command. Mutually exclusive with `steps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Multi-step workflow, run in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// What runs differently on one OS, e.g. `windows = { command = "Remove-Item -Recurse build" }`.
    /// Commands are written in a shell's language, and shells differ by OS.
    #[serde(default, skip_serializing_if = "no_variant")]
    pub macos: Option<OsVariant>,
    #[serde(default, skip_serializing_if = "no_variant")]
    pub linux: Option<OsVariant>,
    #[serde(default, skip_serializing_if = "no_variant")]
    pub windows: Option<OsVariant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// `.env` file, relative to `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dotenv: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
    /// Shell used to run each step (`zsh`, `bash`, `sh`, or a path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Ask before running.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm: bool,
    /// Desktop notification when the run finishes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub notify: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    /// Run on this server (a Clibooker server, an ~/.ssh/config alias, or `user@host[:port]`).
    /// `cwd`, `env`, `runtimes`, `dotenv` and `path` then refer to the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Local port forwards while it runs, e.g. `8888:127.0.0.1:8888` (remote bookmarks only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward: Vec<String>,
    /// Run inside tmux on the server so it survives disconnects (remote bookmarks only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub detach: bool,
}

impl Bookmark {
    pub fn is_remote(&self) -> bool {
        self.server.as_deref().is_some_and(|s| !s.trim().is_empty())
    }

    /// The steps that run on this OS, treating `command` as a one-step workflow.
    pub fn effective_steps(&self) -> Vec<Step> {
        self.steps_on(Os::current())
    }

    /// The shell named for this OS's steps (`None`: the default shell).
    pub fn effective_shell(&self) -> Option<&str> {
        self.shell_on(Os::current())
    }

    /// The steps that run on `os`: its variant's, if it has any, else the base ones.
    pub fn steps_on(&self, os: Os) -> Vec<Step> {
        match self.variant(os) {
            Some(v) if v.replaces_steps() => as_steps(&v.command, &v.steps),
            _ => self.base_steps(),
        }
    }

    /// The shell named for `os`. A variant with its own command doesn't inherit the base
    /// shell, which was picked for the base command.
    pub fn shell_on(&self, os: Os) -> Option<&str> {
        match self.variant(os) {
            Some(v) if v.shell.is_some() => v.shell.as_deref(),
            Some(v) if v.replaces_steps() => None,
            _ => self.shell.as_deref(),
        }
    }

    /// The command or steps that run on every OS without a variant of its own.
    pub fn base_steps(&self) -> Vec<Step> {
        as_steps(&self.command, &self.steps)
    }

    /// `os`'s variant, if it says anything. Server bookmarks run on the server's OS, so
    /// they never use one.
    pub fn variant(&self, os: Os) -> Option<&OsVariant> {
        if self.is_remote() {
            return None;
        }
        self.variant_slot(os).as_ref().filter(|v| !v.is_empty())
    }

    pub fn variant_slot(&self, os: Os) -> &Option<OsVariant> {
        match os {
            Os::Macos => &self.macos,
            Os::Linux => &self.linux,
            Os::Windows => &self.windows,
        }
    }

    pub fn variant_slot_mut(&mut self, os: Os) -> &mut Option<OsVariant> {
        match os {
            Os::Macos => &mut self.macos,
            Os::Linux => &mut self.linux,
            Os::Windows => &mut self.windows,
        }
    }

    /// Checks what's written, the same on every OS (a bookmark synced from a Mac must be
    /// storable on Windows too).
    pub fn validate(&self) -> Result<(), String> {
        if self.command.is_some() && !self.steps.is_empty() {
            return Err("use either `command` or `steps`, not both".into());
        }
        let steps = self.base_steps();
        if steps.is_empty() {
            return Err("bookmark has no `command` or `steps`".into());
        }
        if steps.iter().any(|s| s.run.trim().is_empty()) {
            return Err("a step has an empty command".into());
        }
        for os in Os::ALL {
            let Some(v) = self.variant_slot(os).as_ref().filter(|v| !v.is_empty()) else { continue };
            if self.is_remote() {
                return Err(format!(
                    "`{}` doesn't apply to bookmarks that run on a server (they use the server's shell)",
                    os.key()
                ));
            }
            if v.command.is_some() && !v.steps.is_empty() {
                return Err(format!("{}: use either `command` or `steps`, not both", os.key()));
            }
            if as_steps(&v.command, &v.steps).iter().any(|s| s.run.trim().is_empty()) {
                return Err(format!("{}: a step has an empty command", os.key()));
            }
        }
        if !self.is_remote() && (!self.forward.is_empty() || self.detach) {
            return Err("`forward` and `detach` only apply to bookmarks that run on a server".into());
        }
        if let Some(s) = self.server.as_deref().filter(|s| !s.trim().is_empty()) {
            crate::servers::validate_destination(s)?;
        }
        for f in &self.forward {
            crate::servers::validate_forward(f)?;
        }
        for (lang, version) in &self.runtimes {
            if !crate::env::runtimes::SUPPORTED.contains(&lang.as_str()) {
                return Err(format!(
                    "unknown runtime `{lang}` (supported: {})",
                    crate::env::runtimes::SUPPORTED.join(", ")
                ));
            }
            if version.trim().is_empty() {
                return Err(format!("runtime `{lang}` needs a version (or \"auto\")"));
            }
        }
        Ok(())
    }

    /// Env plus runtimes, e.g. `conda:sam2 · node 20` (or just `node auto`).
    pub fn env_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = self.server.as_deref().filter(|s| !s.is_empty()) {
            parts.push(format!("@{s}"));
        }
        if !self.env.is_inherit() || (self.runtimes.is_empty() && !self.is_remote()) {
            parts.push(self.env.label());
        }
        parts.extend(self.runtimes.iter().map(|(l, v)| format!("{l} {v}")));
        parts.join(" · ")
    }

    /// Short one-line summary of what runs (for lists / pickers).
    pub fn summary(&self) -> String {
        let steps = self.effective_steps();
        match steps.len() {
            0 => String::new(),
            1 => steps[0].run.lines().next().unwrap_or_default().to_string(),
            n => format!("{} (+{} more)", steps[0].run.lines().next().unwrap_or_default(), n - 1),
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn as_steps(command: &Option<String>, steps: &[Step]) -> Vec<Step> {
    match command {
        Some(cmd) if steps.is_empty() => vec![Step::new(cmd.clone())],
        _ => steps.to_vec(),
    }
}

fn no_variant(v: &Option<OsVariant>) -> bool {
    v.as_ref().is_none_or(OsVariant::is_empty)
}

/// An operating system a bookmark can say something different for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Macos,
    Linux,
    Windows,
}

impl Os {
    pub const ALL: [Os; 3] = [Os::Macos, Os::Linux, Os::Windows];

    /// The OS this runs on (other Unixes count as Linux).
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    /// The bookmark key, which is also `std::env::consts::OS`.
    pub fn key(self) -> &'static str {
        match self {
            Os::Macos => "macos",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Os::Macos => "macOS",
            Os::Linux => "Linux",
            Os::Windows => "Windows",
        }
    }

    pub fn parse(s: &str) -> Option<Os> {
        Os::ALL.into_iter().find(|o| o.key() == s)
    }
}

/// What runs differently on one OS: the command (or steps) and the shell they're written for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsVariant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
}

impl OsVariant {
    pub fn is_empty(&self) -> bool {
        !self.replaces_steps() && self.shell.is_none()
    }

    pub fn replaces_steps(&self) -> bool {
        self.command.is_some() || !self.steps.is_empty()
    }
}

/// One step of a workflow. Serialized as a plain string when only `run` is set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    pub run: String,
    /// Optional display name.
    pub name: Option<String>,
    /// Working directory for this step, relative to the bookmark's `cwd`.
    pub cwd: Option<String>,
    /// Keep going with the next step when this one fails.
    pub continue_on_error: bool,
}

impl Step {
    pub fn new(run: impl Into<String>) -> Self {
        Step { run: run.into(), ..Default::default() }
    }

    fn is_simple(&self) -> bool {
        self.name.is_none() && self.cwd.is_none() && !self.continue_on_error
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepTable {
    run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    continue_on_error: bool,
}

impl Serialize for Step {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.is_simple() {
            s.serialize_str(&self.run)
        } else {
            StepTable {
                run: self.run.clone(),
                name: self.name.clone(),
                cwd: self.cwd.clone(),
                continue_on_error: self.continue_on_error,
            }
            .serialize(s)
        }
    }
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StepVisitor;
        impl<'de> Visitor<'de> for StepVisitor {
            type Value = Step;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a command string or a table like { run = \"...\" }")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Step, E> {
                Ok(Step::new(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Step, A::Error> {
                let t = StepTable::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(Step { run: t.run, name: t.name, cwd: t.cwd, continue_on_error: t.continue_on_error })
            }
        }
        d.deserialize_any(StepVisitor)
    }
}

/// Which Python (or other) environment to activate before running.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EnvSpec {
    /// Use the calling environment unchanged.
    #[default]
    Inherit,
    /// Clean environment: strips any active conda env / venv.
    None,
    Conda {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    Venv {
        #[serde(default = "default_venv_path")]
        path: String,
    },
    Uv {
        /// Run `uv sync` before the steps.
        #[serde(default, skip_serializing_if = "is_false")]
        sync: bool,
    },
    Poetry,
    Pyenv {
        version: String,
    },
    /// Detect at run time from the working directory.
    Auto,
    /// Every tool pinned by mise (`.mise.toml`, `.tool-versions`) in the working directory.
    Mise,
}

fn default_venv_path() -> String {
    ".venv".into()
}

impl EnvSpec {
    pub fn is_inherit(&self) -> bool {
        matches!(self, EnvSpec::Inherit)
    }

    /// Parse the compact CLI form: `conda:NAME`, `conda-prefix:PATH`, `venv[:PATH]`,
    /// `uv`, `uv-sync`, `poetry`, `pyenv:VER`, `auto`, `none`, `inherit`.
    pub fn parse(s: &str) -> Result<EnvSpec, String> {
        let (kind, arg) = match s.split_once(':') {
            Some((k, a)) => (k, Some(a.trim()).filter(|a| !a.is_empty())),
            None => (s, None),
        };
        let need =
            |what: &str| arg.map(str::to_string).ok_or_else(|| format!("`{kind}` needs a {what}, e.g. `{kind}:...`"));
        Ok(match kind.trim().to_ascii_lowercase().as_str() {
            "conda" => EnvSpec::Conda { name: Some(need("env name")?), prefix: None },
            "conda-prefix" => EnvSpec::Conda { name: None, prefix: Some(need("path")?) },
            "venv" => EnvSpec::Venv { path: arg.unwrap_or(".venv").to_string() },
            "uv" => EnvSpec::Uv { sync: false },
            "uv-sync" => EnvSpec::Uv { sync: true },
            "poetry" => EnvSpec::Poetry,
            "pyenv" => EnvSpec::Pyenv { version: need("version")? },
            "auto" => EnvSpec::Auto,
            "mise" => EnvSpec::Mise,
            "none" => EnvSpec::None,
            "inherit" | "" => EnvSpec::Inherit,
            other => {
                return Err(format!(
                    "unknown env type `{other}` (try conda:NAME, venv:PATH, uv, poetry, pyenv:VER, mise, auto, none)"
                ));
            }
        })
    }

    /// Short human label, e.g. `conda:sam2`.
    pub fn label(&self) -> String {
        match self {
            EnvSpec::Inherit => "inherit".into(),
            EnvSpec::None => "none".into(),
            EnvSpec::Conda { name: Some(n), .. } => format!("conda:{n}"),
            EnvSpec::Conda { prefix: Some(p), .. } => format!("conda:{p}"),
            EnvSpec::Conda { .. } => "conda:?".into(),
            EnvSpec::Venv { path } => format!("venv:{path}"),
            EnvSpec::Uv { sync: true } => "uv (sync)".into(),
            EnvSpec::Uv { .. } => "uv".into(),
            EnvSpec::Poetry => "poetry".into(),
            EnvSpec::Pyenv { version } => format!("pyenv:{version}"),
            EnvSpec::Auto => "auto".into(),
            EnvSpec::Mise => "mise".into(),
        }
    }
}

/// Accepts either the table form `{ type = "conda", name = "x" }` or the compact string `"conda:x"`.
fn de_env_spec<'de, D: Deserializer<'de>>(d: D) -> Result<EnvSpec, D::Error> {
    struct EnvVisitor;
    impl<'de> Visitor<'de> for EnvVisitor {
        type Value = EnvSpec;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an env table like { type = \"conda\", name = \"x\" } or a string like \"conda:x\"")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<EnvSpec, E> {
            EnvSpec::parse(v).map_err(E::custom)
        }
        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<EnvSpec, A::Error> {
            EnvSpec::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }
    d.deserialize_any(EnvVisitor)
}

/// Global settings (`[settings]` in the global file only).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Default shell for steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Record runs in history (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<bool>,
    /// Global hotkey for the palette (desktop app).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    /// External terminal app used for "pop out": terminal | iterm | ghostty | warp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// system | light | dark
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_at_login: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_in_dock: Option<bool>,
    /// Keep the app running in the menu bar / tray when its window is closed (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_running: Option<bool>,
    /// The desktop app looks for updates by itself (default true). Kept per device: it isn't synced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_font_size: Option<u16>,
    /// ssh binary (default /usr/bin/ssh, which understands macOS `UseKeychain`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    /// `~/.ssh/config` hosts left out of Clibooker's server lists. They still work by name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_servers: Vec<String>,
}

impl Settings {
    pub fn history_enabled(&self) -> bool {
        self.history.unwrap_or(true)
    }
}

/// Whole file contents.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BookFile {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub bookmarks: BTreeMap<String, Bookmark>,
    /// SSH servers. Only honored in the global file.
    #[serde(default)]
    pub servers: BTreeMap<String, Server>,
}

/// An SSH server defined in Clibooker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// Hostname, IP, or an ~/.ssh/config alias.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Private key file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Jump host (ProxyJump): another server name or `user@host[:port]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
    /// Port forwards opened with interactive sessions, e.g. `8888:127.0.0.1:8888`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward: Vec<String>,
    /// Extra ssh options (an allowlist; see `servers::ALLOWED_OPTIONS`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Shell that runs bookmarks on the server (default: bash if present, else sh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Use a login shell (loads ~/.profile) for runs. Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_example() {
        let src = r#"
[bookmarks.train]
cwd = "~/work/sam2"
env = { type = "conda", name = "sam2" }
command = "python train.py --epochs {{epochs:10}}"
tags = ["ml"]
vars = { CUDA_VISIBLE_DEVICES = "0" }

[bookmarks.dev]
env = "uv"
steps = ["alembic upgrade head", { run = "npm run dev", cwd = "frontend", continue_on_error = true }]
"#;
        let f: BookFile = toml::from_str(src).unwrap();
        let t = &f.bookmarks["train"];
        assert_eq!(t.env, EnvSpec::Conda { name: Some("sam2".into()), prefix: None });
        assert_eq!(t.effective_steps().len(), 1);
        let d = &f.bookmarks["dev"];
        assert_eq!(d.env, EnvSpec::Uv { sync: false });
        assert_eq!(d.steps[1].cwd.as_deref(), Some("frontend"));
        assert!(d.steps[1].continue_on_error);
        assert!(!d.steps[0].continue_on_error);
    }

    #[test]
    fn bad_step_gives_useful_error() {
        let src = "[bookmarks.x]\nsteps = [{ rnu = \"oops\" }]\n";
        let err = toml::from_str::<BookFile>(src).unwrap_err().to_string();
        assert!(err.contains("rnu") || err.contains("run"), "{err}");
    }

    #[test]
    fn bad_env_gives_useful_error() {
        let err = toml::from_str::<BookFile>("[bookmarks.x]\nenv = \"condaa:x\"\n").unwrap_err().to_string();
        assert!(err.contains("unknown env type"), "{err}");
    }

    #[test]
    fn env_parse_forms() {
        assert_eq!(EnvSpec::parse("venv").unwrap(), EnvSpec::Venv { path: ".venv".into() });
        assert_eq!(EnvSpec::parse("pyenv:3.11").unwrap(), EnvSpec::Pyenv { version: "3.11".into() });
        assert!(EnvSpec::parse("conda").is_err());
    }

    #[test]
    fn validate_rejects_both() {
        let b = Bookmark { command: Some("a".into()), steps: vec![Step::new("b")], ..Default::default() };
        assert!(b.validate().is_err());
    }

    fn variant(command: Option<&str>, shell: Option<&str>) -> Option<OsVariant> {
        Some(OsVariant { command: command.map(Into::into), shell: shell.map(Into::into), ..Default::default() })
    }

    #[test]
    fn os_variants_pick_steps_and_shell() {
        let b = Bookmark {
            command: Some("rm -rf build".into()),
            shell: Some("zsh".into()),
            windows: variant(Some("Remove-Item -Recurse build"), None),
            linux: variant(None, Some("bash")),
            ..Default::default()
        };
        assert_eq!(b.steps_on(Os::Windows)[0].run, "Remove-Item -Recurse build");
        assert_eq!(b.shell_on(Os::Windows), None, "a variant's own command doesn't inherit the base shell");
        assert_eq!(b.steps_on(Os::Linux)[0].run, "rm -rf build");
        assert_eq!(b.shell_on(Os::Linux), Some("bash"), "a shell-only variant changes just the shell");
        assert_eq!(b.steps_on(Os::Macos)[0].run, "rm -rf build");
        assert_eq!(b.shell_on(Os::Macos), Some("zsh"));
        assert_eq!(b.effective_steps(), b.steps_on(Os::current()));
        assert_eq!(b.summary(), b.steps_on(Os::current())[0].run);
        assert!(b.validate().is_ok());

        // Server bookmarks run in the server's shell: variants don't apply, and aren't accepted.
        let remote = Bookmark { server: Some("gpu".into()), ..b.clone() };
        assert_eq!(remote.steps_on(Os::Windows)[0].run, "rm -rf build");
        assert!(remote.validate().unwrap_err().contains("server"));
    }

    #[test]
    fn validation_is_the_same_on_every_os() {
        let only_windows = Bookmark { windows: variant(Some("dir"), None), ..Default::default() };
        assert!(only_windows.validate().unwrap_err().contains("no `command`"), "the base command is required");
        let both = Bookmark {
            command: Some("ls".into()),
            windows: Some(OsVariant { command: Some("dir".into()), steps: vec![Step::new("x")], ..Default::default() }),
            ..Default::default()
        };
        assert!(both.validate().unwrap_err().starts_with("windows:"));
        let empty = Bookmark { command: Some("ls".into()), macos: variant(Some(" "), None), ..Default::default() };
        assert!(empty.validate().unwrap_err().starts_with("macos:"));
    }
}
