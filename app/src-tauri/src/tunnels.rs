//! Port forwards for remote runs and sessions: one `ssh -N -L …` child per forward, kept
//! separate from the run's connection so a busy port or a failed forward never breaks the
//! run itself, and closed when the run or session ends.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bk_core::env::EnvMap;
use bk_core::paths::Paths;
use bk_core::servers::{self, Purpose, Target};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ForwardInfo {
    pub spec: String,
    pub local_port: u16,
    /// `host:port` on the server side.
    pub remote: String,
    pub error: Option<String>,
}

pub struct Tunnels {
    /// Each running tunnel with the tail of its ssh error output.
    children: Vec<(Child, Arc<Mutex<String>>)>,
    pub info: Vec<ForwardInfo>,
}

impl Tunnels {
    pub fn none() -> Self {
        Tunnels { children: Vec::new(), info: Vec::new() }
    }

    /// Whether each tunnel's ssh is still up, and what it last said (for diagnostics).
    #[cfg(all(test, unix))]
    pub fn describe(&mut self) -> String {
        self.children
            .iter_mut()
            .map(|(c, log)| {
                let state = match c.try_wait() {
                    Ok(None) => "running".to_string(),
                    Ok(Some(st)) => format!("exited ({st})"),
                    Err(e) => format!("unknown ({e})"),
                };
                format!("tunnel {state}: {}", log.lock().map(|l| l.trim().to_string()).unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn open(program: &str, target: &Target, forwards: &[String], paths: &Paths, env: &EnvMap) -> Self {
        let mut t = Tunnels::none();
        for f in forwards {
            let local_port = servers::forward_local_port(f).unwrap_or(0);
            let remote = servers::forward_remote(f).map(|(h, p)| format!("{h}:{p}")).unwrap_or_default();
            let mut info = ForwardInfo { spec: f.clone(), local_port, remote, error: None };
            if TcpListener::bind(("127.0.0.1", local_port)).is_err() {
                info.error = Some(format!("localhost:{local_port} is already in use"));
                t.info.push(info);
                continue;
            }
            let spawned = bk_core::proc::background(program)
                .arg("-L")
                .arg(f)
                .args(servers::ssh_args(target, Purpose::Tunnel, paths))
                .env_clear()
                .envs(env)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn();
            match spawned {
                Ok(mut child) => {
                    let log = Arc::new(Mutex::new(String::new()));
                    match wait_for_failure(&mut child, Duration::from_millis(1500), &log) {
                        Some(err) => info.error = Some(err),
                        None => t.children.push((child, log)),
                    }
                }
                Err(e) => info.error = Some(format!("couldn't start ssh: {e}")),
            }
            t.info.push(info);
        }
        t
    }

    pub fn close(&mut self) {
        for (c, _) in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.children.clear();
    }
}

impl Drop for Tunnels {
    fn drop(&mut self) {
        self.close();
    }
}

/// `ExitOnForwardFailure` makes ssh exit quickly when the forward can't be set up.
fn wait_for_failure(child: &mut Child, within: Duration, log: &Arc<Mutex<String>>) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < within {
        if let Ok(Some(_)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            let line = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("tunnel closed");
            return Some(if line.contains("Permission denied") {
                "the tunnel needs key authentication (ssh-add your key)".into()
            } else {
                line.to_string()
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Still running: the forward is up. Keep draining stderr (e.g. "connect failed" before
    // the remote service starts) so the pipe never fills or breaks, keeping its tail.
    if let Some(mut e) = child.stderr.take() {
        let log = log.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = e.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut l) = log.lock() {
                    l.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if l.len() > 4096 {
                        let cut = l.len() - 2048;
                        let cut = (cut..l.len()).find(|&i| l.is_char_boundary(i)).unwrap_or(l.len());
                        l.drain(..cut);
                    }
                }
            }
        });
    }
    None
}
