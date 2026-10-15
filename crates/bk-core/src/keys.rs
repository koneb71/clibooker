//! Saved SSH keys: private keys kept in this device's data folder (`keys/NAME`, with
//! `NAME.pub` next to it) and used by servers as `identity = "key:NAME"`. They sync with
//! everything else, inside the encrypted snapshot.
//!
//! Clibooker never sees a passphrase or a password: making a key, unlocking it and putting it
//! on a server run OpenSSH's own tools in a terminal, and you type into their prompts.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::env::EnvMap;
use crate::model::Server;
use crate::paths::{self, Paths};
use crate::servers::{self, Purpose, Target};
use crate::{proc, remote, store};

/// How a server names a saved key: `identity = "key:lab"`.
pub const PREFIX: &str = "key:";

/// Bigger than any SSH private key (an RSA 16384 one is about 12 KiB).
const MAX_KEY_BYTES: usize = 64 * 1024;

pub fn dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("keys")
}

/// The keys folder, private to you (0700 on Unix).
fn ensure_dir(paths: &Paths) -> Result<PathBuf> {
    let d = dir(paths);
    fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700))?;
    }
    Ok(d)
}

/// Lowercase letters, digits, `-` and `_`, the same on every OS: a name is a file name on
/// each device the key syncs to, and macOS and Windows don't tell `Lab` from `lab`.
pub fn validate_name(name: &str) -> Result<()> {
    let ok_chars = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if name.is_empty() || name.len() > 64 || !ok_chars || name.starts_with('-') {
        bail!("invalid key name `{name}`: use lowercase letters, digits, `-` and `_` (max 64)");
    }
    if windows_device_name(name) {
        bail!("`{name}` can't be a file name on Windows; pick another name");
    }
    Ok(())
}

fn windows_device_name(name: &str) -> bool {
    matches!(name, "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"]
            .iter()
            .any(|p| name.strip_prefix(p).is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit() && n != "0"))
}

/// The private key file of a saved key (whether or not it's there).
pub fn path(paths: &Paths, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(dir(paths).join(name))
}

fn pub_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".pub");
    PathBuf::from(s)
}

pub fn exists(paths: &Paths, name: &str) -> bool {
    path(paths, name).is_ok_and(|p| p.is_file())
}

/// `key:NAME` → `NAME`.
pub fn saved_name(identity: &str) -> Option<&str> {
    identity.strip_prefix(PREFIX)
}

/// The file ssh should use for a server's `identity`: a saved key's when it's on this device
/// (`None` when it isn't), anything else is a path.
pub fn identity_file(identity: &str, paths: &Paths) -> Option<PathBuf> {
    match saved_name(identity) {
        Some(name) => path(paths, name).ok().filter(|p| p.is_file()),
        None => Some(paths::expand_tilde(identity)),
    }
}

// ---------- reading keys ----------

/// SSH's wire format: big-endian lengths before each string.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        (self.0.len() >= n).then(|| {
            let (head, rest) = self.0.split_at(n);
            self.0 = rest;
            head
        })
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

/// What a private key's text says about it, read without decrypting it.
#[derive(Debug, Clone, PartialEq)]
pub struct Private {
    /// It has a passphrase.
    pub encrypted: bool,
    /// Its public key, in OpenSSH's own format (older PEM files don't carry it).
    pub public_blob: Option<Vec<u8>>,
}

fn armored(text: &str, label: &str) -> Option<String> {
    let body = text.strip_prefix(&format!("-----BEGIN {label}-----"))?;
    let end = body.find(&format!("-----END {label}-----"))?;
    Some(body[..end].split_whitespace().collect())
}

/// Read a private key's header: OpenSSH's format, or the older PEM ones. Public keys, PuTTY
/// keys and anything else are refused with what to do instead.
pub fn parse_private(text: &str) -> Result<Private> {
    let text = text.trim();
    if text.len() > MAX_KEY_BYTES {
        bail!("that's too big to be an SSH private key");
    }
    if text.starts_with("PuTTY-User-Key-File-") {
        bail!("this is a PuTTY key: open it in PuTTYgen, choose Conversions → Export OpenSSH key, and import that");
    }
    if text.starts_with("---- BEGIN SSH2 PUBLIC KEY") || parse_public(text.lines().next().unwrap_or("")).is_ok() {
        bail!("this is a public key: import the private key (the file without .pub)");
    }
    if let Some(body) = armored(text, "OPENSSH PRIVATE KEY") {
        let data = STANDARD.decode(body).context("this OpenSSH private key is damaged")?;
        let mut r = Reader(data.strip_prefix(b"openssh-key-v1\0").context("this OpenSSH private key is damaged")?);
        let header = (|| {
            let cipher = r.string()?;
            let (_kdf, _kdf_options) = (r.string()?, r.string()?);
            let keys = r.u32()?;
            Some((cipher, keys, r.string()?))
        })();
        let Some((cipher, 1.., blob)) = header else { bail!("this OpenSSH private key is damaged") };
        return Ok(Private { encrypted: cipher != b"none", public_blob: Some(blob.to_vec()) });
    }
    for label in ["RSA PRIVATE KEY", "EC PRIVATE KEY", "DSA PRIVATE KEY", "PRIVATE KEY", "ENCRYPTED PRIVATE KEY"] {
        if armored(text, label).is_some() {
            let encrypted = label.starts_with("ENCRYPTED") || text.contains("Proc-Type: 4,ENCRYPTED");
            return Ok(Private { encrypted, public_blob: None });
        }
    }
    bail!("this isn't an SSH private key (one looks like ~/.ssh/id_ed25519)")
}

