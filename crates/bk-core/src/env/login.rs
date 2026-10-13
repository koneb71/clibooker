//! Capture the user's login-shell environment.
//!
//! GUI apps launched from Finder/Dock get a minimal `PATH` (no Homebrew, `~/.local/bin`,
//! `condabin`, ...). The desktop app runs the user's shell once as an interactive login
//! shell and uses that environment as its base.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::{EnvMap, parse_env0};

/// The user's login shell: `$SHELL`, else `/bin/zsh`.
pub fn user_shell() -> PathBuf {
    std::env::var_os("SHELL").filter(|s| !s.is_empty()).map(PathBuf::from).unwrap_or_else(|| "/bin/zsh".into())
}

/// Run `shell -l -i` and capture its environment. The env is written to a temp file
/// rather than stdout so rc-file noise or background jobs holding the pipe can't interfere.
pub fn login_env(shell: &Path, timeout: Duration) -> Result<EnvMap> {
    let out = tempfile::NamedTempFile::new()?;
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", r#"/usr/bin/env -0 > "$1""#, "bk-login"])
        .arg(out.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", shell.display()))?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} took longer than {:?} to start", shell.display(), timeout);
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    let bytes = std::fs::read(out.path())?;
    if bytes.is_empty() {
        bail!("{} exited with {status} without printing its environment", shell.display());
    }
    let mut env = parse_env0(&bytes);
    for k in ["_", "PWD", "OLDPWD", "SHLVL"] {
        env.remove(std::ffi::OsStr::new(k));
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_login_shell() {
        let shell = Path::new("/bin/zsh");
        if !shell.exists() {
            return;
        }
        let env = login_env(shell, Duration::from_secs(10)).unwrap();
        assert!(super::super::get(&env, "PATH").is_some());
        assert!(super::super::get(&env, "HOME").is_some());
        assert!(super::super::get(&env, "SHLVL").is_none());
    }
}
