//! SSH servers: where they come from (Clibooker + ~/.ssh/config), validation, building
//! `ssh` invocations, connection checks, and discovering what's installed on a server.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::env::EnvMap;
use crate::env::conda::CondaEnv;
use crate::env::runtimes::Install;
use crate::model::Server;
use crate::paths::{self, Paths};

/// `-o` options a Clibooker server may set. Anything riskier (ProxyCommand, LocalCommand,
/// PKCS11Provider, ...) belongs in ~/.ssh/config.
pub const ALLOWED_OPTIONS: &[&str] = &[
    "ServerAliveInterval",
    "ServerAliveCountMax",
    "ConnectTimeout",
    "Compression",
    "StrictHostKeyChecking",
    "AddKeysToAgent",
    "UseKeychain",
    "IdentitiesOnly",
    "ConnectionAttempts",
];

fn bad(msg: impl Into<String>) -> String {
    msg.into()
}

/// A hostname, IP, alias or user name: no spaces, no leading `-`.
fn validate_token(what: &str, s: &str) -> Result<(), String> {
    let ok =
        !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || "._%:[]-".contains(c));
    if ok { Ok(()) } else { Err(bad(format!("invalid {what} `{s}`"))) }
}

/// `user@host[:port]`, a host/alias, or a server name.
pub fn validate_destination(s: &str) -> Result<(), String> {
    let s = s.trim();
    let (user, rest) = match s.split_once('@') {
        Some((u, r)) => (Some(u), r),
        None => (None, s),
    };
    if let Some(u) = user {
        validate_token("user", u)?;
    }
    validate_token("host", rest)
}

/// Local forwards only, bound to loopback: `[127.0.0.1:|localhost:]LPORT:HOST:PORT`.
pub fn validate_forward(f: &str) -> Result<(), String> {
    let parts: Vec<&str> = f.split(':').collect();
    let (bind, rest) = match parts.len() {
        3 => (None, &parts[..]),
        4 => (Some(parts[0]), &parts[1..]),
        _ => return Err(bad(format!("forward `{f}` should look like 8888:127.0.0.1:8888"))),
    };
    if let Some(b) = bind
        && !matches!(b, "127.0.0.1" | "localhost")
    {
        return Err(bad(format!("forward `{f}`: only 127.0.0.1/localhost may be bound locally")));
    }
    let port_ok = |p: &str| p.parse::<u16>().is_ok_and(|n| n > 0);
    if !port_ok(rest[0]) || !port_ok(rest[2]) {
        return Err(bad(format!("forward `{f}`: ports must be numbers")));
    }
    validate_token("forward host", rest[1])
}

/// Local port of a forward spec.
pub fn forward_local_port(f: &str) -> Option<u16> {
    let parts: Vec<&str> = f.split(':').collect();
    let i = if parts.len() == 4 { 1 } else { 0 };
    parts.get(i)?.parse().ok()
}

/// Remote `host:port` of a forward spec.
pub fn forward_remote(f: &str) -> Option<(String, u16)> {
    let parts: Vec<&str> = f.split(':').collect();
    let n = parts.len();
    Some((parts.get(n.checked_sub(2)?)?.to_string(), parts.last()?.parse().ok()?))
}

pub fn validate_server(name: &str, s: &Server) -> Result<(), String> {
    crate::store::validate_name(name).map_err(|e| e.to_string())?;
    validate_token("host", &s.host)?;
    if let Some(u) = &s.user {
        validate_token("user", u)?;
    }
    if let Some(j) = &s.jump {
        for hop in j.split(',') {
            validate_destination(hop)?;
        }
    }
    if let Some(i) = &s.identity {
        if let Some(name) = crate::keys::saved_name(i) {
            crate::keys::validate_name(name).map_err(|e| e.to_string())?;
        } else if i.trim().is_empty() || i.starts_with('-') {
            return Err(bad(format!("invalid identity file `{i}`")));
        }
    }
    for f in &s.forward {
        validate_forward(f)?;
    }
    for (k, v) in &s.options {
        if !ALLOWED_OPTIONS.iter().any(|a| a.eq_ignore_ascii_case(k)) {
            return Err(bad(format!(
                "ssh option `{k}` isn't allowed here (allowed: {}); put it in ~/.ssh/config",
                ALLOWED_OPTIONS.join(", ")
            )));
        }
        if v.is_empty() || v.chars().any(|c| c.is_whitespace() || c == '=') {
            return Err(bad(format!("invalid value for {k}")));
        }
    }
    if let Some(sh) = &s.shell
        && !matches!(sh.as_str(), "bash" | "sh" | "zsh" | "dash" | "ksh")
    {
        return Err(bad(format!("server shell `{sh}` isn't supported (use bash, sh, zsh, dash or ksh)")));
    }
    Ok(())
}

// ---------- ~/.ssh/config ----------

fn ssh_dir() -> PathBuf {
    paths::home().join(".ssh")
}