/// A public key line: `ssh-ed25519 AAAA… comment`.
#[derive(Debug, Clone, PartialEq)]
pub struct Public {
    pub algorithm: String,
    pub blob: Vec<u8>,
    pub comment: String,
}

impl Public {
    pub fn line(&self) -> String {
        let key = format!("{} {}", self.algorithm, STANDARD.encode(&self.blob));
        if self.comment.is_empty() { key } else { format!("{key} {}", self.comment) }
    }
}

pub fn parse_public(line: &str) -> Result<Public> {
    let line = line.trim();
    let (algorithm, rest) = line.split_once(char::is_whitespace).context("not a public key line")?;
    let rest = rest.trim_start();
    let (data, comment) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let blob = STANDARD.decode(data).context("not a public key line")?;
    if Reader(&blob).string() != Some(algorithm.as_bytes()) {
        bail!("not a public key line");
    }
    Ok(Public { algorithm: algorithm.to_string(), blob, comment: comment.trim().to_string() })
}

/// `SHA256:…`, as `ssh-keygen -l` and `ssh-add -l` show it.
pub fn fingerprint(blob: &[u8]) -> String {
    format!("SHA256:{}", STANDARD_NO_PAD.encode(&Sha256::digest(blob)[..]))
}

/// `ED25519`, `RSA 4096`, `ECDSA 256`, …
pub fn kind(blob: &[u8]) -> String {
    let mut r = Reader(blob);
    let algorithm = String::from_utf8_lossy(r.string().unwrap_or_default()).into_owned();
    match algorithm.as_str() {
        "ssh-ed25519" => "ED25519".into(),
        "ssh-rsa" => {
            let n = r.string().and_then(|_e| r.string()).unwrap_or_default();
            let n: Vec<u8> = n.iter().copied().skip_while(|b| *b == 0).collect();
            let bits = n.first().map_or(0, |b| (n.len() - 1) * 8 + (8 - b.leading_zeros() as usize));
            format!("RSA {bits}")
        }
        "ssh-dss" => "DSA".into(),
        "sk-ssh-ed25519@openssh.com" => "ED25519-SK".into(),
        a if a.starts_with("sk-ecdsa-") => "ECDSA-SK".into(),
        a => match a.strip_prefix("ecdsa-sha2-nistp") {
            Some(bits) => format!("ECDSA {bits}"),
            None => a.to_ascii_uppercase(),
        },
    }
}

/// Line endings as OpenSSH writes them (Windows editors and clipboards add `\r`).
fn normalize(text: &str) -> String {
    format!("{}\n", text.trim().replace("\r\n", "\n"))
}

// ---------- the keychain ----------

/// A saved key, as lists show it. Nothing secret.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyInfo {
    pub name: String,
    /// `ED25519`, `RSA 4096`, …
    pub kind: String,
    /// `SHA256:…`
    pub fingerprint: String,
    pub comment: String,
    /// It has a passphrase.
    pub encrypted: bool,
    /// The public key line, for a server's `authorized_keys`.
    pub public: String,
}

/// The public key an OpenSSH private key carries (without its comment, which is encrypted).
fn embedded_public(private: &Private) -> Option<Public> {
    let blob = private.public_blob.clone()?;
    let algorithm = String::from_utf8_lossy(Reader(&blob).string()?).into_owned();
    Some(Public { algorithm, blob, comment: String::new() })
}

/// The public half: `NAME.pub`, or (when that's gone) what the OpenSSH key itself says.
fn public_text(file: &Path, private: Option<&Private>) -> Option<String> {
    fs::read_to_string(pub_path(file)).ok().or_else(|| Some(embedded_public(private?)?.line()))
}

pub fn info(paths: &Paths, name: &str) -> Result<KeyInfo> {
    let file = path(paths, name)?;
    let text = fs::read_to_string(&file).with_context(|| format!("no saved key named `{name}`"))?;
    let private = parse_private(&text).with_context(|| format!("saved key `{name}`"))?;
    let public = public_text(&file, Some(&private)).with_context(|| format!("saved key `{name}` has no {name}.pub"))?;
    let public = parse_public(&public).with_context(|| format!("saved key `{name}`: {name}.pub"))?;
    Ok(KeyInfo {
        name: name.to_string(),
        kind: kind(&public.blob),
        fingerprint: fingerprint(&public.blob),
        comment: public.comment.clone(),
        encrypted: private.encrypted,
        public: public.line(),
    })
}

/// Names in the keys folder: files with a key's name (so not `.pub` files or temporary ones).
fn names(paths: &Paths) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir(paths)) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| validate_name(n).is_ok())
        .collect();
    out.sort();
    out
}

/// Saved keys, by name. One that can't be read is left out (`info` says why).
pub fn list(paths: &Paths) -> Vec<KeyInfo> {
    names(paths).iter().filter_map(|n| info(paths, n).ok()).collect()
}

