//! Running bookmarks on a server over SSH.
//!
//! The bookmark's activation and steps become one POSIX script (the same wrapper as local
//! app runs). It is sent base64-encoded so any login shell on the server (bash, zsh, fish,
//! csh, dash) passes it through untouched, and runs as
//! `exec bash -lc 'eval "$(printf %s B64 | base64 -d)"'` with the terminal still attached.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;

use crate::env::{EnvMap, runtimes};
use crate::model::{EnvSpec, Server};
use crate::paths::Paths;
use crate::plan::{PlanSummary, PlannedStep, RunRequest};
use crate::render::q;
use crate::script::{WrapperOptions, WrapperStep, wrapper};
use crate::servers::{self, Batch, Purpose, RemoteInventory, Target};
use crate::store::Entry;
use crate::template;

/// Shell expression for a path on the server: `~` and `~/x` expand to `$HOME`, everything
/// else is quoted. (Never goes through the local `paths::*` helpers.)
pub fn sh_path(p: &str) -> String {
    let p = p.trim();
    if p == "~" || p.is_empty() {
        "\"$HOME\"".into()
    } else if let Some(rest) = p.strip_prefix("~/") {
        if rest.is_empty() { "\"$HOME\"".into() } else { format!("\"$HOME\"/{}", q(rest)) }
    } else {
        q(p)
    }
}

/// A bookmark's working directory on the server; relative paths are relative to `$HOME`.
pub fn sh_cwd(cwd: Option<&str>) -> String {
    match cwd.map(str::trim).filter(|c| !c.is_empty()) {
        None => "\"$HOME\"".into(),
        Some(c) if c.starts_with('/') || c.starts_with('~') => sh_path(c),
        Some(c) => sh_path(&format!("~/{c}")),
    }
}

/// Double-quote but keep `${VAR}` expansion (for vars like `PATH=${PATH}:x`).
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

/// Helpers defined before the wrapper body.
const HELPERS: &str = r#"bk_conda() {
  for b in "$1" "${CONDA_EXE%/bin/conda}" "$HOME/miniconda3" "$HOME/anaconda3" "$HOME/miniforge3" "$HOME/mambaforge" /opt/conda /opt/miniconda3 /opt/anaconda3 /opt/miniforge3; do
    if [ -n "$b" ] && [ -f "$b/etc/profile.d/conda.sh" ]; then . "$b/etc/profile.d/conda.sh"; conda activate "$2"; return $?; fi
  done
  if command -v micromamba >/dev/null 2>&1; then eval "$(micromamba shell hook -s posix)"; micromamba activate "$2"; return $?; fi
  echo "bk: conda was not found on this server" >&2; return 127
}
bk_nvm() {
  [ -s "${NVM_DIR:-$HOME/.nvm}/nvm.sh" ] || { echo "bk: nvm not found on this server" >&2; return 127; }
  . "${NVM_DIR:-$HOME/.nvm}/nvm.sh" >/dev/null 2>&1; nvm use "$1" >/dev/null
}
"#;

/// Everything needed to start a remote run.
#[derive(Debug, Clone)]
pub struct RemotePlan {
    pub name: String,
    pub entry_id: String,
    pub target: Target,
    pub program: String,
    /// ssh arguments, ending with the remote command.
    pub args: Vec<String>,
    /// Environment for the local ssh process.
    pub env: EnvMap,
    /// The script that runs on the server (before encoding).
    pub script: String,
    pub cwd: String,
    pub env_label: String,
    pub env_description: String,
    pub steps: Vec<PlannedStep>,
    pub params: BTreeMap<String, String>,
    /// Port forwards for this run (the bookmark's).
    pub forwards: Vec<String>,
    pub detach: bool,
    /// For `detach`: the script that runs inside tmux (progress goes to a state file).
    pub detach_script: Option<String>,
    /// Non-fatal notes (e.g. discovery unavailable).
    pub notes: Vec<String>,
}