/// `~/.ssh/config` hosts to show next to Clibooker's servers: not shadowed by a Clibooker
/// server of the same name and not hidden in settings.
pub fn listed_config_hosts(servers: &BTreeMap<String, Server>, hidden: &[String]) -> Vec<String> {
    ssh_config_hosts().into_iter().filter(|h| !servers.contains_key(h) && !hidden.contains(h)).collect()
}

/// Host aliases from ~/.ssh/config (following Include), skipping wildcard/negated patterns.
pub fn ssh_config_hosts() -> Vec<String> {
    let mut out = Vec::new();
    let mut seen_files = std::collections::HashSet::new();
    collect_hosts(&ssh_dir().join("config"), &mut out, &mut seen_files, 0);
    let mut uniq = std::collections::HashSet::new();
    out.retain(|h| uniq.insert(h.clone()));
    out
}

fn split_config_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // `Keyword value`, `Keyword=value`, `Keyword = value`
    let idx = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..idx].to_ascii_lowercase();
    let rest = line[idx..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in rest.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    Some((key, args))
}

fn collect_hosts(file: &Path, out: &mut Vec<String>, seen: &mut std::collections::HashSet<PathBuf>, depth: u8) {
    if depth > 8 || !seen.insert(file.to_path_buf()) {
        return;
    }
    let Ok(text) = fs::read_to_string(file) else { return };
    for line in text.lines() {
        let Some((key, args)) = split_config_line(line) else { continue };
        match key.as_str() {
            "host" => {
                out.extend(
                    args.into_iter().filter(|a| !a.contains(['*', '?', '!']) && validate_token("host", a).is_ok()),
                );
            }
            "include" => {
                for pattern in args {
                    let p = paths::expand_tilde(&pattern);
                    let p = if p.is_absolute() { p } else { ssh_dir().join(p) };
                    for f in glob_files(&p) {
                        collect_hosts(&f, out, seen, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Minimal glob for Include: `*` and `?` in the last path component.
fn glob_files(p: &Path) -> Vec<PathBuf> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return if p.is_file() { vec![p.to_path_buf()] } else { Vec::new() };
    }
    let dir = p.parent().unwrap_or(Path::new("/"));
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let re = regex::Regex::new(&format!("^{}$", regex::escape(&name).replace(r"\*", ".*").replace(r"\?", ".")))
        .expect("glob regex");
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| re.is_match(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

// ---------- targets & argv ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Clibooker,
    SshConfig,
    Literal,
}

/// Everything needed to reach a server.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    /// Display name (server name, alias, or the literal).
    pub name: String,
    pub source: Source,
    /// What ssh connects to: `user@host`, or an ssh-config alias.
    pub destination: String,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub jump: Option<String>,
    pub forward: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub shell: Option<String>,
    pub login: bool,
}

/// Resolve a bookmark's `server` value.
pub fn resolve_target(spec: &str, servers: &BTreeMap<String, Server>) -> Result<Target> {
    let spec = spec.trim();
    if let Some(s) = servers.get(spec) {
        validate_server(spec, s).map_err(anyhow::Error::msg)?;
        let destination = match &s.user {
            Some(u) => format!("{u}@{}", s.host),
            None => s.host.clone(),
        };
        // A jump given as another server's name becomes that server's `user@host:port`.
        let jump = s.jump.as_ref().map(|j| {
            j.split(',')
                .map(|hop| match servers.get(hop) {
                    Some(h) => {
                        let mut d =
                            h.user.as_ref().map(|u| format!("{u}@{}", h.host)).unwrap_or_else(|| h.host.clone());
                        if let Some(p) = h.port {
                            d = format!("{d}:{p}");
                        }
                        d
                    }
                    None => hop.to_string(),
                })
                .collect::<Vec<_>>()
                .join(",")
        });
        return Ok(Target {
            name: spec.to_string(),
            source: Source::Clibooker,
            destination,
            port: s.port,
            identity: s.identity.clone(),
            jump,
            forward: s.forward.clone(),
            options: s.options.clone(),
            shell: s.shell.clone(),
            login: s.login.unwrap_or(true),
        });
    }
    validate_destination(spec).map_err(anyhow::Error::msg)?;
    let is_alias = !spec.contains('@') && ssh_config_hosts().iter().any(|h| h == spec);
    // `user@host:port` literal (not for IPv6 brackets).
    let (destination, port) = match spec.rsplit_once(':') {
        Some((d, p)) if !spec.contains('[') && p.parse::<u16>().is_ok() => (d.to_string(), p.parse().ok()),
        _ => (spec.to_string(), None),
    };
    Ok(Target {
        name: spec.to_string(),
        source: if is_alias { Source::SshConfig } else { Source::Literal },
        destination,
        port,
        identity: None,
        jump: None,
        forward: Vec::new(),
        options: BTreeMap::new(),
        shell: None,
        login: true,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A run or session with a terminal.
    Interactive,
    /// Background query over a shared (multiplexed) connection; never prompts.
    Batch,
    /// Port forwards only.
    Tunnel,
}

/// Whether background ssh calls share one connection (ControlMaster). Windows' OpenSSH has no
/// connection sharing, so there every call opens its own connection.
pub const MULTIPLEX: bool = cfg!(unix);

/// Directory for ControlMaster sockets; falls back to /tmp when the path would exceed
/// macOS's 104-byte socket limit.
#[cfg(unix)]
pub fn control_dir(paths: &Paths) -> PathBuf {
    let preferred = paths.data_dir.join("ssh");
    // %C expands to 40 hex chars.
    let dir = if preferred.as_os_str().len() + 42 < 100 {
        preferred
    } else {
        PathBuf::from(format!("/tmp/bk-{}", unsafe { libc_getuid() }))
    };
    if fs::create_dir_all(&dir).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    dir
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// macOS's own OpenSSH (`/usr/bin/ssh`, `/usr/bin/ssh-add`), the only one that knows the
/// Keychain: others refuse to start with `UseKeychain`.
pub fn apple_openssh(program: &str) -> bool {
    cfg!(target_os = "macos") && program.starts_with("/usr/bin/")
}

/// ssh arguments (everything after the program name, up to and including the destination)
/// for running `program`.
///
/// A saved key (`key:NAME`) is used when it's on this device; otherwise ssh falls back to the
/// agent and the default keys. In a terminal, its passphrase is remembered once typed: the
/// agent keeps it, and Apple's ssh the Keychain (unless the server's options say otherwise).
pub fn ssh_args(program: &str, t: &Target, purpose: Purpose, paths: &Paths) -> Vec<String> {
    let mut a: Vec<String> = vec!["-e".into(), "none".into()];
    let mut opt = |k: &str, v: &str| {
        a.push("-o".into());
        a.push(format!("{k}={v}"));
    };
    let has = |name: &str| t.options.keys().any(|k| k.eq_ignore_ascii_case(name));
    if !has("ServerAliveInterval") {
        opt("ServerAliveInterval", "15");
        opt("ServerAliveCountMax", "4");
    }
    let apple = apple_openssh(program);
    for (k, v) in &t.options {
        if k.eq_ignore_ascii_case("UseKeychain") && !apple {
            continue;
        }
        opt(k, v);
    }
    let identity = t.identity.as_deref().and_then(|i| crate::keys::identity_file(i, paths));
    let saved_key = identity.is_some() && t.identity.as_deref().and_then(crate::keys::saved_name).is_some();
    if saved_key {
        if purpose == Purpose::Interactive && !has("AddKeysToAgent") {
            opt("AddKeysToAgent", "yes");
        }
        if apple && !has("UseKeychain") {
            opt("UseKeychain", "yes");
        }
    }
    match purpose {
        Purpose::Interactive => a.push("-tt".into()),
        Purpose::Batch => {
            a.push("-T".into());
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut o = vec![("BatchMode", "yes".to_string()), ("ConnectTimeout", "6".to_string())];
            #[cfg(unix)]
            o.extend([
                ("ControlMaster", "auto".to_string()),
                ("ControlPath", control_dir(paths).join("%C").display().to_string()),
                ("ControlPersist", "60".to_string()),
            ]);
            #[cfg(not(unix))]
            let _ = paths;
            for (k, v) in o {
                a.push("-o".into());
                a.push(format!("{k}={v}"));
            }
        }
        Purpose::Tunnel => {
            a.extend(["-N".into(), "-T".into(), "-o".into(), "ExitOnForwardFailure=yes".into()]);
            if MULTIPLEX {
                a.extend(["-o".into(), "ControlMaster=no".into(), "-S".into(), "none".into()]);
            }
        }
    }
    if let Some(p) = t.port {
        a.extend(["-p".into(), p.to_string()]);
    }
    if let Some(file) = identity {
        a.extend(["-i".into(), file.display().to_string()]);
        a.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
    }
    if let Some(j) = &t.jump {
        a.extend(["-J".into(), j.clone()]);
    }
    a.push("--".into());
    a.push(t.destination.clone());
    a
}

/// The ssh binary: settings, else the system's (/usr/bin/ssh, which on macOS understands
/// UseKeychain; Windows' built-in OpenSSH), else whatever `ssh` is on PATH.
pub fn ssh_program(setting: Option<&str>) -> String {
    if let Some(s) = setting.filter(|s| !s.trim().is_empty()) {
        return paths::expand_tilde(s).display().to_string();
    }
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let p = Path::new(&root).join("System32").join("OpenSSH").join("ssh.exe");
        if p.exists() {
            return p.display().to_string();
        }
    } else if Path::new("/usr/bin/ssh").exists() {
        return "/usr/bin/ssh".into();
    }
    "ssh".into()
}

/// Environment for the local ssh process: just what ssh needs. `LC_*` is dropped because
/// macOS ssh forwards it (`SendEnv LC_*`) and it breaks locales on Linux servers.
pub fn ssh_env(base: &EnvMap) -> EnvMap {
    const KEEP: &[&str] = &[
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "SHELL",
        "SSH_AUTH_SOCK",
        "TMPDIR",
        "LANG",
        "TERM",
        "DISPLAY",
        "SSH_ASKPASS",
        "SSH_ASKPASS_REQUIRE",
        // Windows: without these ssh.exe can't find its config, keys, agent or even start Winsock.
        "SystemRoot",
        "windir",
        "SystemDrive",
        "USERPROFILE",
        "USERNAME",
        "HOMEDRIVE",
        "HOMEPATH",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "PATHEXT",
        "ComSpec",
        "TEMP",
        "TMP",
    ];
    base.iter().filter(|(k, _)| KEEP.iter().any(|keep| k.is(keep))).map(|(k, v)| (k.clone(), v.clone())).collect()
}

// ---------- batch execution over the shared connection ----------

pub struct Batch<'a> {
    pub program: String,
    pub target: &'a Target,
    pub paths: &'a Paths,
    pub env: EnvMap,
}

pub struct BatchOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Batch<'_> {
    fn command(&self, remote: &str) -> Command {
        let mut c = crate::proc::background(&self.program);
        c.args(ssh_args(&self.program, self.target, Purpose::Batch, self.paths));
        c.arg(remote);
        c.env_clear().envs(&self.env);
        c
    }

    /// Run `remote` (a command line for the server's login shell), feeding `stdin`.
    pub fn exec(&self, remote: &str, stdin: &[u8], timeout: Duration) -> Result<BatchOutput> {
        let start = Instant::now();
        let mut child = self
            .command(remote)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", self.program))?;
        if let Some(mut w) = child.stdin.take() {
            let data = stdin.to_vec();
            std::thread::spawn(move || {
                let _ = w.write_all(&data);
            });
        }
        let out = wait_with_timeout(child, timeout)?;
        Ok(BatchOutput {
            status: out.0,
            stdout: String::from_utf8_lossy(&out.1).into_owned(),
            stderr: String::from_utf8_lossy(&out.2).into_owned(),
            elapsed: start.elapsed(),
        })
    }

    /// Run a POSIX script with `sh -s` and return what it printed between sentinels
    /// (so banners and rc-file noise are ignored).
    pub fn script(&self, script: &str, timeout: Duration) -> Result<String> {
        let nonce = format!("{:x}", crate::history::now_ms());
        let begin = format!("__BK_BEGIN_{nonce}__");
        let end = format!("__BK_END_{nonce}__");
        let full = format!("echo {begin}\n{script}\necho {end}\n");
        let out = self.exec("exec sh -s", full.as_bytes(), timeout)?;
        let Some(start) = out.stdout.find(&begin) else {
            bail!("{}", describe_failure(out.status, &out.stderr));
        };
        let body = &out.stdout[start + begin.len()..];
        let body = body.split(&end).next().unwrap_or(body);
        Ok(body.trim_start_matches(['\r', '\n']).to_string())
    }
}

pub(crate) fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let t1 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let t2 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st.code();
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok((status, t1.join().unwrap_or_default(), t2.join().unwrap_or_default()))
}

// ---------- status ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Reach {
    Reachable { ms: u64 },
    AuthRequired,
    UnknownHostKey,
    ChangedHostKey,
    Unreachable { reason: String },
}

pub fn classify(status: Option<i32>, stderr: &str, elapsed: Duration) -> Reach {
    let e = stderr.to_ascii_lowercase();
    if status == Some(0) {
        return Reach::Reachable { ms: elapsed.as_millis() as u64 };
    }
    if e.contains("remote host identification has changed") {
        Reach::ChangedHostKey
    } else if e.contains("host key verification failed") || e.contains("no matching host key") && e.contains("known") {
        Reach::UnknownHostKey
    } else if e.contains("permission denied") || e.contains("too many authentication failures") {
        Reach::AuthRequired
    } else {
        let reason = stderr
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
            .unwrap_or("connection failed")
            .to_string();
        Reach::Unreachable { reason }
    }
}

fn describe_failure(status: Option<i32>, stderr: &str) -> String {
    match classify(status, stderr, Duration::ZERO) {
        Reach::AuthRequired => {
            "the server needs a password or key that isn't loaded (ssh-add, or connect once interactively)".into()
        }
        Reach::UnknownHostKey => "the server's host key isn't trusted yet (connect once, or use Trust host)".into(),
        Reach::ChangedHostKey => {
            "the server's host key CHANGED — refusing to connect (check ~/.ssh/known_hosts)".into()
        }
        Reach::Unreachable { reason } => reason,
        Reach::Reachable { .. } => "unexpected output from the server".into(),
    }
}

pub fn check(b: &Batch) -> Reach {
    match b.exec("true", b"", Duration::from_secs(12)) {
        Ok(out) => classify(out.status, &out.stderr, out.elapsed),
        Err(e) => Reach::Unreachable { reason: format!("{e:#}") },
    }
}

// ---------- discovery ----------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteInventory {
    pub probed_ms: i64,
    pub os: String,
    pub shell: String,
    pub home: String,
    /// Tools found on PATH: bash, tmux, conda, uv, poetry, pyenv, mise, micromamba, ...
    pub tools: Vec<String>,
    pub conda_base: Option<String>,
    pub conda_envs: Vec<CondaEnv>,
    /// Installed runtimes per language, newest first (paths are on the server).
    pub runtimes: BTreeMap<String, Vec<Install>>,
}

impl RemoteInventory {
    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }
}

/// POSIX script listing what's installed. One line per fact: `kind<TAB>a<TAB>b<TAB>c`.
pub const PROBE_SCRIPT: &str = r#"
t=$(printf '\t')
echo "os${t}$(uname -sm 2>/dev/null)"
echo "shell${t}${SHELL:-}"
echo "home${t}$HOME"
for x in bash zsh tmux screen conda micromamba mamba uv poetry pyenv mise rustup node python3 git docker nvidia-smi; do
  command -v "$x" >/dev/null 2>&1 && echo "tool${t}$x"
done
[ -s "${NVM_DIR:-$HOME/.nvm}/nvm.sh" ] && echo "tool${t}nvm"
cb=""
for b in "${CONDA_EXE%/bin/conda}" "$HOME/miniconda3" "$HOME/anaconda3" "$HOME/miniforge3" "$HOME/mambaforge" "$HOME/micromamba" /opt/conda /opt/miniconda3 /opt/anaconda3 /opt/miniforge3 /usr/local/miniconda3 /usr/local/anaconda3; do
  if [ -n "$b" ] && [ -f "$b/etc/profile.d/conda.sh" ]; then cb="$b"; break; fi
done
if [ -n "$cb" ]; then
  echo "conda_base${t}$cb"
  echo "conda_env${t}base${t}$cb"
  for d in "$cb"/envs/*/; do [ -d "${d}conda-meta" ] && echo "conda_env${t}$(basename "$d")${t}${d%/}"; done
fi
[ -f "$HOME/.conda/environments.txt" ] && while IFS= read -r l; do [ -d "$l/conda-meta" ] && echo "conda_env${t}$(basename "$l")${t}$l"; done < "$HOME/.conda/environments.txt"
lsd() { for d in "$2"/*/; do [ -d "$d" ] && echo "rt${t}$1${t}$(basename "$d")${t}${d%/}${t}$3"; done; }
lsd node "${NVM_DIR:-$HOME/.nvm}/versions/node" nvm
for f in "${FNM_DIR:-$HOME/.local/share/fnm}" "$HOME/.fnm"; do for d in "$f"/node-versions/*/; do [ -d "$d" ] && echo "rt${t}node${t}$(basename "$d")${t}${d}installation${t}fnm"; done; done
lsd node "$HOME/.volta/tools/image/node" volta
lsd node "${MISE_DATA_DIR:-$HOME/.local/share/mise}/installs/node" mise
lsd node "$HOME/.asdf/installs/nodejs" asdf
lsd java "$HOME/.sdkman/candidates/java" sdkman
lsd java /usr/lib/jvm system
lsd java "${MISE_DATA_DIR:-$HOME/.local/share/mise}/installs/java" mise
lsd ruby "$HOME/.rbenv/versions" rbenv
lsd ruby "$HOME/.rubies" chruby
lsd ruby "$HOME/.rvm/rubies" rvm
lsd go "$HOME/.goenv/versions" goenv
for d in "$HOME"/sdk/go*/; do [ -d "$d" ] && echo "rt${t}go${t}$(basename "$d")${t}${d%/}${t}go sdk"; done
[ -f /usr/local/go/VERSION ] && echo "rt${t}go${t}$(head -n1 /usr/local/go/VERSION)${t}/usr/local/go${t}system"
lsd rust "${RUSTUP_HOME:-$HOME/.rustup}/toolchains" rustup
lsd python "${PYENV_ROOT:-$HOME/.pyenv}/versions" pyenv
true
"#;