/// Save a private key as `name`. `public` is its `.pub` line when there is one; otherwise an
/// OpenSSH key's own header has it, and an older PEM key's comes from `ssh-keygen -y` (which
/// only works without a passphrase).
pub fn import(paths: &Paths, name: &str, private: &str, public: Option<&str>, keygen: &str) -> Result<KeyInfo> {
    let file = path(paths, name)?;
    if file.exists() {
        bail!("a key named `{name}` is already saved");
    }
    let private = normalize(private);
    let parsed = parse_private(&private)?;
    let public = match public {
        Some(line) => {
            let p = parse_public(line).context("its .pub file isn't a public key")?;
            if parsed.public_blob.as_ref().is_some_and(|b| *b != p.blob) {
                bail!("its .pub file belongs to another key");
            }
            p
        }
        None => match embedded_public(&parsed) {
            Some(p) => p,
            None => derive_public(paths, &private, keygen)?,
        },
    };
    let fp = fingerprint(&public.blob);
    if let Some(same) = list(paths).into_iter().find(|k| k.fingerprint == fp) {
        bail!("this key is already saved as `{}`", same.name);
    }
    ensure_dir(paths)?;
    store::write_private(&pub_path(&file), format!("{}\n", public.line()).as_bytes())?;
    store::write_private(&file, private.as_bytes())?;
    info(paths, name)
}

/// An older PEM key's public half, from `ssh-keygen -y`.
fn derive_public(paths: &Paths, private: &str, keygen: &str) -> Result<Public> {
    let hint = "convert it to OpenSSH's format first (`ssh-keygen -p -f FILE`), or put its .pub file next to it";
    let mut tmp = tempfile::NamedTempFile::new_in(ensure_dir(paths)?)?;
    std::io::Write::write_all(&mut tmp, private.as_bytes())?;
    let out = proc::background(keygen)
        .args(["-y", "-P", "", "-f"])
        .arg(tmp.path())
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("reading its public key needs {keygen}; {hint}"))?;
    if !out.status.success() {
        bail!("can't read its public key (it has a passphrase?): {hint}");
    }
    parse_public(&String::from_utf8_lossy(&out.stdout))
}

/// Save a key from a file (with its `.pub`, when it's next to it).
pub fn import_file(paths: &Paths, name: &str, file: &Path, keygen: &str) -> Result<KeyInfo> {
    let meta = fs::metadata(file).with_context(|| format!("reading {}", file.display()))?;
    if meta.len() > MAX_KEY_BYTES as u64 {
        bail!("{} is too big to be an SSH private key", file.display());
    }
    let private = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let public = fs::read_to_string(pub_path(file)).ok();
    import(paths, name, &private, public.as_deref(), keygen)
}

/// A name for a key imported from `file`: its file name, except that `id_ed25519` and the
/// like (the same on every machine) become `<device>-ed25519`.
pub fn suggest_name(file: &Path, device: &str) -> String {
    let stem = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = stem.strip_suffix(".pem").or_else(|| stem.strip_suffix(".key")).unwrap_or(&stem);
    let name = match stem.strip_prefix("id_") {
        Some(kind) => format!("{device}-{kind}"),
        None => stem.to_string(),
    };
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase).filter(|c| !matches!(c, '\'' | '’')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(64).collect();
    let out = out.trim_end_matches('-');
    if validate_name(out).is_ok() { out.to_string() } else { "key".into() }
}

/// Delete a saved key (both files). Returns whether it was there.
pub fn remove(paths: &Paths, name: &str) -> Result<bool> {
    let file = path(paths, name)?;
    let existed = file.exists();
    for f in [&file, &pub_path(&file)] {
        match fs::remove_file(f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("deleting {}", f.display()));
            }
            _ => {}
        }
    }
    Ok(existed)
}

/// Clibooker servers that use a saved key.
pub fn used_by(servers: &BTreeMap<String, Server>, name: &str) -> Vec<String> {
    let identity = format!("{PREFIX}{name}");
    servers.iter().filter(|(_, s)| s.identity.as_deref() == Some(&identity)).map(|(n, _)| n.clone()).collect()
}

fn servers_in(file: &Path) -> Result<BTreeMap<String, Server>> {
    Ok(if file.exists() { store::read_file(file)?.servers } else { BTreeMap::new() })
}

/// Rename a saved key, and the servers that use it with it. Returns those servers.
pub fn rename(paths: &Paths, from: &str, to: &str) -> Result<Vec<String>> {
    let (old, new) = (path(paths, from)?, path(paths, to)?);
    if !old.is_file() {
        bail!("no saved key named `{from}`");
    }
    if new.exists() {
        bail!("a key named `{to}` is already saved");
    }
    if pub_path(&old).exists() {
        fs::rename(pub_path(&old), pub_path(&new))?;
    }
    fs::rename(&old, &new)?;
    let file = &paths.config_file;
    let mut renamed = Vec::new();
    for (name, mut server) in servers_in(file)? {
        if server.identity.as_deref() == Some(&format!("{PREFIX}{from}")) {
            server.identity = Some(format!("{PREFIX}{to}"));
            store::upsert_server(file, &name, &server)?;
            renamed.push(name);
        }
    }
    Ok(renamed)
}