impl RemotePlan {
    pub fn summary(&self) -> PlanSummary {
        PlanSummary {
            name: self.name.clone(),
            cwd: format!("{}:{}", self.target.name, self.cwd),
            shell: format!("ssh {}", self.target.destination),
            env_label: self.env_label.clone(),
            env_description: self.env_description.clone(),
            env_changes: self.notes.clone(),
            steps: self.steps.clone(),
        }
    }

    /// Arguments with `-L` forwards added (the CLI keeps tunnels on the run's connection).
    pub fn args_with_forwards(&self) -> Vec<String> {
        let mut a = Vec::new();
        for f in &self.forwards {
            a.push("-L".to_string());
            a.push(f.clone());
        }
        a.extend(self.args.iter().cloned());
        a
    }
}

pub struct RemoteInputs<'a> {
    pub servers: &'a BTreeMap<String, Server>,
    pub paths: &'a Paths,
    pub ssh_program: String,
    /// Local env (only SSH_AUTH_SOCK & co. reach ssh).
    pub base_env: &'a EnvMap,
    /// Contact the server for discovery / version files / `.env` (false for previews).
    pub online: bool,
    /// Emit progress markers (app) or not (CLI).
    pub markers: bool,
}

impl RemoteInputs<'_> {
    pub fn batch<'b>(&'b self, target: &'b Target) -> Batch<'b> {
        Batch { program: self.ssh_program.clone(), target, paths: self.paths, env: servers::ssh_env(self.base_env) }
    }
}

/// Encode a script so it survives any remote login shell, keeping stdin a terminal.
pub fn remote_command(script: &str, shell: &str, login: bool) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(script);
    let inner = format!("eval \"$(printf %s {b64} | base64 -d 2>/dev/null || printf %s {b64} | base64 -D)\"");
    format!("exec {shell} {}c {}", if login { "-l" } else { "-" }, q(&inner))
}

fn remote_shell(target: &Target, inv: Option<&RemoteInventory>) -> String {
    target.shell.clone().unwrap_or_else(|| if inv.is_none_or(|i| i.has("bash")) { "bash".into() } else { "sh".into() })
}