pub fn parse_inventory(text: &str) -> RemoteInventory {
    let mut inv = RemoteInventory { probed_ms: crate::history::now_ms(), ..Default::default() };
    for line in text.lines() {
        let f: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        match f.as_slice() {
            ["os", v] => inv.os = v.to_string(),
            ["shell", v] => inv.shell = v.to_string(),
            ["home", v] => inv.home = v.to_string(),
            ["tool", v] => inv.tools.push(v.to_string()),
            ["conda_base", v] => inv.conda_base = Some(v.to_string()),
            ["conda_env", name, prefix] => {
                if !inv.conda_envs.iter().any(|e| e.prefix == *prefix) {
                    inv.conda_envs.push(CondaEnv { name: name.to_string(), prefix: prefix.to_string() });
                }
            }
            ["rt", lang, version, home, source] => {
                if *version == "current" || version.is_empty() {
                    continue;
                }
                let version = version.trim_start_matches('v').trim_start_matches("go").trim_start_matches("ruby-");
                let list = inv.runtimes.entry(lang.to_string()).or_default();
                if !list.iter().any(|i| i.version == version) {
                    list.push(Install {
                        version: version.to_string(),
                        home: PathBuf::from(home),
                        source: source.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    for list in inv.runtimes.values_mut() {
        list.sort_by_key(|i| std::cmp::Reverse(crate::env::runtimes::parse_version(&i.version)));
    }
    inv
}

pub fn probe(b: &Batch) -> Result<RemoteInventory> {
    let text = b.script(PROBE_SCRIPT, Duration::from_secs(20))?;
    Ok(parse_inventory(&text))
}

fn cache_file(paths: &Paths, name: &str) -> PathBuf {
    let safe: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-@".contains(c) { c } else { '_' }).collect();
    paths.data_dir.join("servers").join(format!("{safe}.json"))
}

/// Drop a server's cached scan (when the server is deleted).
pub fn forget_cached(paths: &Paths, name: &str) {
    let _ = fs::remove_file(cache_file(paths, name));
}

pub fn load_cached(paths: &Paths, name: &str) -> Option<RemoteInventory> {
    serde_json::from_str(&fs::read_to_string(cache_file(paths, name)).ok()?).ok()
}

pub fn save_cached(paths: &Paths, name: &str, inv: &RemoteInventory) -> Result<()> {
    let f = cache_file(paths, name);
    fs::create_dir_all(f.parent().expect("has parent"))?;
    fs::write(f, serde_json::to_string_pretty(inv)?)?;
    Ok(())
}

/// Cached inventory, probing (and caching) when there is none.
pub fn inventory(b: &Batch, refresh: bool) -> Result<RemoteInventory> {
    if !refresh && let Some(inv) = load_cached(b.paths, &b.target.name) {
        return Ok(inv);
    }
    let inv = probe(b)?;
    let _ = save_cached(b.paths, &b.target.name, &inv);
    Ok(inv)
}

/// A folder on a server: small files we asked for, plus the names of everything in it.
#[derive(Debug, Clone, Default)]
pub struct RemoteFiles {
    pub files: BTreeMap<String, String>,
    pub names: Vec<String>,
}

/// Read small files from a remote folder in one round trip. Missing files are absent.
pub fn fetch_files(b: &Batch, dir: &str, names: &[&str]) -> Result<BTreeMap<String, String>> {
    Ok(fetch_folder(b, dir, names)?.files)
}

pub fn fetch_folder(b: &Batch, dir: &str, names: &[&str]) -> Result<RemoteFiles> {
    let mut script = format!(
        "cd {} 2>/dev/null || exit 0\nls -1A 2>/dev/null | sed 's/^/__BK_LS__ /'\n",
        crate::remote::sh_path(dir)
    );
    for n in names {
        let q = crate::render::q(n);
        script.push_str(&format!(
            "if [ -f {q} ] && [ $(wc -c < {q}) -lt 200000 ]; then echo \"__BK_FILE__ {n}\"; cat {q}; echo; echo \"__BK_EOF__\"; fi\n"
        ));
    }
    let text = b.script(&script, Duration::from_secs(15))?;
    let mut out = BTreeMap::new();
    let mut listing = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        if cur.is_none()
            && let Some(name) = line.strip_prefix("__BK_LS__ ")
        {
            listing.push(name.trim_end().to_string());
        } else if let Some(name) = line.strip_prefix("__BK_FILE__ ") {
            cur = Some((name.trim().to_string(), Vec::new()));
        } else if line.trim_end() == "__BK_EOF__" {
            if let Some((name, lines)) = cur.take() {
                out.insert(name, lines.join("\n"));
            }
        } else if let Some((_, lines)) = cur.as_mut() {
            lines.push(line);
        }
    }
    Ok(RemoteFiles { files: out, names: listing })
}

// ---------- host keys ----------

/// Resolved `hostname` and `port` for a target, via `ssh -G` (handles ssh-config aliases).
pub fn resolved_host(program: &str, t: &Target, env: &EnvMap) -> Option<(String, u16)> {
    let mut c = crate::proc::background(program);
    c.arg("-G");
    if let Some(p) = t.port {
        c.args(["-p", &p.to_string()]);
    }
    let out = c.arg(&t.destination).env_clear().envs(env).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k} ")).map(str::to_string));
    Some((get("hostname")?, get("port")?.parse().ok()?))
}

/// Fingerprints of the server's host keys (for the "Trust host" dialog).
pub fn host_fingerprints(program: &str, t: &Target, env: &EnvMap) -> Result<Vec<String>> {
    let (host, port) = resolved_host(program, t, env).context("could not resolve the host name")?;
    let keys = crate::proc::background("ssh-keyscan")
        .args(["-T", "6", "-p", &port.to_string(), "--", &host])
        .stderr(Stdio::null())
        .output()
        .context("running ssh-keyscan")?;
    if keys.stdout.is_empty() {
        bail!("couldn't fetch host keys from {host}:{port}");
    }
    let mut child = crate::proc::background("ssh-keygen")
        .args(["-l", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running ssh-keygen")?;
    child.stdin.take().expect("piped").write_all(&keys.stdout)?;
    let out = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

/// After the user confirmed the fingerprint: let ssh record the key (never accepts a *changed* key).
pub fn accept_host_key(b: &Batch) -> Reach {
    let mut t = b.target.clone();
    t.options.insert("StrictHostKeyChecking".into(), "accept-new".into());
    let batch = Batch { program: b.program.clone(), target: &t, paths: b.paths, env: b.env.clone() };
    check(&batch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str) -> Server {
        Server { host: host.into(), ..Default::default() }
    }

    #[test]
    fn destinations_and_forwards() {
        assert!(validate_destination("ubuntu@10.0.0.5").is_ok());
        assert!(validate_destination("gpu-box").is_ok());
        assert!(validate_destination("[fe80::1]").is_ok());
        assert!(validate_destination("-oProxyCommand=evil").is_err());
        assert!(validate_destination("a b").is_err());
        assert!(validate_destination("u@-x").is_err());
        assert!(validate_forward("8888:127.0.0.1:8888").is_ok());
        assert!(validate_forward("localhost:8888:db:5432").is_ok());
        assert!(validate_forward("0.0.0.0:8888:x:1").is_err());
        assert!(validate_forward("8888").is_err());
        assert_eq!(forward_local_port("127.0.0.1:9000:x:80"), Some(9000));
        assert_eq!(forward_remote("9000:x:80"), Some(("x".to_string(), 80)));
    }

    #[test]
    fn option_allowlist() {
        let mut s = server("h");
        s.options.insert("ServerAliveInterval".into(), "30".into());
        assert!(validate_server("gpu", &s).is_ok());
        s.options.insert("ProxyCommand".into(), "nc".into());
        assert!(validate_server("gpu", &s).unwrap_err().contains("isn't allowed"));
        let mut s = server("h");
        s.shell = Some("fish".into());
        assert!(validate_server("gpu", &s).is_err());
    }

    #[test]
    fn argv_for_each_purpose() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { config_file: d.path().join("b.toml"), data_dir: d.path().join("data") };
        let mut servers = BTreeMap::new();
        servers.insert(
            "gpu".to_string(),
            Server {
                host: "10.0.0.5".into(),
                user: Some("ubuntu".into()),
                port: Some(2222),
                identity: Some("~/.ssh/k".into()),
                jump: Some("bastion".into()),
                ..Default::default()
            },
        );
        servers.insert(
            "bastion".to_string(),
            Server { host: "b.example".into(), user: Some("me".into()), port: Some(22), ..Default::default() },
        );
        let t = resolve_target("gpu", &servers).unwrap();
        let a = ssh_args("ssh", &t, Purpose::Interactive, &paths);
        let s = a.join(" ");
        assert!(s.starts_with("-e none -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -tt -p 2222 -i "), "{s}");
        assert!(s.contains("-o IdentitiesOnly=yes -J me@b.example:22 -- ubuntu@10.0.0.5"), "{s}");
        assert_eq!(a.last().unwrap(), "ubuntu@10.0.0.5");
        let b = ssh_args("ssh", &t, Purpose::Batch, &paths).join(" ");
        assert!(b.contains("-T -o BatchMode=yes"), "{b}");
        assert_eq!(b.contains("ControlMaster=auto"), MULTIPLEX, "{b}");
        let tun = ssh_args("ssh", &t, Purpose::Tunnel, &paths).join(" ");
        assert!(tun.contains("-N -T -o ExitOnForwardFailure=yes"), "{tun}");
        assert_eq!(tun.contains("-o ControlMaster=no -S none"), MULTIPLEX, "{tun}");
        let lit = resolve_target("deploy@web1.example:2200", &BTreeMap::new()).unwrap();
        assert_eq!((lit.destination.as_str(), lit.port), ("deploy@web1.example", Some(2200)));
        assert!(resolve_target("-oProxyCommand=x", &BTreeMap::new()).is_err());
    }

    #[test]
    fn saved_keys_in_argv() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths { config_file: d.path().join("b.toml"), data_dir: d.path().join("data") };
        let mut servers = BTreeMap::new();
        let mut lab = Server { host: "h".into(), identity: Some("key:lab".into()), ..Default::default() };
        servers.insert("lab".to_string(), lab.clone());
        let argv = |servers: &BTreeMap<String, Server>, program: &str, purpose| {
            ssh_args(program, &resolve_target("lab", servers).unwrap(), purpose, &paths).join(" ")
        };
        // Not on this device: ssh falls back to the agent and default keys.
        let a = argv(&servers, "ssh", Purpose::Interactive);
        assert!(!a.contains("-i ") && !a.contains("IdentitiesOnly") && !a.contains("AddKeysToAgent"), "{a}");

        let key = crate::keys::dir(&paths).join("lab");
        fs::create_dir_all(key.parent().unwrap()).unwrap();
        fs::write(&key, "").unwrap();
        let a = argv(&servers, "ssh", Purpose::Interactive);
        assert!(a.contains("-o AddKeysToAgent=yes"), "{a}");
        assert!(a.contains(&format!("-i {} -o IdentitiesOnly=yes", key.display())), "{a}");
        assert!(!a.contains("UseKeychain"), "only Apple's ssh knows it: {a}");
        assert_eq!(
            argv(&servers, "/usr/bin/ssh", Purpose::Interactive).contains("-o UseKeychain=yes"),
            cfg!(target_os = "macos")
        );
        assert_eq!(
            argv(&servers, "/usr/bin/ssh", Purpose::Batch).contains("-o UseKeychain=yes"),
            cfg!(target_os = "macos")
        );
        assert!(!argv(&servers, "ssh", Purpose::Batch).contains("AddKeysToAgent"));

        // The server's own options win.
        lab.options.insert("AddKeysToAgent".into(), "no".into());
        lab.options.insert("UseKeychain".into(), "no".into());
        servers.insert("lab".to_string(), lab.clone());
        let a = argv(&servers, "/usr/bin/ssh", Purpose::Interactive);
        assert!(a.contains("AddKeysToAgent=no") && !a.contains("AddKeysToAgent=yes"), "{a}");
        assert!(!a.contains("UseKeychain=yes"), "{a}");
        // Homebrew's ssh on a Mac would refuse UseKeychain.
        assert!(!argv(&servers, "/opt/homebrew/bin/ssh", Purpose::Interactive).contains("UseKeychain"));

        lab.identity = Some("key:Bad.Name".into());
        assert!(validate_server("lab", &lab).is_err());
    }

    #[test]
    fn ssh_config_parsing() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("config");
        fs::create_dir_all(d.path().join("conf.d")).unwrap();
        fs::write(
            &main,
            format!(
                "Host gpu \"lab box\" *.corp !bad\n  HostName 1.2.3.4\nhost=db\nInclude {}/conf.d/*.conf\nMatch host x\n  User y\n",
                d.path().display()
            ),
        )
        .unwrap();
        // Including the main file again must not loop.
        fs::write(
            d.path().join("conf.d/a.conf"),
            format!("Host orb\n  HostName 127.0.0.1\nInclude {}\n", main.display()),
        )
        .unwrap();
        let mut out = Vec::new();
        collect_hosts(&main, &mut out, &mut Default::default(), 0);
        // "lab box" has a space → invalid alias, skipped; wildcard/negated skipped; include loop safe.
        assert_eq!(out, vec!["gpu", "db", "orb"]);
    }

    #[test]
    fn status_classification() {
        let z = Duration::from_millis(42);
        assert_eq!(classify(Some(0), "", z), Reach::Reachable { ms: 42 });
        assert_eq!(classify(Some(255), "user@h: Permission denied (publickey).", z), Reach::AuthRequired);
        assert_eq!(classify(Some(255), "Host key verification failed.", z), Reach::UnknownHostKey);
        assert_eq!(
            classify(
                Some(255),
                "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\nHost key verification failed.",
                z
            ),
            Reach::ChangedHostKey
        );
        assert_eq!(
            classify(Some(255), "ssh: connect to host 10.9.9.9 port 22: Operation timed out", z),
            Reach::Unreachable { reason: "ssh: connect to host 10.9.9.9 port 22: Operation timed out".into() }
        );
    }

    #[test]
    fn inventory_parsing() {
        let text = "os\tLinux x86_64\nshell\t/bin/bash\nhome\t/home/u\ntool\tbash\ntool\ttmux\nconda_base\t/home/u/miniconda3\n\
conda_env\tbase\t/home/u/miniconda3\nconda_env\tsam2\t/home/u/miniconda3/envs/sam2\n\
rt\tnode\tv18.20.1\t/home/u/.nvm/versions/node/v18.20.1\tnvm\nrt\tnode\tv22.14.0\t/home/u/.nvm/versions/node/v22.14.0\tnvm\n\
rt\tgo\tgo1.22.3\t/usr/local/go\tsystem\nrt\tjava\tcurrent\t/x\tsdkman\n";
        let inv = parse_inventory(text);
        assert!(inv.has("tmux") && inv.has("bash"));
        assert_eq!(inv.conda_envs.len(), 2);
        assert_eq!(inv.runtimes["node"][0].version, "22.14.0");
        assert_eq!(inv.runtimes["go"][0].version, "1.22.3");
        assert!(!inv.runtimes.contains_key("java"));
    }

    #[cfg(unix)]
    #[test]
    fn probe_script_is_valid_sh() {
        let st = Command::new("/bin/sh").args(["-n", "-c", PROBE_SCRIPT]).status().unwrap();
        assert!(st.success());
        // And it runs locally, producing parseable output.
        let out = Command::new("/bin/sh").args(["-c", PROBE_SCRIPT]).output().unwrap();
        let inv = parse_inventory(&String::from_utf8_lossy(&out.stdout));
        assert!(!inv.home.is_empty());
    }
}