// ---------- OpenSSH's tools ----------

/// One of OpenSSH's tools (`ssh-keygen`, `ssh-add`): the one next to the ssh Clibooker runs,
/// so Windows' or Homebrew's OpenSSH gets its own, else whichever is on PATH.
pub fn tool(ssh_program: &str, name: &str) -> String {
    let sibling = Path::new(ssh_program)
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)));
    match sibling {
        Some(p) if p.is_file() => p.display().to_string(),
        _ => name.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyType {
    #[default]
    Ed25519,
    /// For servers too old for Ed25519 (OpenSSH before 6.5).
    Rsa,
}

/// `you@this-device`, the comment new keys get.
pub fn default_comment(paths: &Paths) -> String {
    let device = crate::sync::this_device(paths);
    match std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        Ok(user) if !user.is_empty() => format!("{user}@{device}"),
        _ => device,
    }
}

/// `ssh-keygen` arguments that make a new saved key. Without `passphrase`, it gets none and
/// nothing is asked; otherwise ssh-keygen asks for one.
pub fn generate_args(paths: &Paths, name: &str, comment: &str, kind: KeyType, passphrase: bool) -> Result<Vec<String>> {
    let file = path(paths, name)?;
    if file.exists() {
        bail!("a key named `{name}` is already saved");
    }
    ensure_dir(paths)?;
    let mut a: Vec<String> = match kind {
        KeyType::Ed25519 => vec!["-t".into(), "ed25519".into()],
        KeyType::Rsa => vec!["-t".into(), "rsa".into(), "-b".into(), "4096".into()],
    };
    a.extend(["-q".into(), "-f".into(), file.display().to_string(), "-C".into(), comment.to_string()]);
    if !passphrase {
        a.extend(["-N".into(), String::new()]);
    }
    Ok(a)
}

/// `ssh-keygen` arguments that add or change a saved key's passphrase (it asks for them).
pub fn passphrase_args(paths: &Paths, name: &str) -> Result<Vec<String>> {
    let file = path(paths, name)?;
    if !file.is_file() {
        bail!("no saved key named `{name}`");
    }
    Ok(vec!["-p".into(), "-f".into(), file.display().to_string()])
}

fn agent_args(ssh_add: &str, paths: &Paths, name: &str, remove: bool) -> Result<Vec<String>> {
    let file = path(paths, name)?;
    if !file.is_file() {
        bail!("no saved key named `{name}`");
    }
    let mut a = Vec::new();
    if servers::apple_openssh(ssh_add) {
        a.push("--apple-use-keychain".to_string());
    }
    if remove {
        a.push("-d".into());
    }
    a.push(file.display().to_string());
    Ok(a)
}

/// `ssh-add` arguments that unlock a key: you type its passphrase once, and the agent (and
/// on macOS, the Keychain) keeps it.
pub fn unlock_args(ssh_add: &str, paths: &Paths, name: &str) -> Result<Vec<String>> {
    agent_args(ssh_add, paths, name, false)
}

/// `ssh-add` arguments that lock a key again (and forget its passphrase in the Keychain).
pub fn lock_args(ssh_add: &str, paths: &Paths, name: &str) -> Result<Vec<String>> {
    agent_args(ssh_add, paths, name, true)
}

/// What ssh-agent holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Agent {
    /// The fingerprints of its keys.
    Running(Vec<String>),
    Unavailable(NoAgent),
}

/// Why there's no ssh-agent, and the command that starts one, when there is one to give.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoAgent {
    pub why: String,
    /// Windows: the PowerShell command (run as administrator) that starts the agent service.
    pub fix: Option<String>,
}

impl std::fmt::Display for NoAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)?;
        match &self.fix {
            Some(fix) => write!(f, "; to start it, run in PowerShell as administrator: {fix}"),
            None => Ok(()),
        }
    }
}

impl Agent {
    fn missing(why: impl Into<String>) -> Agent {
        Agent::Unavailable(NoAgent { why: why.into(), fix: None })
    }

    pub fn holds(&self, fingerprint: &str) -> bool {
        matches!(self, Agent::Running(fps) if fps.iter().any(|f| f == fingerprint))
    }
}

/// Ask ssh-agent which keys it holds (`ssh-add -l`). `env` is ssh's environment.
pub fn agent(ssh_add: &str, env: &EnvMap) -> Agent {
    let child = proc::background(ssh_add)
        .args(["-l", "-E", "sha256"])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let out = match child {
        Ok(c) => servers::wait_with_timeout(c, Duration::from_secs(3)),
        Err(e) => return Agent::missing(format!("can't run {ssh_add}: {e}")),
    };
    match out {
        Ok((Some(0), stdout, _)) => Agent::Running(
            String::from_utf8_lossy(&stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().nth(1))
                .filter(|f| f.starts_with("SHA256:"))
                .map(String::from)
                .collect(),
        ),
        // "The agent has no identities."
        Ok((Some(1), _, _)) => Agent::Running(Vec::new()),
        Ok(_) if cfg!(windows) => Agent::Unavailable(NoAgent {
            why: "the OpenSSH Authentication Agent service isn't running".into(),
            fix: Some("Get-Service ssh-agent | Set-Service -StartupType Automatic; Start-Service ssh-agent".into()),
        }),
        Ok(_) if !env.keys().any(|k| k.is("SSH_AUTH_SOCK")) => {
            Agent::missing("ssh-agent isn't running here (SSH_AUTH_SOCK isn't set)")
        }
        Ok((_, _, stderr)) => {
            let why = String::from_utf8_lossy(&stderr).lines().next().unwrap_or_default().trim().to_string();
            Agent::missing(format!("can't reach ssh-agent: {why}"))
        }
        Err(e) => Agent::missing(format!("ssh-agent didn't answer: {e:#}")),
    }
}