/// Activation for the Python env, run on the server. Returns (commands, description).
fn activation(
    spec: &EnvSpec,
    inv: Option<&RemoteInventory>,
    server: &str,
) -> Result<(Vec<String>, Vec<String>, String)> {
    let mut pre_steps = Vec::new();
    let cmds = match spec {
        EnvSpec::Inherit | EnvSpec::None => return Ok((vec![], vec![], "server login environment".into())),
        EnvSpec::Conda { name, prefix } => {
            let target = prefix.clone().or_else(|| name.clone()).context("conda env needs a name or prefix")?;
            if let (Some(i), Some(n)) = (inv, name)
                && !i.conda_envs.is_empty()
                && !i.conda_envs.iter().any(|e| &e.name == n)
            {
                let have: Vec<&str> = i.conda_envs.iter().map(|e| e.name.as_str()).collect();
                bail!("conda env `{n}` not found on {server} (has: {})", have.join(", "));
            }
            let base = inv.and_then(|i| i.conda_base.clone()).unwrap_or_default();
            return Ok((
                vec![format!("bk_conda {} {}", q(&base), q(&target))],
                vec![],
                format!("conda env {target} on {server}"),
            ));
        }
        EnvSpec::Venv { path } => {
            let p = if path.starts_with('~') { sh_path(path) } else { q(path) };
            return Ok((vec![format!(". {p}/bin/activate")], vec![], format!("venv {path} on {server}")));
        }
        EnvSpec::Uv { sync } => {
            if *sync {
                pre_steps.push("uv sync".to_string());
            }
            vec![r#". "${UV_PROJECT_ENVIRONMENT:-.venv}/bin/activate""#.to_string()]
        }
        EnvSpec::Poetry => vec![r#". "$(poetry env info --path)/bin/activate""#.to_string()],
        EnvSpec::Pyenv { version } => vec![format!(
            r#"export PYENV_VERSION={} PATH="${{PYENV_ROOT:-$HOME/.pyenv}}/shims:$PATH""#,
            q(version)
        )],
        EnvSpec::Mise => vec![r#"eval "$(mise env -s bash)""#.to_string()],
        EnvSpec::Auto => vec![
            "{ if [ -f .venv/bin/activate ]; then . .venv/bin/activate; elif [ -f venv/bin/activate ]; then . venv/bin/activate; fi; }"
                .to_string(),
        ],
    };
    Ok((cmds, pre_steps, format!("{} on {server}", spec.label())))
}

/// A runtime on the server, resolved from the discovery cache. Returns (commands, label).
fn runtime(
    lang: &str,
    spec: &str,
    inv: Option<&RemoteInventory>,
    files: &BTreeMap<String, String>,
) -> Result<Option<(Vec<String>, String)>> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("system") {
        return Ok(None);
    }
    let wanted = if spec.eq_ignore_ascii_case("auto") {
        match runtimes::hint_in(lang, &|f| files.get(f).cloned()) {
            Some((v, _)) => v,
            None => return Ok(None),
        }
    } else {
        spec.to_string()
    };
    match lang {
        "rust" => return Ok(Some((vec![format!("export RUSTUP_TOOLCHAIN={}", q(&wanted))], format!("rust {wanted}")))),
        "go" if inv.is_none_or(|i| !i.runtimes.contains_key("go")) && runtimes::parse_version(&wanted).len() >= 3 => {
            let tc = format!("go{}", wanted.trim_start_matches("go"));
            return Ok(Some((vec![format!("export GOTOOLCHAIN={}", q(&tc))], format!("go {wanted}"))));
        }
        _ => {}
    }
    let Some(inv) = inv else {
        if lang == "node" {
            return Ok(Some((vec![format!("bk_nvm {}", q(&wanted))], format!("node {wanted} (nvm)"))));
        }
        bail!("{lang} {wanted}: the server hasn't been scanned yet (run `bk servers --check` or open it in the app)");
    };
    let installs = inv.runtimes.get(lang).cloned().unwrap_or_default();
    let want = runtimes::parse_want(&wanted, lang).with_context(|| format!("can't read {lang} version `{wanted}`"))?;
    let Some(i) = runtimes::pick(&installs, &want, lang) else {
        let have: Vec<&str> = installs.iter().map(|i| i.version.as_str()).take(8).collect();
        bail!(
            "{lang} {wanted} is not installed on the server ({})",
            if have.is_empty() { "none found".to_string() } else { format!("installed: {}", have.join(", ")) }
        );
    };
    let home = i.home.display().to_string();
    let bin = q(&format!("{home}/bin"));
    let cmds = match lang {
        "node" => vec![format!("export PATH=\"$PWD/node_modules/.bin\":{bin}:\"$PATH\"")],
        "java" => vec![format!("export JAVA_HOME={} PATH={bin}:\"$PATH\"", q(&home))],
        "go" => vec![format!("export GOROOT={} GOTOOLCHAIN=local PATH={bin}:\"$PATH\"", q(&home))],
        "php" => vec![format!("export PATH=\"$PWD/vendor/bin\":{bin}:\"$PATH\"")],
        _ => vec![format!("export PATH={bin}:\"$PATH\"")],
    };
    Ok(Some((cmds, format!("{lang} {} ({})", i.version, i.source))))
}

/// Build the plan for running `entry` on its server.
pub fn build(entry: &Entry, req: &RunRequest, inp: &RemoteInputs) -> Result<RemotePlan> {
    let b = &entry.bookmark;
    b.validate().map_err(anyhow::Error::msg)?;
    let server = b.server.as_deref().context("bookmark has no server")?;
    let target = servers::resolve_target(server, inp.servers)?;
    let batch = inp.batch(&target);
    let mut notes = Vec::new();

    let inv = if inp.online {
        match servers::inventory(&batch, false) {
            Ok(i) => Some(i),
            Err(e) => {
                notes.push(format!("couldn't scan {}: {e:#}", target.name));
                None
            }
        }
    } else {
        servers::load_cached(inp.paths, &target.name)
    };

    let cwd_written = b.cwd.clone().unwrap_or_else(|| "~".into());
    let cwd_dir = if cwd_written.starts_with('/') || cwd_written.starts_with('~') {
        cwd_written.clone()
    } else {
        format!("~/{cwd_written}")
    };
    let needs_auto = b.runtimes.values().any(|v| v.eq_ignore_ascii_case("auto"));
    let mut wanted: Vec<&str> = Vec::new();
    if needs_auto {
        wanted.extend(runtimes::VERSION_FILES);
    }
    if let Some(f) = &b.dotenv {
        wanted.push(f.as_str());
    }
    let files = if inp.online && !wanted.is_empty() {
        servers::fetch_files(&batch, &cwd_dir, &wanted).unwrap_or_else(|e| {
            notes.push(format!("couldn't read project files: {e:#}"));
            BTreeMap::new()
        })
    } else {
        BTreeMap::new()
    };

    let mut prelude = vec![format!("cd {}", sh_cwd(b.cwd.as_deref()))];
    let (act, pre_steps, mut description) = activation(&b.env, inv.as_ref(), &target.name)?;
    prelude.extend(act);
    let mut label = if b.env.is_inherit() { String::new() } else { b.env.label() };
    for (lang, spec) in &b.runtimes {
        if let Some((cmds, l)) = runtime(lang, spec, inv.as_ref(), &files)? {
            prelude.extend(cmds);
            description = format!("{description} · {l}");
            label = if label.is_empty() { l } else { format!("{label} + {l}") };
        }
    }
    for p in b.path.iter().rev() {
        let e = if p.starts_with('/') || p.starts_with('~') { sh_path(p) } else { format!("\"$PWD\"/{}", q(p)) };
        prelude.push(format!("export PATH={e}:\"$PATH\""));
    }
    if let Some(f) = &b.dotenv {
        match files.get(f.as_str()) {
            Some(text) => {
                for item in dotenvy::from_read_iter(text.as_bytes()) {
                    let (k, v) = item.with_context(|| format!("parsing {f} on {}", target.name))?;
                    prelude.push(format!("export {k}={}", q(&v)));
                }
            }
            None if inp.online && notes.is_empty() => bail!("{f} not found in {cwd_written} on {}", target.name),
            None => prelude.push(format!("set -a && . {} && set +a", q(f))),
        }
    }
    for (k, v) in &b.vars {
        // `${OTHER}` expands on the server.
        let val = if v.contains("${") { dq(v) } else { q(v) };
        prelude.push(format!("export {k}={val}"));
    }
    for (k, v) in &req.overrides {
        prelude.push(format!("export {k}={}", q(v)));
    }

    let shell = remote_shell(&target, inv.as_ref());
    let values = &req.params;
    let mut steps: Vec<PlannedStep> = Vec::new();
    let mut wsteps: Vec<WrapperStep> = Vec::new();
    for c in &pre_steps {
        steps.push(PlannedStep {
            label: c.clone(),
            command: c.clone(),
            cwd: PathBuf::from(&cwd_written),
            continue_on_error: false,
            synthetic: true,
        });
        wsteps.push(WrapperStep {
            label: c.clone(),
            run: format!("{shell} -c {}", q(c)),
            cd: None,
            display: c.clone(),
            continue_on_error: false,
        });
    }
    let user_steps = b.effective_steps();
    let last = user_steps.len().saturating_sub(1);
    for (i, s) in user_steps.iter().enumerate() {
        let mut command = template::render(&s.run, values).map_err(anyhow::Error::msg)?;
        if i == last {
            for a in &req.extra_args {
                command.push(' ');
                command.push_str(&q(a));
            }
        }
        let step_cwd = s.cwd.as_ref().map(|c| template::render(c, values)).transpose().map_err(anyhow::Error::msg)?;
        let label = s.name.clone().unwrap_or_else(|| command.lines().next().unwrap_or_default().to_string());
        steps.push(PlannedStep {
            label: label.clone(),
            command: command.clone(),
            cwd: PathBuf::from(match &step_cwd {
                Some(c) => format!("{cwd_written}/{c}"),
                None => cwd_written.clone(),
            }),
            continue_on_error: s.continue_on_error,
            synthetic: false,
        });
        wsteps.push(WrapperStep {
            label,
            run: format!("{shell} -c {}", q(&command)),
            cd: step_cwd.as_deref().map(|c| if c.starts_with('/') || c.starts_with('~') { sh_path(c) } else { q(c) }),
            display: command,
            continue_on_error: s.continue_on_error,
        });
    }

    let title = format!(
        "{} · {}:{} · {}",
        entry.name,
        target.name,
        cwd_written,
        if description.is_empty() { "login environment".to_string() } else { description.clone() }
    );
    let detach_script = b.detach.then(|| {
        let state = format!("\"{}/{}.state\"", DETACH_DIR, tmux_name(&entry.name));
        format!(
            "{HELPERS}{}",
            wrapper(
                &wsteps,
                &WrapperOptions {
                    markers: false,
                    state_file: Some(state),
                    title: title.clone(),
                    prelude: prelude.clone()
                }
            )
        )
    });
    let script = format!(
        "{HELPERS}{}",
        wrapper(&wsteps, &WrapperOptions { markers: inp.markers, state_file: None, title, prelude })
    );
    let mut args = servers::ssh_args(&target, Purpose::Interactive, inp.paths);
    args.push(remote_command(&script, &shell, target.login));

    let mut params = values.clone();
    for p in template::bookmark_params(b) {
        if let (false, Some(d)) = (params.contains_key(&p.name), p.default) {
            params.insert(p.name, d);
        }
    }
    let env_label = if label.is_empty() { target.name.clone() } else { format!("{}: {label}", target.name) };
    Ok(RemotePlan {
        name: entry.name.clone(),
        entry_id: entry.id.clone(),
        program: inp.ssh_program.clone(),
        env: servers::ssh_env(inp.base_env),
        target,
        args,
        script,
        cwd: cwd_written,
        env_label,
        env_description: description,
        steps,
        params,
        forwards: b.forward.clone(),
        detach: b.detach,
        detach_script,
        notes,
    })
}

// ---------- project scripts on a server ----------

/// Scripts in a folder on a server, plus the env and runtime pins to apply to them.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteScripts {
    pub scripts: Vec<crate::scripts::ProjectScript>,
    pub env: EnvSpec,
    pub runtimes: BTreeMap<String, String>,
}

pub fn scan_scripts(b: &Batch, dir: &str) -> Result<RemoteScripts> {
    let mut wanted: Vec<&str> = crate::scripts::SCRIPT_FILES.to_vec();
    wanted.extend(runtimes::VERSION_FILES);
    wanted.push("environment.yml");
    wanted.sort();
    wanted.dedup();
    let folder = servers::fetch_folder(b, dir, &wanted)?;
    let read = |f: &str| folder.files.get(f).cloned();
    let exists = |f: &str| folder.names.iter().any(|n| n == f);
    let env = if exists("mise.toml") || exists(".mise.toml") {
        EnvSpec::Mise
    } else if exists("uv.lock") {
        EnvSpec::Uv { sync: false }
    } else if exists(".venv") {
        EnvSpec::Venv { path: ".venv".into() }
    } else if exists("venv") {
        EnvSpec::Venv { path: "venv".into() }
    } else if exists("poetry.lock") {
        EnvSpec::Poetry
    } else if let Some(name) = read("environment.yml").and_then(|t| {
        t.lines().find_map(|l| l.strip_prefix("name:").map(|n| n.trim().trim_matches(['"', '\'']).to_string()))
    }) {
        EnvSpec::Conda { name: Some(name), prefix: None }
    } else {
        EnvSpec::Inherit
    };
    let runtimes = if matches!(env, EnvSpec::Mise) {
        BTreeMap::new()
    } else {
        runtimes::SUPPORTED
            .iter()
            .filter(|l| runtimes::hint_in(l, &read).is_some())
            .map(|l| (l.to_string(), "auto".to_string()))
            .collect()
    };
    Ok(RemoteScripts { scripts: crate::scripts::discover_with(&read, &exists), env, runtimes })
}

/// `server:path` (for `bk scan gpu:~/proj`), when the prefix names a server.
pub fn split_server_path<'a>(spec: &'a str, servers_: &BTreeMap<String, Server>) -> Option<(&'a str, &'a str)> {
    let (server, path) = spec.split_once(':')?;
    if server.is_empty() || server.contains('/') {
        return None;
    }
    let known = servers_.contains_key(server)
        || server.contains('@')
        || servers::ssh_config_hosts().iter().any(|h| h == server);
    known.then_some((server, if path.is_empty() { "~" } else { path }))
}

