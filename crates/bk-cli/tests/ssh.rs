//! End-to-end tests against a real, unprivileged `sshd` on 127.0.0.1 (skipped if missing).
//! The "server" is this machine, logged into as the current user.
#![cfg(unix)]

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command as Cmd;
use predicates::prelude::*;
use predicates::str::contains;

struct Sshd {
    dir: tempfile::TempDir,
    port: u16,
    child: Child,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn keygen(path: &Path) {
    let ok =
        Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(path).status().unwrap().success();
    assert!(ok, "ssh-keygen failed");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// With `fake_home`, sessions get `HOME=<dir>/home`, and keys in its `.ssh/authorized_keys`
/// log in too (so a test can install a key without touching this user's real one).
fn start_sshd(fake_home: bool) -> Option<Sshd> {
    if !Path::new("/usr/sbin/sshd").exists() {
        eprintln!("skipping: no /usr/sbin/sshd");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    keygen(&d.join("hostkey"));
    keygen(&d.join("client"));
    std::fs::copy(d.join("client.pub"), d.join("authorized_keys")).unwrap();
    let (auth, env) = if fake_home {
        std::fs::create_dir_all(d.join("home")).unwrap();
        (
            format!("{0}/authorized_keys {0}/home/.ssh/authorized_keys", d.display()),
            format!(" HOME={}/home", d.display()),
        )
    } else {
        (format!("{}/authorized_keys", d.display()), String::new())
    };
    // Parallel tests can be handed the same free port; if another test's sshd got it, ours
    // exits at once while the port still answers. Check ours is alive, else try another port.
    let mut started = None;
    for _ in 0..5 {
        let port = free_port();
        std::fs::write(
            d.join("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/hostkey\nAuthorizedKeysFile {auth}\n\
                 PidFile {d}/sshd.pid\nStrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
                 PubkeyAuthentication yes\nAllowTcpForwarding yes\nPermitTTY yes\nLogLevel ERROR\n\
                 SetEnv PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin{env}\n",
                d = d.display()
            ),
        )
        .unwrap();
        let Ok(mut child) = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(d.join("sshd_config"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return None;
        };
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(150));
        if TcpStream::connect(("127.0.0.1", port)).is_ok() && matches!(child.try_wait(), Ok(None)) {
            started = Some((port, child));
            break;
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let Some((port, child)) = started else {
        eprintln!("skipping: sshd did not start");
        return None;
    };
    // ssh wrapper: a private known_hosts so tests never touch ~/.ssh/known_hosts.
    let wrapper = d.join("ssh-wrapper");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec /usr/bin/ssh -o UserKnownHostsFile={d}/known_hosts -o StrictHostKeyChecking=accept-new -o LogLevel=ERROR \"$@\"\n",
            d = d.display()
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&wrapper).status().unwrap();
    Some(Sshd { dir, port, child })
}

struct Env {
    sshd: Sshd,
    home: tempfile::TempDir,
}

impl Env {
    fn new() -> Option<Self> {
        Env::start(false)
    }

    fn start(fake_home: bool) -> Option<Self> {
        let sshd = start_sshd(fake_home)?;
        let home = tempfile::tempdir().unwrap();
        let cfg = home.path().join("bookmarks.toml");
        std::fs::write(&cfg, format!("[settings]\nssh = \"{}\"\n", sshd.dir.path().join("ssh-wrapper").display()))
            .unwrap();
        let e = Env { sshd, home };
        let key = e.sshd.dir.path().join("client");
        e.bk()
            .args(["server", "add", "t", "127.0.0.1", "-p", &e.sshd.port.to_string(), "-i"])
            .arg(&key)
            .assert()
            .success();
        Some(e)
    }

    fn bk(&self) -> Cmd {
        let mut c = Cmd::new(env!("CARGO_BIN_EXE_bk"));
        c.env("BK_CONFIG", self.home.path().join("bookmarks.toml"))
            .env("BK_DATA_DIR", self.home.path().join("data"))
            .env("NO_COLOR", "1")
            .timeout(Duration::from_secs(60));
        c
    }

    fn work(&self) -> PathBuf {
        let w = self.home.path().join("work");
        std::fs::create_dir_all(&w).unwrap();
        w.canonicalize().unwrap()
    }
}

#[test]
fn remote_run_cwd_exit_codes_params_and_dotenv() {
    let Some(e) = Env::new() else { return };
    let work = e.work();
    std::fs::write(work.join(".env"), "GREETING=\"hello there\"\nSECRET=s3\n").unwrap();
    let cwd = work.display().to_string();
    e.bk()
        .args(["add", "r", "--server", "t", "--cwd", &cwd, "--dotenv", ".env", "--var", "WHO=bk"])
        .arg("--")
        .args(["sh", "-c", "echo \"$GREETING from $WHO in $PWD ({{n:1}})\""])
        .assert()
        .success();
    e.bk().arg("r").assert().success().stdout(contains(format!("hello there from bk in {cwd} (1)")));
    e.bk().args(["r", "n=42"]).assert().success().stdout(contains("(42)"));
    e.bk().args(["add", "fail", "-s", "t", "exit 7"]).assert().success();
    e.bk().arg("fail").assert().code(7);
    // History records the server.
    let hist = std::fs::read_to_string(e.home.path().join("data/history.jsonl")).unwrap();
    assert!(hist.contains("\"server\":\"t\""), "{hist}");
}

#[test]
fn remote_workflow_and_dry_run() {
    let Some(e) = Env::new() else { return };
    e.bk()
        .args(["add", "wf", "-s", "t", "--step", "echo one", "--step", "false", "--step", "echo never"])
        .assert()
        .success();
    e.bk().arg("wf").assert().code(1).stdout(contains("one").and(contains("never").not()));
    e.bk().args(["run", "wf", "--dry-run"]).assert().success().stderr(contains("(on t)"));
    e.bk().args(["run", "wf", "--print"]).assert().success().stdout(contains("exec bash -lc"));
}

#[test]
fn remote_conda_env_via_discovery() {
    if !Path::new("/opt/miniconda3/envs/test/conda-meta").is_dir() {
        eprintln!("skipping: conda env `test` not present");
        return;
    }
    let Some(e) = Env::new() else { return };
    e.bk().args(["server", "scan", "t"]).assert().success().stderr(contains("conda"));
    e.bk()
        .args(["add", "c", "-s", "t", "--env", "conda:test", "python -c 'import sys; print(sys.prefix)'"])
        .assert()
        .success();
    e.bk().arg("c").assert().success().stdout(contains("/opt/miniconda3/envs/test"));
    e.bk().args(["add", "nope", "-s", "t", "--env", "conda:does-not-exist", "true"]).assert().success();
    e.bk().arg("nope").assert().failure().stderr(contains("not found on t"));
}

#[test]
fn ssh_session_check_and_go() {
    let Some(e) = Env::new() else { return };
    e.bk().args(["ssh", "t", "--", "echo", "session ok"]).assert().success().stdout(contains("session ok"));
    e.bk().args(["servers", "--check"]).assert().success().stdout(contains("reachable"));
    e.bk().args(["add", "g", "-s", "t", "--cwd", "/tmp", "true"]).assert().success();
    e.bk().args(["go", "g", "--print"]).assert().success().stdout(contains("bash -lc").and(contains("127.0.0.1")));
    // A server nobody listens on.
    e.bk().args(["server", "add", "dead", "127.0.0.1", "-p", &free_port().to_string()]).assert().success();
    e.bk().args(["add", "d", "-s", "dead", "true"]).assert().success();
    e.bk().arg("d").assert().code(255).stderr(contains("failed or the connection dropped"));
    e.bk().args(["servers", "--check"]).assert().success().stdout(contains("unreachable"));
}

/// Saved keys: a server logs in with one (`-i key:NAME`), and `bk keys install` puts a new one
/// on a server, which then uses it.
#[test]
fn saved_keys_log_in_and_install() {
    let Some(e) = Env::start(true) else { return };
    let port = e.sshd.port.to_string();
    let client = e.sshd.dir.path().join("client");
    e.bk().args(["keys", "import"]).arg(&client).args(["--name", "client"]).assert().success();
    e.bk().args(["server", "add", "k", "127.0.0.1", "-p", &port, "-i", "key:client"]).assert().success();
    e.bk().args(["ssh", "k", "--", "echo", "with a saved key"]).assert().success().stdout(contains("with a saved key"));
    e.bk().args(["keys"]).assert().success().stdout(contains("client").and(contains("used by k")));

    // Installing writes to the server's ~/.ssh: make sure that's the test's, never this user's.
    let home = e.sshd.dir.path().join("home");
    let out = e.bk().args(["ssh", "t", "--", "sh", "-c", "echo $HOME"]).output().unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() != home.display().to_string() {
        eprintln!("skipping the install: sshd didn't set the test's HOME");
        return;
    }
    e.bk().args(["keys", "new", "fresh", "--no-passphrase"]).assert().success();
    e.bk()
        .args(["keys", "install", "fresh", "t"])
        .assert()
        .success()
        .stdout(contains("added the key"))
        .stderr(contains("t now uses key fresh").and(contains("logs in with the key")));
    let authorized = std::fs::read_to_string(home.join(".ssh/authorized_keys")).unwrap();
    assert_eq!(authorized.lines().count(), 1, "{authorized}");
    let cfg = std::fs::read_to_string(e.home.path().join("bookmarks.toml")).unwrap();
    assert!(cfg.contains("identity = \"key:fresh\""), "{cfg}");
    e.bk().args(["ssh", "t", "--", "echo", "with the new key"]).assert().success().stdout(contains("with the new key"));

    // Without the key on this device, ssh falls back to the agent and default keys (which
    // this server doesn't take), and doctor says why.
    e.bk().args(["keys", "rm", "fresh", "--yes"]).assert().success();
    e.bk().args(["add", "on-t", "-s", "t", "true"]).assert().success();
    e.bk().arg("doctor").assert().stderr(contains("saved key `fresh`, which isn't on this device"));
}

#[test]
fn server_validation() {
    let home = tempfile::tempdir().unwrap();
    let bk = || {
        let mut c = Cmd::new(env!("CARGO_BIN_EXE_bk"));
        c.env("BK_CONFIG", home.path().join("b.toml")).env("BK_DATA_DIR", home.path().join("data"));
        c
    };
    bk().args(["server", "add", "x", "-oProxyCommand=evil"]).assert().failure();
    bk().args(["server", "add", "x", "h", "-o", "ProxyCommand=nc"])
        .assert()
        .failure()
        .stderr(contains("isn't allowed"));
    bk().args(["server", "add", "x", "h", "-L", "0.0.0.0:80:h:80"]).assert().failure();
    bk().args(["add", "y", "--detach", "ls"]).assert().failure().stderr(contains("need --server"));
    bk().args(["server", "add", "x", "deploy@web1", "-p", "2200", "-L", "8080:127.0.0.1:80"]).assert().success();
    let text = std::fs::read_to_string(home.path().join("b.toml")).unwrap();
    assert!(text.contains("[servers.x]") && text.contains("user = \"deploy\""), "{text}");
}

#[test]
fn remote_node_runtime_and_scan() {
    let Some(e) = Env::new() else { return };
    let work = e.work();
    std::fs::write(work.join("package.json"), r#"{"scripts":{"hello":"echo hello from npm"}}"#).unwrap();
    std::fs::write(work.join("Makefile"), "greet:\n\techo hi from make\n").unwrap();
    let dir = work.display().to_string();
    e.bk()
        .args(["scan", "--list", &format!("t:{dir}")])
        .assert()
        .success()
        .stdout(contains("npm run hello").and(contains("make greet")));
    e.bk()
        .args(["scan", "--yes", "--prefix", "w", &format!("t:{dir}")])
        .assert()
        .success()
        .stderr(contains("bookmarked 2 scripts"));
    e.bk().arg("w-greet").assert().success().stdout(contains("hi from make"));
    let cfg = std::fs::read_to_string(e.home.path().join("bookmarks.toml")).unwrap();
    assert!(cfg.contains("server = \"t\""), "{cfg}");
    // Node from the server's nvm, picked by .nvmrc (needs nvm's 22.x on this machine; without
    // it the pin can't be met and runs fail, as they should).
    if std::fs::read_dir(bk_core::paths::home().join(".nvm/versions/node"))
        .map(|rd| rd.flatten().any(|d| d.file_name().to_string_lossy().starts_with("v22")))
        .unwrap_or(false)
    {
        std::fs::write(work.join(".nvmrc"), "22\n").unwrap();
        e.bk().args(["add", "nv", "-s", "t", "--cwd", &dir, "-r", "node=auto", "node --version"]).assert().success();
        e.bk().arg("nv").assert().success().stdout(contains("v22."));
    }
}

#[test]
fn keep_running_in_tmux() {
    use bk_core::remote::{self, RemoteInputs, TmuxState};
    use bk_core::servers::Batch;
    let Some(e) = Env::new() else { return };
    let paths =
        bk_core::Paths { config_file: e.home.path().join("bookmarks.toml"), data_dir: e.home.path().join("data") };
    let state_file = bk_core::paths::home().join(".local/state/bk/bk-tmuxtest.state");
    e.bk()
        .args([
            "add",
            "tmuxtest",
            "-s",
            "t",
            "--detach",
            "--step",
            "echo started",
            "--step",
            "sleep 3; echo finished-job",
        ])
        .assert()
        .success();
    let lib = bk_core::Library::load(&paths, &[]);
    let entry = lib.find("tmuxtest").unwrap().clone();
    let mut env: bk_core::env::EnvMap = bk_core::env::current();
    env.insert("TERM".into(), "xterm-256color".into());
    let inp = RemoteInputs {
        servers: &lib.servers,
        paths: &paths,
        ssh_program: e.sshd.dir.path().join("ssh-wrapper").display().to_string(),
        base_env: &env,
        online: true,
        markers: true,
    };
    let plan = remote::build(&entry, &Default::default(), &inp).unwrap();
    let b: Batch = inp.batch(&plan.target);
    match remote::tmux_state(&b, "tmuxtest").unwrap() {
        TmuxState::NoTmux => {
            eprintln!("skipping: tmux not reachable on the test server");
            return;
        }
        TmuxState::Alive => remote::detached_kill(&b, "tmuxtest"),
        TmuxState::Absent => {}
    }
    let args = remote::detached_start(&plan, &b, &paths).unwrap();
    let mut child = std::process::Command::new(&plan.program)
        .args(&args)
        .env_clear()
        .envs(&plan.env)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(remote::tmux_state(&b, "tmuxtest").unwrap(), TmuxState::Alive);
    // Detach from the side, like the app's Detach button: the job keeps going.
    let _ = b.exec("tmux detach-client -s '=bk-tmuxtest'", b"", Duration::from_secs(10));
    let st = child.wait().unwrap();
    assert!(st.success(), "detach should end the client cleanly");
    assert_eq!(remote::tmux_state(&b, "tmuxtest").unwrap(), TmuxState::Alive, "job survives the disconnect");
    let progress = remote::detached_progress(&b, "tmuxtest").unwrap();
    assert!(progress.iter().any(|l| l == "step;1"), "{progress:?}");
    // Let it finish; the pane then waits for Enter, so the session is still there.
    std::thread::sleep(Duration::from_secs(4));
    let progress = remote::detached_progress(&b, "tmuxtest").unwrap();
    assert!(progress.iter().any(|l| l == "done;0"), "{progress:?}");
    remote::detached_kill(&b, "tmuxtest");
    assert_eq!(remote::tmux_state(&b, "tmuxtest").unwrap(), TmuxState::Absent);
    let _ = std::fs::remove_file(&state_file);
    for ext in ["sh", "log"] {
        let _ = std::fs::remove_file(bk_core::paths::home().join(format!(".local/state/bk/bk-tmuxtest.{ext}")));
    }
}