// ---------- putting a key on a server ----------

/// A POSIX script that adds `public` to `~/.ssh/authorized_keys` unless it's already there,
/// like `ssh-copy-id`. What it creates is private to you; what's there is left as it is.
pub fn install_script(public: &str) -> Result<String> {
    let p = parse_public(public)?;
    Ok(format!(
        r#"key={key}
data={data}
umask 077
mkdir -p "$HOME/.ssh" || exit 1
f="$HOME/.ssh/authorized_keys"
if [ -f "$f" ] && grep -qF "$data" "$f"; then
  echo "bk: the key was already in ~/.ssh/authorized_keys on $(uname -n)"
else
  if [ -s "$f" ] && [ -n "$(tail -c1 "$f")" ]; then echo >> "$f"; fi
  printf '%s\n' "$key" >> "$f" || exit 1
  echo "bk: added the key to ~/.ssh/authorized_keys on $(uname -n)"
fi
command -v restorecon >/dev/null 2>&1 && restorecon -R "$HOME/.ssh" >/dev/null 2>&1
exit 0
"#,
        key = crate::render::q(&p.line()),
        data = STANDARD.encode(&p.blob),
    ))
}

/// ssh arguments that put saved key `name` on `target`. You log in the way you do now (a
/// password at ssh's prompt, usually); the key itself isn't offered, since it's not there yet.
pub fn install_args(program: &str, target: &Target, paths: &Paths, name: &str) -> Result<Vec<String>> {
    let key = info(paths, name)?;
    let mut t = target.clone();
    if t.identity.as_deref().and_then(saved_name) == Some(name) {
        t.identity = None;
    }
    let mut a = servers::ssh_args(program, &t, Purpose::Interactive, paths);
    a.push(remote::remote_command(&install_script(&key.public)?, "sh", false));
    Ok(a)
}

/// After installing: a Clibooker server now uses the key. Returns false for other targets
/// (an `~/.ssh/config` alias or `user@host`), which Clibooker can't change.
pub fn after_install(paths: &Paths, server: &str, name: &str) -> Result<bool> {
    let file = &paths.config_file;
    let Some(mut s) = servers_in(file)?.remove(server) else { return Ok(false) };
    s.identity = Some(format!("{PREFIX}{name}"));
    store::upsert_server(file, server, &s)?;
    Ok(true)
}

// ---------- sync ----------