// ---------- keep running after disconnect (tmux) ----------

/// Where detached runs keep their script, progress and log on the server.
pub const DETACH_DIR: &str = "$HOME/.local/state/bk";

/// tmux session name for a bookmark (`.` and `:` are special to tmux).
pub fn tmux_name(bookmark: &str) -> String {
    let safe: String =
        bookmark.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("bk-{safe}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxState {
    /// No session (or no tmux).
    Absent,
    /// The job is still running.
    Alive,
    NoTmux,
}

/// Is there a detached run for this bookmark on the server?
pub fn tmux_state(b: &Batch, bookmark: &str) -> Result<TmuxState> {
    let s = tmux_name(bookmark);
    let out = b.script(
        &format!(
            "if ! command -v tmux >/dev/null 2>&1; then echo notmux; elif tmux has-session -t {} 2>/dev/null; then echo alive; else echo absent; fi",
            q(&format!("={s}"))
        ),
        std::time::Duration::from_secs(15),
    )?;
    Ok(match out.trim() {
        "alive" => TmuxState::Alive,
        "notmux" => TmuxState::NoTmux,
        _ => TmuxState::Absent,
    })
}

/// Upload the run script, then (interactively) start it in a tmux session and attach.
pub fn detached_start(plan: &RemotePlan, b: &Batch, paths: &Paths) -> Result<Vec<String>> {
    let script = plan.detach_script.as_deref().context("bookmark isn't set to keep running")?;
    let s = tmux_name(&plan.name);
    let dir = DETACH_DIR;
    let up = b.exec(
        &format!("mkdir -p \"{dir}\" && cat > \"{dir}/{s}.sh\" && : > \"{dir}/{s}.state\""),
        script.as_bytes(),
        std::time::Duration::from_secs(20),
    )?;
    if up.status != Some(0) {
        bail!("couldn't upload the run script: {}", up.stderr.trim());
    }
    let interactive = start_command(&plan.name, &remote_shell(&plan.target, None));
    let mut args = servers::ssh_args(&plan.target, Purpose::Interactive, paths);
    args.push(remote_command(&interactive, "sh", plan.target.login));
    Ok(args)
}

/// The interactive part of starting a detached run: new tmux session running the uploaded
/// script, log its output, and attach.
pub fn start_command(bookmark: &str, shell: &str) -> String {
    let s = tmux_name(bookmark);
    let dir = DETACH_DIR;
    let pane = format!("{shell} -l \"{dir}/{s}.sh\"; printf '\\n[finished: press Enter to close]'; read _");
    format!(
        "tmux new-session -d -s {sess} {pane} && tmux pipe-pane -t {target} -o {log}; exec tmux attach -t {target}\n",
        sess = q(&s),
        pane = q(&pane),
        target = q(&format!("={s}")),
        log = q(&format!("cat >> \"{dir}/{s}.log\"")),
    )
}

/// Attach to a detached run that's still going.
pub fn detached_attach(plan: &RemotePlan, paths: &Paths) -> Vec<String> {
    let mut args = servers::ssh_args(&plan.target, Purpose::Interactive, paths);
    let cmd = format!("exec tmux attach -t {}\n", q(&format!("={}", tmux_name(&plan.name))));
    args.push(remote_command(&cmd, "sh", plan.target.login));
    args
}

/// Progress lines (`step;N`, `exit;N;C`, `done;C`) written by a detached run.
pub fn detached_progress(b: &Batch, bookmark: &str) -> Result<Vec<String>> {
    let s = tmux_name(bookmark);
    let out =
        b.exec(&format!("cat \"{DETACH_DIR}/{s}.state\" 2>/dev/null; true"), b"", std::time::Duration::from_secs(10))?;
    Ok(out.stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Stop a detached run for good.
pub fn detached_kill(b: &Batch, bookmark: &str) {
    let s = tmux_name(bookmark);
    let _ = b.script(
        &format!("tmux kill-session -t {} 2>/dev/null; true", q(&format!("={s}"))),
        std::time::Duration::from_secs(10),
    );
}

/// An interactive shell on the server in the bookmark's folder and env ("Open shell here",
/// `bk go`). Uses bash with an rcfile so `~/.bashrc` can't undo the activation.
pub fn shell_here(entry: &Entry, inp: &RemoteInputs) -> Result<(Target, Vec<String>)> {
    let b = &entry.bookmark;
    let server = b.server.as_deref().context("bookmark has no server")?;
    let target = servers::resolve_target(server, inp.servers)?;
    let inv = servers::load_cached(inp.paths, &target.name);
    let mut lines = vec![format!("cd {}", sh_cwd(b.cwd.as_deref()))];
    let (act, _, _) = activation(&b.env, inv.as_ref(), &target.name)?;
    lines.extend(act);
    for (lang, spec) in &b.runtimes {
        if let Ok(Some((cmds, _))) = runtime(lang, spec, inv.as_ref(), &BTreeMap::new()) {
            lines.extend(cmds);
        }
    }
    for (k, v) in &b.vars {
        lines.push(format!("export {k}={}", q(v)));
    }
    let rc = format!("[ -f ~/.bashrc ] && . ~/.bashrc\n{HELPERS}{}\nrm -f \"$BK_RC\"\n", lines.join("\n"));
    let script = format!(
        "BK_RC=$(mktemp 2>/dev/null || echo /tmp/bk-rc-$$)\nexport BK_RC\ncat > \"$BK_RC\" <<'BK_RC_EOF'\n{rc}BK_RC_EOF\nexec bash --rcfile \"$BK_RC\" -i\n"
    );
    let mut args = servers::ssh_args(&target, Purpose::Interactive, inp.paths);
    args.push(remote_command(&script, "bash", target.login));
    Ok((target, args))
}

/// Plain interactive login session (`bk ssh`, the app's Connect).
pub fn session_args(target: &Target, paths: &Paths) -> Vec<String> {
    servers::ssh_args(target, Purpose::Interactive, paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bookmark, Step};
    use crate::store::Scope;
    use std::path::Path;
    use std::process::Command;

    fn inputs<'a>(servers: &'a BTreeMap<String, Server>, paths: &'a Paths, env: &'a EnvMap) -> RemoteInputs<'a> {
        RemoteInputs { servers, paths, ssh_program: "ssh".into(), base_env: env, online: false, markers: true }
    }

    #[test]
    fn tmux_start_command() {
        let c = start_command("train.big", "bash");
        assert!(Command::new("/bin/sh").args(["-n", "-c", &c]).status().unwrap().success(), "{c}");
        assert!(
            c.starts_with(
                "tmux new-session -d -s bk-train_big 'bash -l \"$HOME/.local/state/bk/bk-train_big.sh\"; printf"
            ),
            "{c}"
        );
        assert!(c.contains("exec tmux attach -t '=bk-train_big'"), "{c}");
        assert!(c.contains("[finished: press Enter to close]"), "{c}");
    }

    #[test]
    fn paths_on_the_server() {
        assert_eq!(sh_path("~"), "\"$HOME\"");
        assert_eq!(sh_path("~/my proj"), "\"$HOME\"/'my proj'");
        assert_eq!(sh_path("/srv/app"), "/srv/app");
        assert_eq!(sh_cwd(Some("work/x")), "\"$HOME\"/work/x");
        assert_eq!(sh_cwd(None), "\"$HOME\"");
    }

    #[test]
    fn transport_round_trips_through_shells() {
        let script = "printf '%s\\n' \"it's $((1+2)) \\\"quoted\\\" ~ ! $HOME\" | sed 's/ /_/g'\n";
        let cmd = remote_command(script, "sh", false);
        // The remote login shell parses `cmd`; try the shells we have.
        for login_shell in ["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash", "/bin/csh"] {
            if !Path::new(login_shell).exists() {
                continue;
            }
            let out = Command::new(login_shell).arg("-c").arg(&cmd).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                text.contains("it's_3_\"quoted\"_~_!_/"),
                "{login_shell}: {text:?} {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn builds_a_valid_remote_script() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { config_file: d.path().join("b.toml"), data_dir: d.path().join("data") };
        let mut servers = BTreeMap::new();
        servers.insert(
            "gpu".to_string(),
            Server { host: "10.0.0.5".into(), user: Some("u".into()), ..Default::default() },
        );
        // Cached discovery for the server.
        let inv = servers::parse_inventory(
            "tool\tbash\nconda_base\t/home/u/miniconda3\nconda_env\tsam2\t/home/u/miniconda3/envs/sam2\n\
             rt\tnode\tv22.14.0\t/home/u/.nvm/versions/node/v22.14.0\tnvm\n",
        );
        servers::save_cached(&paths, "gpu", &inv).unwrap();
        let b = Bookmark {
            server: Some("gpu".into()),
            cwd: Some("~/sam2".into()),
            env: EnvSpec::Conda { name: Some("sam2".into()), prefix: None },
            runtimes: [("node".to_string(), "22".to_string())].into(),
            vars: [("A".to_string(), "x y".to_string())].into(),
            steps: vec![
                Step::new("python train.py --epochs {{epochs:3}}"),
                Step { run: "ls".into(), cwd: Some("runs".into()), ..Default::default() },
            ],
            forward: vec!["6006:127.0.0.1:6006".into()],
            ..Default::default()
        };
        let e = Entry::new("train", Scope::Global, &paths.config_file, b);
        let env = EnvMap::new();
        let plan = build(&e, &RunRequest::default(), &inputs(&servers, &paths, &env)).unwrap();
        assert!(
            Command::new("/bin/sh").args(["-n", "-c", &plan.script]).status().unwrap().success(),
            "{}",
            plan.script
        );
        assert!(plan.script.contains("cd \"$HOME\"/sam2"), "{}", plan.script);
        assert!(plan.script.contains("bk_conda /home/u/miniconda3 sam2"), "{}", plan.script);
        assert!(plan.script.contains("/home/u/.nvm/versions/node/v22.14.0/bin"), "{}", plan.script);
        assert!(plan.script.contains("python train.py --epochs 3"), "{}", plan.script);
        assert_eq!(plan.env_label, "gpu: conda:sam2 + node 22.14.0 (nvm)");
        assert_eq!(plan.args.iter().filter(|a| *a == "--").count(), 1);
        assert_eq!(plan.args[plan.args.len() - 2], "u@10.0.0.5");
        assert!(plan.args.last().unwrap().starts_with("exec bash -lc 'eval"));
        assert_eq!(plan.args_with_forwards()[..2], ["-L".to_string(), "6006:127.0.0.1:6006".to_string()]);
        // Unknown conda env is caught early using discovery.
        let mut bad = e.clone();
        bad.bookmark.env = EnvSpec::Conda { name: Some("nope".into()), prefix: None };
        let err = build(&bad, &RunRequest::default(), &inputs(&servers, &paths, &env)).unwrap_err().to_string();
        assert!(err.contains("not found on gpu (has: sam2)"), "{err}");
    }
}