/// Saved keys as they sync, by name: `{"private": …, "public": …}`, the files' text as is.
pub fn synced_values(paths: &Paths) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for name in names(paths) {
        let file = dir(paths).join(&name);
        let private = String::from_utf8_lossy(&fs::read(&file)?).into_owned();
        let public = public_text(&file, parse_private(&private).ok().as_ref()).unwrap_or_default();
        out.insert(name, json!({ "private": private, "public": public }));
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// A different key already has this name here: sync never replaces it.
    #[error("a different key with this name is saved here")]
    Clash,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Save a key that came from another device. The same key (by fingerprint) is updated, for a
/// new passphrase or comment; a different one is never overwritten.
pub fn apply_synced(paths: &Paths, name: &str, value: &Value) -> Result<(), ApplyError> {
    let file = path(paths, name)?;
    let text = |k: &str| value.get(k).and_then(Value::as_str).ok_or_else(|| anyhow!("synced key without {k}"));
    let (private, public) = (text("private")?, text("public")?);
    let parsed = parse_private(private)?;
    let incoming = parse_public(public)?;
    if parsed.public_blob.as_ref().is_some_and(|b| *b != incoming.blob) {
        return Err(anyhow!("its public key belongs to another key").into());
    }
    if file.exists() && info(paths, name).ok().map(|k| k.fingerprint) != Some(fingerprint(&incoming.blob)) {
        return Err(ApplyError::Clash);
    }
    ensure_dir(paths)?;
    store::write_private(&pub_path(&file), public.as_bytes())?;
    store::write_private(&file, private.as_bytes())?;
    Ok(())
}

/// Keys for tests: real OpenSSH headers around filler. Only headers are ever read, and a real
/// private key in the repo would trip secret scanners.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    pub fn string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend((s.len() as u32).to_be_bytes());
        out.extend(s);
    }

    pub fn ed25519_blob(seed: u8) -> Vec<u8> {
        let mut b = Vec::new();
        string(&mut b, b"ssh-ed25519");
        string(&mut b, &[seed; 32]);
        b
    }

    /// An OpenSSH private key's text around `blob`, with a passphrase unless `cipher` is `none`.
    pub fn openssh(blob: &[u8], cipher: &str) -> String {
        let mut d = b"openssh-key-v1\0".to_vec();
        string(&mut d, cipher.as_bytes());
        string(&mut d, if cipher == "none" { b"none" } else { b"bcrypt" });
        string(&mut d, b"");
        d.extend(1u32.to_be_bytes());
        string(&mut d, blob);
        string(&mut d, &[7; 64]);
        let body = STANDARD.encode(&d);
        let lines: Vec<&str> = body.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap()).collect();
        format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n", lines.join("\n"))
    }

    /// A different key for each `seed`.
    pub fn key(seed: u8) -> String {
        openssh(&ed25519_blob(seed), "none")
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{ed25519_blob, openssh, string};
    use super::*;

    fn paths(d: &Path) -> Paths {
        Paths { config_file: d.join("bookmarks.toml"), data_dir: d.join("data") }
    }

    #[test]
    fn reads_openssh_headers() {
        let blob = ed25519_blob(1);
        let p = parse_private(&openssh(&blob, "none")).unwrap();
        assert_eq!(p, Private { encrypted: false, public_blob: Some(blob.clone()) });
        assert!(parse_private(&openssh(&blob, "aes256-ctr")).unwrap().encrypted);
        assert_eq!(kind(&blob), "ED25519");
        // As `ssh-keygen -l` computes it: sha256 of the blob, base64 without padding.
        let expected = STANDARD_NO_PAD.encode(&Sha256::digest(&blob)[..]);
        assert_eq!(fingerprint(&blob), format!("SHA256:{expected}"));
        assert!(!fingerprint(&blob).ends_with('='));

        let mut rsa = Vec::new();
        string(&mut rsa, b"ssh-rsa");
        string(&mut rsa, &[1, 0, 1]);
        let mut n = vec![0u8];
        n.extend([0xc5; 512]);
        string(&mut rsa, &n);
        assert_eq!(kind(&rsa), "RSA 4096");
        let mut ec = Vec::new();
        string(&mut ec, b"ecdsa-sha2-nistp384");
        assert_eq!(kind(&ec), "ECDSA 384");
    }

    #[test]
    fn refuses_what_isnt_a_private_key() {
        let public = format!("ssh-ed25519 {} me@mac", STANDARD.encode(ed25519_blob(1)));
        let err = |t: &str| parse_private(t).unwrap_err().to_string();
        assert!(err(&public).contains("public key"));
        assert!(err("---- BEGIN SSH2 PUBLIC KEY ----\nAAAA\n").contains("public key"));
        assert!(err("PuTTY-User-Key-File-3: ssh-ed25519\n").contains("PuTTYgen"));
        assert!(err("hello").contains("isn't an SSH private key"));
        assert!(
            err("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----").contains("damaged")
        );
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n\nAAAA\n-----END RSA PRIVATE KEY-----\n";
        assert_eq!(parse_private(pem).unwrap(), Private { encrypted: true, public_blob: None });
        assert!(!parse_private(&pem.replace("Proc-Type: 4,ENCRYPTED\n", "")).unwrap().encrypted);
    }

    #[test]
    fn public_lines() {
        let blob = ed25519_blob(3);
        let line = format!("ssh-ed25519  {}   me@my mac \n", STANDARD.encode(&blob));
        let p = parse_public(&line).unwrap();
        assert_eq!((p.algorithm.as_str(), p.comment.as_str()), ("ssh-ed25519", "me@my mac"));
        assert_eq!(p.line(), format!("ssh-ed25519 {} me@my mac", STANDARD.encode(&blob)));
        // The type inside the blob must match.
        assert!(parse_public(&format!("ssh-rsa {}", STANDARD.encode(&blob))).is_err());
        assert!(parse_public("ssh-ed25519 !!!").is_err());
    }

    #[test]
    fn names_are_safe_file_names_everywhere() {
        for ok in ["lab", "gpu-2", "a_b", "x", "com0", "console"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Lab", "a.b", "x.pub", "-x", "../x", "a/b", "con", "nul", "com1", "lpt9", &"k".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        assert!(path(&paths(Path::new("/d")), "../../.bashrc").is_err());
        assert_eq!(
            suggest_name(Path::new("/h/.ssh/id_ed25519"), "Neiell’s MacBook Pro"),
            "neiells-macbook-pro-ed25519"
        );
        assert_eq!(suggest_name(Path::new("/h/.ssh/lab_key.pem"), "mac"), "lab_key");
        assert_eq!(suggest_name(Path::new("C:/keys/Work Server.key"), "pc"), "work-server");
        assert_eq!(suggest_name(Path::new("/h/....."), "pc"), "key");
        assert_eq!(suggest_name(Path::new("/h/con"), "pc"), "key");
    }

    #[test]
    fn identities() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        assert_eq!(saved_name("key:lab"), Some("lab"));
        assert_eq!(identity_file("key:lab", &p), None);
        assert_eq!(identity_file("key:../x", &p), None);
        fs::create_dir_all(dir(&p)).unwrap();
        fs::write(dir(&p).join("lab"), "x").unwrap();
        assert_eq!(identity_file("key:lab", &p), Some(dir(&p).join("lab")));
        assert_eq!(identity_file("~/k", &p), Some(paths::home().join("k")));
    }

    #[test]
    fn import_list_rename_remove() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        let blob = ed25519_blob(5);
        let private = openssh(&blob, "aes256-ctr").replace('\n', "\r\n");
        let public = format!("ssh-ed25519 {} me@mac\n", STANDARD.encode(&blob));

        // Pasted text, without a .pub: the key's own header gives the public half.
        let k = import(&p, "lab", &private, None, "ssh-keygen").unwrap();
        assert_eq!((k.kind.as_str(), k.encrypted, k.comment.as_str()), ("ED25519", true, ""));
        assert_eq!(k.fingerprint, fingerprint(&blob));
        let saved = fs::read_to_string(dir(&p).join("lab")).unwrap();
        assert!(!saved.contains('\r') && saved.ends_with("-----\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |f: &Path| fs::metadata(f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir(&p)), 0o700);
            assert_eq!(mode(&dir(&p).join("lab")), 0o600);
            assert_eq!(mode(&dir(&p).join("lab.pub")), 0o600);
        }
        assert!(import(&p, "lab", &private, None, "ssh-keygen").unwrap_err().to_string().contains("already saved"));
        assert!(import(&p, "lab2", &private, None, "ssh-keygen").unwrap_err().to_string().contains("saved as `lab`"));

        // From a file, with its .pub (and its comment).
        let src = d.path().join("id_ed25519");
        let other = ed25519_blob(6);
        fs::write(&src, openssh(&other, "none")).unwrap();
        fs::write(pub_path(&src), format!("ssh-ed25519 {} me@pc\n", STANDARD.encode(&other))).unwrap();
        let k = import_file(&p, "pc", &src, "ssh-keygen").unwrap();
        assert_eq!((k.comment.as_str(), k.encrypted), ("me@pc", false));
        // A .pub that isn't this key's is refused.
        fs::write(pub_path(&src), &public).unwrap();
        assert!(import_file(&p, "pc2", &src, "ssh-keygen").unwrap_err().to_string().contains("another key"));

        let names: Vec<String> = list(&p).into_iter().map(|k| k.name).collect();
        assert_eq!(names, ["lab", "pc"]);

        // Renaming takes the servers along.
        let server = Server { host: "h".into(), identity: Some("key:lab".into()), ..Default::default() };
        store::upsert_server(&p.config_file, "gpu", &server).unwrap();
        assert_eq!(rename(&p, "lab", "work").unwrap(), ["gpu"]);
        assert_eq!(store::read_file(&p.config_file).unwrap().servers["gpu"].identity.as_deref(), Some("key:work"));
        assert_eq!(used_by(&store::read_file(&p.config_file).unwrap().servers, "work"), ["gpu"]);
        assert!(rename(&p, "work", "pc").is_err());

        assert!(remove(&p, "work").unwrap());
        assert!(!remove(&p, "work").unwrap());
        assert!(!dir(&p).join("work.pub").exists());
        assert_eq!(list(&p).len(), 1);
    }

    #[test]
    fn synced_keys_never_replace_a_different_one() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (paths(&d.path().join("a")), paths(&d.path().join("b")));
        let blob = ed25519_blob(8);
        import(&a, "lab", &openssh(&blob, "none"), None, "ssh-keygen").unwrap();
        let values = synced_values(&a).unwrap();
        let v = &values["lab"];
        assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["private", "public"]);

        apply_synced(&b, "lab", v).unwrap();
        assert_eq!(synced_values(&b).unwrap(), values, "what's written reads back the same");
        assert_eq!(info(&b, "lab").unwrap().fingerprint, fingerprint(&blob));

        // The same key with a passphrase now: updated.
        let locked = json!({ "private": openssh(&blob, "aes256-ctr"), "public": v["public"] });
        apply_synced(&b, "lab", &locked).unwrap();
        assert!(info(&b, "lab").unwrap().encrypted);

        // Another key under the same name: never.
        let other = ed25519_blob(9);
        let other = json!({
            "private": openssh(&other, "none"),
            "public": format!("ssh-ed25519 {}\n", STANDARD.encode(&other)),
        });
        assert!(matches!(apply_synced(&b, "lab", &other), Err(ApplyError::Clash)));
        assert!(info(&b, "lab").unwrap().encrypted);
        assert!(matches!(apply_synced(&b, "../evil", v), Err(ApplyError::Other(_))));
    }

    #[test]
    fn tools_and_arguments() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        assert_eq!(tool("ssh", "ssh-add"), "ssh-add");
        let bin = d.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = |n: &str| bin.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
        fs::write(exe("ssh-keygen"), "").unwrap();
        assert_eq!(tool(&exe("ssh").display().to_string(), "ssh-keygen"), exe("ssh-keygen").display().to_string());
        assert_eq!(tool(&exe("ssh").display().to_string(), "ssh-add"), "ssh-add");

        let a = generate_args(&p, "new", "me@mac", KeyType::Ed25519, false).unwrap();
        let file = dir(&p).join("new").display().to_string();
        assert_eq!(a, ["-t", "ed25519", "-q", "-f", &file, "-C", "me@mac", "-N", ""]);
        let a = generate_args(&p, "new", "me@mac", KeyType::Rsa, true).unwrap();
        assert_eq!(a[..4], ["-t", "rsa", "-b", "4096"]);
        assert!(unlock_args("ssh-add", &p, "new").unwrap_err().to_string().contains("no saved key"));
        import(&p, "lab", &openssh(&ed25519_blob(1), "none"), None, "ssh-keygen").unwrap();
        let lab = dir(&p).join("lab").display().to_string();
        assert_eq!(unlock_args("ssh-add", &p, "lab").unwrap(), [lab.as_str()]);
        assert_eq!(lock_args("ssh-add", &p, "lab").unwrap(), ["-d", lab.as_str()]);
        assert_eq!(passphrase_args(&p, "lab").unwrap(), ["-p", "-f", lab.as_str()]);
        assert!(generate_args(&p, "lab", "", KeyType::Ed25519, false).is_err());
    }

    #[cfg(unix)]
    fn ssh_keygen() -> Option<&'static str> {
        std::process::Command::new("ssh-keygen").arg("-?").output().ok().map(|_| "ssh-keygen")
    }

    /// Against real keys: kinds and fingerprints as ssh-keygen sees them, and PEM keys.
    #[cfg(unix)]
    #[test]
    fn matches_ssh_keygen() {
        let Some(keygen) = ssh_keygen() else { return };
        let d = tempfile::tempdir().unwrap();
        let p = paths(d.path());
        let made = |file: &Path, args: &[&str]| {
            let st = std::process::Command::new(keygen)
                .args(args)
                .arg("-f")
                .arg(file)
                .args(["-C", "me@test", "-q"])
                .stdin(Stdio::null())
                .status()
                .unwrap();
            assert!(st.success());
            let out = std::process::Command::new(keygen).args(["-l", "-E", "sha256", "-f"]).arg(file).output().unwrap();
            String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).unwrap().to_string()
        };
        for (name, args, kind_, encrypted) in [
            ("ed", &["-t", "ed25519", "-N", ""][..], "ED25519", false),
            ("rsa", &["-t", "rsa", "-b", "3072", "-N", ""][..], "RSA 3072", false),
            ("ec", &["-t", "ecdsa", "-b", "521", "-N", ""][..], "ECDSA 521", false),
            ("locked", &["-t", "ed25519", "-N", "secret phrase"][..], "ED25519", true),
        ] {
            let src = d.path().join(format!("src-{name}"));
            let fp = made(&src, args);
            let k = import_file(&p, name, &src, keygen).unwrap();
            assert_eq!(
                (k.kind.as_str(), k.fingerprint.as_str(), k.encrypted),
                (kind_, fp.as_str(), encrypted),
                "{name}"
            );
            assert_eq!(k.comment, "me@test");
        }
        // A PEM key: its public half comes from ssh-keygen (without a .pub next to it).
        let pem = d.path().join("pem");
        let fp = made(&pem, &["-t", "rsa", "-b", "2048", "-m", "PEM", "-N", ""]);
        fs::remove_file(pub_path(&pem)).unwrap();
        let k = import_file(&p, "pem", &pem, keygen).unwrap();
        assert_eq!((k.kind.as_str(), k.fingerprint.as_str()), ("RSA 2048", fp.as_str()));
        // …which can't work with a passphrase.
        let locked = d.path().join("pem-locked");
        made(&locked, &["-t", "rsa", "-b", "2048", "-m", "PEM", "-N", "secret phrase"]);
        fs::remove_file(pub_path(&locked)).unwrap();
        let err = import_file(&p, "pem2", &locked, keygen).unwrap_err().to_string();
        assert!(err.contains("ssh-keygen -p"), "{err}");
        assert!(!exists(&p, "pem2"));
        // ssh-keygen itself accepts the saved file (and its permissions).
        let st =
            std::process::Command::new(keygen).args(["-y", "-P", "", "-f"]).arg(dir(&p).join("ed")).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    }

    /// The install script, run here with a fake home: it creates what's missing privately,
    /// adds the key once, and mends a last line without a newline.
    #[cfg(unix)]
    #[test]
    fn install_script_adds_a_key_once() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let public = format!("ssh-ed25519 {} it's me", STANDARD.encode(ed25519_blob(4)));
        let script = install_script(&public).unwrap();
        let run = || {
            let out = std::process::Command::new("/bin/sh").args(["-c", &script]).env("HOME", &home).output().unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        assert!(run().contains("added the key"));
        let auth = home.join(".ssh/authorized_keys");
        let mode = |f: &Path| fs::metadata(f).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&home.join(".ssh")), mode(&auth)), (0o700, 0o600));
        assert!(run().contains("already"));
        assert_eq!(fs::read_to_string(&auth).unwrap(), format!("{public}\n"));

        fs::write(&auth, "ssh-rsa AAAA old").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o640)).unwrap();
        run();
        assert_eq!(fs::read_to_string(&auth).unwrap(), format!("ssh-rsa AAAA old\n{public}\n"));
        assert_eq!(mode(&auth), 0o640, "what was there is left as it is");
    }
}
