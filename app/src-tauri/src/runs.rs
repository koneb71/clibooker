//! Runs bookmarks and SSH sessions inside pseudo-terminals and streams their output to the UI.
//!
//! A local run executes under one POSIX `sh` wrapper (`bk_core::script`) that is the PTY's
//! session leader. On macOS the kernel revokes a terminal when its session leader exits,
//! so steps can't be separate session leaders on a shared PTY. A remote run is `ssh -tt`
//! running the same wrapper on the server. Either way the wrapper prints step headers and
//! reports progress with in-band markers (see `markers.rs`) that the reader strips.
//!
//! Windows has no POSIX `sh`, and ConPTY drops unknown escape sequences, so there a local run
//! is the bundled `bk.exe __run` (the CLI's own step runner), which writes its progress to a
//! file we follow. Its process tree lives in a Job Object so Stop can end all of it.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bk_core::env::EnvMap;
use bk_core::history::{self, RunRecord};
use bk_core::paths::{self, Paths};
use bk_core::plan::RunPlan;
use bk_core::remote::RemotePlan;
use bk_core::render::q;
use bk_core::script::{WrapperOptions, WrapperStep, wrapper};
use bk_core::servers::{Batch, Target};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::markers::MarkerParser;
use crate::tunnels::{ForwardInfo, Tunnels};

const BUFFER_MAX: usize = 2 * 1024 * 1024;
const BUFFER_KEEP: usize = 1536 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Succeeded,
    Failed,
    Stopped,
    /// The SSH connection dropped before the remote command finished.
    Disconnected,
    /// Still running in tmux on the server (you detached, or the connection dropped).
    Detached,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bookmark,
    Session,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub entry_id: String,
    pub status: Status,
    pub exit_code: Option<i32>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub step: usize,
    pub total_steps: usize,
    pub step_label: String,
    pub cwd: String,
    pub env_label: String,
    pub params: BTreeMap<String, String>,
    pub background: bool,
    /// Server name for remote runs and sessions.
    pub server: Option<String>,
    pub forwards: Vec<ForwardInfo>,
    /// tmux session on the server for runs that keep running after disconnect.
    pub tmux: Option<String>,
    /// Attached to a run that was already going (not started now).
    pub reattached: bool,
}

struct Output {
    buf: Vec<u8>,
    subs: Vec<(String, Channel<InvokeResponseBody>)>,
}

impl Output {
    fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() > BUFFER_MAX {
            // Trim at a line boundary to avoid cutting escape sequences in half.
            let cut = self.buf.len() - BUFFER_KEEP;
            let cut = self.buf[cut..].iter().position(|b| *b == b'\n').map(|p| cut + p + 1).unwrap_or(cut);
            self.buf.drain(..cut);
        }
        self.subs.retain(|(_, ch)| ch.send(InvokeResponseBody::Raw(data.to_vec())).is_ok());
    }
}

/// How to reach the server again (for stopping remote runs from a side connection).
#[derive(Clone)]
pub struct RemoteCtl {
    pub program: String,
    pub target: Target,
    pub paths: Paths,
    pub env: EnvMap,
}

impl RemoteCtl {
    pub fn batch(&self) -> Batch<'_> {
        Batch { program: self.program.clone(), target: &self.target, paths: &self.paths, env: self.env.clone() }
    }

    fn kill_group(&self, pgid: i64, signal: &str) {
        let _ = self.batch().exec(&format!("kill -{signal} -- -{pgid} 2>/dev/null; true"), b"", Duration::from_secs(8));
    }
}

pub struct Run {
    info: Mutex<RunInfo>,
    output: Mutex<Output>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    stop_requested: AtomicBool,
    remote: Option<RemoteCtl>,
    /// Process group of the remote wrapper (from the `pgid` marker), 0 if unknown.
    remote_pgid: AtomicI64,
    /// Final code reported by the wrapper's `done` marker.
    done_code: Mutex<Option<i32>>,
    tunnels: Mutex<Tunnels>,
    /// Bookmark name of a tmux-backed (keep running) run.
    tmux_bookmark: Option<String>,
    /// Windows: the job holding the run's whole process tree.
    #[cfg(windows)]
    job: Option<crate::job::Job>,
}

impl Run {
    pub fn info(&self) -> RunInfo {
        self.info.lock().expect("info lock").clone()
    }

    fn is_running(&self) -> bool {
        self.info.lock().expect("info lock").status == Status::Running
    }

    fn kind(&self) -> Kind {
        self.info.lock().expect("info lock").kind
    }

    fn update<R: Runtime>(&self, app: &AppHandle<R>, f: impl FnOnce(&mut RunInfo)) {
        let info = {
            let mut i = self.info.lock().expect("info lock");
            f(&mut i);
            i.clone()
        };
        let _ = app.emit("run-updated", info);
    }

    #[cfg(unix)]
    fn signal_foreground(&self, sig: i32) {
        let pgid = self.master.lock().expect("master lock").as_ref().and_then(|m| m.process_group_leader());
        if let Some(pg) = pgid.filter(|p| *p > 0) {
            unsafe {
                libc::killpg(pg, sig);
            }
        }
    }

    /// Ctrl+C for whatever runs in the terminal.
    fn interrupt(&self) {
        #[cfg(unix)]
        self.signal_foreground(libc::SIGINT);
        // ConPTY turns ^C into a Ctrl+C event for every process on the console.
        #[cfg(windows)]
        self.write_input(b"\x03");
    }

    /// The firmer second step: SIGTERM, or on Windows ending the whole process tree.
    fn terminate(&self) {
        #[cfg(unix)]
        self.signal_foreground(libc::SIGTERM);
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }

    /// The app is quitting.
    fn hang_up(&self) {
        #[cfg(unix)]
        self.signal_foreground(libc::SIGHUP);
        #[cfg(windows)]
        self.terminate();
    }

    fn kill_local(&self) {
        #[cfg(windows)]
        self.terminate();
        if let Some(k) = self.killer.lock().expect("killer lock").as_mut() {
            let _ = k.kill();
        }
    }

    fn write_input(&self, data: &[u8]) {
        if let Some(w) = self.writer.lock().expect("writer lock").as_mut() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
    }
}

pub struct StartOptions {
    pub cols: u16,
    pub rows: u16,
    pub background: bool,
    pub notify: bool,
    pub record_history: bool,
    pub history_file: PathBuf,
}

/// What to start in a PTY.
pub struct Spawn {
    pub kind: Kind,
    pub name: String,
    pub entry_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: EnvMap,
    pub step_labels: Vec<String>,
    pub cwd_label: String,
    pub env_label: String,
    pub params: BTreeMap<String, String>,
    pub server: Option<String>,
    pub remote: Option<RemoteCtl>,
    /// Forwards to open as separate tunnels while it runs.
    pub forwards: Vec<String>,
    /// History template, filled in when the run ends.
    pub record: Option<RunRecord>,
    /// Keep-running (tmux) run of this bookmark, and whether we reattached to it.
    pub tmux: Option<(String, bool)>,
    /// Windows local runs: the steps file to write for `bk __run`, and its progress file.
    pub local_steps: Option<LocalSteps>,
}

pub struct LocalSteps {
    pub steps_file: PathBuf,
    pub steps_json: String,
    pub progress_file: PathBuf,
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn term_env(env: &mut EnvMap) {
    bk_core::env::set(env, "TERM", "xterm-256color");
    bk_core::env::set(env, "COLORTERM", "truecolor");
    bk_core::env::set(env, "TERM_PROGRAM", "Clibooker");
    if bk_core::env::get(env, "LANG").is_none() {
        bk_core::env::set(env, "LANG", "en_US.UTF-8");
    }
}

impl Spawn {
    /// A local bookmark run: the shared wrapper under `/bin/sh`, or `bk.exe __run` on Windows.
    pub fn local(plan: &RunPlan) -> Spawn {
        if cfg!(windows) {
            return Spawn::local_runner(plan);
        }
        let steps: Vec<WrapperStep> = plan
            .steps
            .iter()
            .map(|s| WrapperStep {
                label: s.label.clone(),
                run: plan.argv(s).iter().map(|a| q(&a.to_string_lossy())).collect::<Vec<_>>().join(" "),
                cd: Some(q(&s.cwd.display().to_string())),
                display: s.command.clone(),
                continue_on_error: s.continue_on_error,
            })
            .collect();
        let title = plan.title();
        let script = wrapper(&steps, &WrapperOptions { markers: true, title, ..Default::default() });
        let mut env = plan.env.clone();
        term_env(&mut env);
        Spawn {
            kind: Kind::Bookmark,
            name: plan.name.clone(),
            entry_id: plan.entry_id.clone(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
            cwd: plan.cwd.clone(),
            env,
            step_labels: plan.steps.iter().map(|s| s.label.clone()).collect(),
            cwd_label: paths::contract_tilde(&plan.cwd),
            env_label: plan.env_label.clone(),
            params: plan.params.clone(),
            server: None,
            remote: None,
            forwards: Vec::new(),
            record: Some(RunRecord::from_plan(plan, "app", 0)),
            tmux: None,
            local_steps: None,
        }
    }

    /// A local run through the bundled CLI's step runner (Windows). The plan's env becomes the
    /// runner's own environment, so `.env` values never touch disk; the steps file holds only
    /// the shell, the steps and the title.
    fn local_runner(plan: &RunPlan) -> Spawn {
        let base = std::env::temp_dir().join(format!(
            "clibooker-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let steps_file = base.with_extension("json");
        let progress_file = base.with_extension("progress");
        let steps = plan.steps_file(Some(progress_file.clone()));
        // The bundled CLI sits next to the app; CLIBOOKER_BK points elsewhere (tests, dev).
        let runner = std::env::var_os("CLIBOOKER_BK").map(PathBuf::from).unwrap_or_else(|| {
            std::env::current_exe()
                .map(|e| e.with_file_name(if cfg!(windows) { "bk.exe" } else { "bk" }))
                .unwrap_or_else(|_| PathBuf::from("bk"))
        });
        let mut env = plan.env.clone();
        term_env(&mut env);
        Spawn {
            kind: Kind::Bookmark,
            name: plan.name.clone(),
            entry_id: plan.entry_id.clone(),
            program: runner.display().to_string(),
            args: vec!["__run".into(), steps_file.display().to_string()],
            cwd: plan.cwd.clone(),
            env,
            step_labels: plan.steps.iter().map(|s| s.label.clone()).collect(),
            cwd_label: paths::contract_tilde(&plan.cwd),
            env_label: plan.env_label.clone(),
            params: plan.params.clone(),
            server: None,
            remote: None,
            forwards: Vec::new(),
            record: Some(RunRecord::from_plan(plan, "app", 0)),
            tmux: None,
            local_steps: Some(LocalSteps {
                steps_file,
                steps_json: serde_json::to_string(&steps).unwrap_or_default(),
                progress_file,
            }),
        }
    }

    /// A remote bookmark run: `ssh -tt … 'exec bash -lc …'`.
    pub fn remote(plan: &RemotePlan, paths: &Paths) -> Spawn {
        let mut env = plan.env.clone();
        term_env(&mut env);
        Spawn {
            kind: Kind::Bookmark,
            name: plan.name.clone(),
            entry_id: plan.entry_id.clone(),
            program: plan.program.clone(),
            args: plan.args.clone(),
            cwd: paths::home(),
            env: env.clone(),
            step_labels: plan.steps.iter().map(|s| s.label.clone()).collect(),
            cwd_label: format!("{}:{}", plan.target.name, plan.cwd),
            env_label: plan.env_label.clone(),
            params: plan.params.clone(),
            server: Some(plan.target.name.clone()),
            remote: Some(RemoteCtl {
                program: plan.program.clone(),
                target: plan.target.clone(),
                paths: paths.clone(),
                env,
            }),
            forwards: plan.forwards.clone(),
            record: Some(RunRecord::from_remote(plan, "app", 0)),
            tmux: None,
            local_steps: None,
        }
    }

    /// An interactive SSH session (Connect, Open shell here).
    pub fn session(
        name: &str,
        program: &str,
        args: Vec<String>,
        target: &Target,
        env: EnvMap,
        forwards: Vec<String>,
    ) -> Spawn {
        let mut env = env;
        term_env(&mut env);
        Spawn {
            kind: Kind::Session,
            name: name.to_string(),
            entry_id: String::new(),
            program: program.to_string(),
            args,
            cwd: paths::home(),
            env,
            step_labels: Vec::new(),
            cwd_label: target.destination.clone(),
            env_label: format!("ssh {}", target.name),
            params: BTreeMap::new(),
            server: Some(target.name.clone()),
            remote: None,
            forwards,
            record: None,
            tmux: None,
            local_steps: None,
        }
    }
}

#[derive(Default)]
pub struct RunManager {
    runs: Mutex<HashMap<String, Arc<Run>>>,
    order: Mutex<Vec<String>>,
    counter: AtomicU64,
}

impl RunManager {
    pub fn list(&self) -> Vec<RunInfo> {
        let runs = self.runs.lock().expect("runs lock");
        self.order.lock().expect("order lock").iter().filter_map(|id| runs.get(id)).map(|r| r.info()).collect()
    }

    fn get(&self, id: &str) -> Result<Arc<Run>> {
        self.runs.lock().expect("runs lock").get(id).cloned().with_context(|| format!("no run `{id}`"))
    }

    pub fn any_running(&self) -> bool {
        self.runs.lock().expect("runs lock").values().any(|r| r.is_running())
    }

    pub fn start<R: Runtime>(
        &self,
        app: AppHandle<R>,
        spawn: Spawn,
        opts: StartOptions,
        tunnel_ctl: Option<&RemoteCtl>,
    ) -> Result<RunInfo> {
        // Tunnels first, so a busy port is reported in the run's header.
        let tunnels = match tunnel_ctl {
            Some(ctl) if !spawn.forwards.is_empty() => {
                Tunnels::open(&ctl.program, &ctl.target, &spawn.forwards, &ctl.paths, &ctl.env)
            }
            _ => Tunnels::none(),
        };

        if let Some(ls) = &spawn.local_steps {
            std::fs::write(&ls.steps_file, &ls.steps_json).context("writing the steps file")?;
            let _ = std::fs::remove_file(&ls.progress_file);
        }
        let pair = native_pty_system()
            .openpty(PtySize { rows: opts.rows.max(5), cols: opts.cols.max(20), pixel_width: 0, pixel_height: 0 })
            .context("opening a pseudo-terminal")?;
        let mut cmd = CommandBuilder::new(&spawn.program);
        cmd.args(&spawn.args);
        cmd.cwd(&spawn.cwd);
        cmd.env_clear();
        for (k, v) in &spawn.env {
            cmd.env(k, v);
        }
        let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("starting {}", spawn.program))?;
        // Only the child should hold the slave side, so we see EOF when it's done.
        drop(pair.slave);
        #[cfg(windows)]
        let job = child.process_id().and_then(|pid| match crate::job::Job::for_process(pid) {
            Ok(j) => Some(j),
            Err(e) => {
                eprintln!("run won't be able to stop child processes: {e:#}");
                None
            }
        });
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("run-{n}");
        let info = RunInfo {
            id: id.clone(),
            kind: spawn.kind,
            name: spawn.name.clone(),
            entry_id: spawn.entry_id.clone(),
            status: Status::Running,
            exit_code: None,
            started_ms: history::now_ms(),
            ended_ms: None,
            step: 0,
            total_steps: spawn.step_labels.len(),
            step_label: String::new(),
            cwd: spawn.cwd_label.clone(),
            env_label: spawn.env_label.clone(),
            params: spawn.params.clone(),
            background: opts.background,
            server: spawn.server.clone(),
            forwards: tunnels.info.clone(),
            tmux: spawn.tmux.as_ref().map(|(b, _)| bk_core::remote::tmux_name(b)),
            reattached: spawn.tmux.as_ref().is_some_and(|(_, existing)| *existing),
        };
        let run = Arc::new(Run {
            info: Mutex::new(info.clone()),
            output: Mutex::new(Output { buf: Vec::new(), subs: Vec::new() }),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(Some(killer)),
            stop_requested: AtomicBool::new(false),
            remote: spawn.remote.clone(),
            remote_pgid: AtomicI64::new(0),
            done_code: Mutex::new(None),
            tunnels: Mutex::new(tunnels),
            tmux_bookmark: spawn.tmux.as_ref().map(|(b, _)| b.clone()),
            #[cfg(windows)]
            job,
        });
        if let Some((_, true)) = &spawn.tmux {
            let line = "\x1b[2m↺ reattached to the run still going on the server (tmux)\x1b[0m\r\n";
            run.output.lock().expect("output lock").push(line.as_bytes());
        }
        // Tunnel problems go at the top of the output.
        for f in &info.forwards {
            let line = match &f.error {
                Some(e) => format!("\x1b[33m⚠ forward localhost:{} → {}: {e}\x1b[0m\r\n", f.local_port, f.remote),
                None => format!("\x1b[2m↔ localhost:{} → {} (open while this runs)\x1b[0m\r\n", f.local_port, f.remote),
            };
            run.output.lock().expect("output lock").push(line.as_bytes());
        }
        self.runs.lock().expect("runs lock").insert(id.clone(), run.clone());
        self.order.lock().expect("order lock").push(id.clone());

        let (eof_tx, eof_rx) = mpsc::channel::<()>();
        let reader_run = run.clone();
        let reader_app = app.clone();
        let labels = spawn.step_labels.clone();
        std::thread::Builder::new().name(format!("{id}-reader")).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut parser = MarkerParser::default();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let (clean, markers) = parser.feed(&buf[..n]);
                        // Under ConPTY, programs may ask where the cursor is (ESC[6n) and wait
                        // for the terminal's answer. xterm.js answers while a view is attached;
                        // for a run nobody is watching (e.g. started in the background), answer
                        // for it so it doesn't hang.
                        #[cfg(windows)]
                        if clean.windows(4).any(|w| w == b"\x1b[6n")
                            && reader_run.output.lock().expect("output lock").subs.is_empty()
                        {
                            reader_run.write_input(b"\x1b[1;1R");
                        }
                        reader_run.output.lock().expect("output lock").push(&clean);
                        for m in markers {
                            handle_marker(&reader_run, &reader_app, &labels, &m);
                        }
                    }
                }
            }
            let rest = parser.flush();
            reader_run.output.lock().expect("output lock").push(&rest);
            let _ = eof_tx.send(());
        })?;

        // tmux swallows the markers: follow the run's state file instead. Without ssh's
        // connection sharing (Windows) each poll is a full login, so poll less often there.
        if let (Some(bookmark), Some(ctl)) = (run.tmux_bookmark.clone(), run.remote.clone()) {
            let poll_run = run.clone();
            let poll_app = app.clone();
            let labels = spawn.step_labels.clone();
            let every = Duration::from_secs(if bk_core::servers::MULTIPLEX { 2 } else { 5 });
            std::thread::Builder::new().name(format!("{id}-progress")).spawn(move || {
                std::thread::sleep(Duration::from_millis(800));
                while poll_run.is_running() {
                    if let Ok(lines) = bk_core::remote::detached_progress(&ctl.batch(), &bookmark) {
                        for m in lines {
                            handle_marker(&poll_run, &poll_app, &labels, &m);
                        }
                    }
                    std::thread::sleep(every);
                }
            })?;
        }

        // `bk __run` (Windows) writes progress lines to a file: follow its new lines.
        let mut temp_files = Vec::new();
        if let Some(ls) = &spawn.local_steps {
            temp_files.extend([ls.steps_file.clone(), ls.progress_file.clone()]);
            let poll_run = run.clone();
            let poll_app = app.clone();
            let labels = spawn.step_labels.clone();
            let file = ls.progress_file.clone();
            std::thread::Builder::new().name(format!("{id}-progress")).spawn(move || {
                let mut seen = 0usize;
                loop {
                    let running = poll_run.is_running();
                    if let Ok(text) = std::fs::read_to_string(&file)
                        && text.len() > seen
                    {
                        // Only whole lines; a partial last line is read next time.
                        let upto = text[seen..].rfind('\n').map(|i| seen + i + 1).unwrap_or(seen);
                        for line in text[seen..upto].lines() {
                            handle_marker(&poll_run, &poll_app, &labels, line.trim());
                        }
                        seen = upto;
                    }
                    if !running {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            })?;
        }

        let waiter_app = app.clone();
        let waiter_run = run.clone();
        let record = spawn.record;
        let final_progress = spawn.local_steps.as_ref().map(|ls| (ls.progress_file.clone(), spawn.step_labels.clone()));
        std::thread::Builder::new().name(format!("{id}-waiter")).spawn(move || {
            let started = Instant::now();
            let status = child.wait();
            // ConPTY keeps its output open until the pseudo console is closed, even after the
            // process exits; closing it can block while output drains, so do it on the side.
            #[cfg(windows)]
            {
                let master = waiter_run.master.lock().expect("master lock").take();
                let writer = waiter_run.writer.lock().expect("writer lock").take();
                std::thread::spawn(move || drop((writer, master)));
            }
            // Let the reader drain; don't wait forever if a background job keeps the tty open.
            let _ = eof_rx.recv_timeout(Duration::from_millis(1500));
            // A quick run can end between two polls of its progress file: read it once more.
            if let Some((file, labels)) = &final_progress
                && let Ok(text) = std::fs::read_to_string(file)
            {
                for line in text.lines() {
                    handle_marker(&waiter_run, &waiter_app, labels, line.trim());
                }
            }
            finish(waiter_app, waiter_run, record, status, started.elapsed(), opts);
            for f in temp_files {
                let _ = std::fs::remove_file(f);
            }
        })?;

        let _ = app.emit("run-updated", info.clone());
        Ok(info)
    }

    /// Replay buffered output, then stream new output to `channel` (replacing any channel with the same key).
    pub fn attach(&self, id: &str, key: &str, channel: Channel<InvokeResponseBody>) -> Result<()> {
        let run = self.get(id)?;
        let mut out = run.output.lock().expect("output lock");
        if !out.buf.is_empty() {
            channel.send(InvokeResponseBody::Raw(out.buf.clone()))?;
        }
        out.subs.retain(|(k, _)| k != key);
        out.subs.push((key.to_string(), channel));
        Ok(())
    }

    pub fn detach(&self, id: &str, key: &str) {
        if let Ok(run) = self.get(id) {
            run.output.lock().expect("output lock").subs.retain(|(k, _)| k != key);
        }
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        let run = self.get(id)?;
        let mut w = run.writer.lock().expect("writer lock");
        if let Some(w) = w.as_mut() {
            w.write_all(data)?;
            w.flush()?;
        }
        Ok(())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        let run = self.get(id)?;
        if let Some(m) = run.master.lock().expect("master lock").as_ref() {
            m.resize(PtySize { rows: rows.max(2), cols: cols.max(2), pixel_width: 0, pixel_height: 0 })?;
        }
        Ok(())
    }

    /// Local runs: SIGINT the foreground job, then SIGTERM, then kill.
    /// Remote runs: ^C through ssh, then kill the remote process group over a side
    /// connection (TERM, then KILL), then drop the connection.
    /// Sessions: just close the connection.
    pub fn stop(&self, id: &str) -> Result<()> {
        let run = self.get(id)?;
        if !run.is_running() {
            return Ok(());
        }
        run.stop_requested.store(true, Ordering::SeqCst);
        if run.kind() == Kind::Session {
            run.kill_local();
            return Ok(());
        }
        if let (Some(bookmark), Some(ctl)) = (run.tmux_bookmark.clone(), run.remote.clone()) {
            // ^C goes to the job inside tmux; if that isn't enough, end the tmux session.
            run.write_input(b"\x03");
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if run.is_running() {
                    bk_core::remote::detached_kill(&ctl.batch(), &bookmark);
                    std::thread::sleep(Duration::from_secs(2));
                }
                if run.is_running() {
                    run.kill_local();
                }
            });
            return Ok(());
        }
        if let Some(ctl) = run.remote.clone() {
            run.write_input(b"\x03");
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                let pgid = run.remote_pgid.load(Ordering::SeqCst);
                if run.is_running() && pgid > 0 {
                    ctl.kill_group(pgid, "TERM");
                    std::thread::sleep(Duration::from_secs(2));
                    if run.is_running() {
                        ctl.kill_group(pgid, "KILL");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                if run.is_running() {
                    run.kill_local();
                }
            });
            return Ok(());
        }
        run.interrupt();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            if run.is_running() {
                run.terminate();
                std::thread::sleep(Duration::from_secs(2));
                if run.is_running() {
                    run.kill_local();
                }
            }
        });
        Ok(())
    }

    /// Leave a keep-running run going on the server and close our connection to it.
    pub fn detach_tmux(&self, id: &str) -> Result<()> {
        let run = self.get(id)?;
        let (Some(bookmark), Some(ctl)) = (run.tmux_bookmark.clone(), run.remote.clone()) else {
            anyhow::bail!("this run isn't kept running on a server");
        };
        let s = bk_core::remote::tmux_name(&bookmark);
        std::thread::spawn(move || {
            let cmd = format!("tmux detach-client -s {} 2>/dev/null; true", q(&format!("={s}")));
            let _ = ctl.batch().script(&cmd, Duration::from_secs(10));
        });
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let run = self.get(id)?;
        if run.is_running() {
            anyhow::bail!("run is still going; stop it first");
        }
        self.runs.lock().expect("runs lock").remove(id);
        self.order.lock().expect("order lock").retain(|r| r != id);
        Ok(())
    }

    pub fn stop_all(&self) {
        let runs: Vec<Arc<Run>> = self.runs.lock().expect("runs lock").values().cloned().collect();
        for run in runs.into_iter().filter(|r| r.is_running()) {
            run.stop_requested.store(true, Ordering::SeqCst);
            if run.remote.is_none() {
                run.hang_up();
            }
            run.kill_local();
            run.tunnels.lock().expect("tunnels lock").close();
        }
    }
}

fn handle_marker<R: Runtime>(run: &Run, app: &AppHandle<R>, labels: &[String], m: &str) {
    let mut parts = m.split(';');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("step"), Some(n), _) => {
            if let Ok(step) = n.parse::<usize>() {
                let label = labels.get(step.wrapping_sub(1)).cloned().unwrap_or_default();
                run.update(app, |i| {
                    i.step = step;
                    i.step_label = label;
                });
            }
        }
        (Some("pgid"), Some(n), _) => {
            if let Ok(pg) = n.trim().parse::<i64>() {
                run.remote_pgid.store(pg, Ordering::SeqCst);
            }
        }
        (Some("done"), Some(c), _) => {
            if let Ok(code) = c.trim().parse() {
                *run.done_code.lock().expect("done lock") = Some(code);
            }
        }
        _ => {}
    }
}

fn exit_code(st: &portable_pty::ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(name) = st.signal() {
        return 128 + signal_number(name);
    }
    // Windows crash codes (0xC0000005, ...) would otherwise read as negative or huge.
    bk_core::proc::normalize_code(st.exit_code() as i32)
}

/// portable-pty reports signals by name (`strsignal`), e.g. "Interrupt: 2" on macOS.
#[cfg(unix)]
fn signal_number(name: &str) -> i32 {
    if let Some((_, n)) = name.rsplit_once(':')
        && let Ok(n) = n.trim().parse()
    {
        return n;
    }
    let lower = name.to_ascii_lowercase();
    match () {
        _ if lower.starts_with("interrupt") => libc::SIGINT,
        _ if lower.starts_with("terminated") => libc::SIGTERM,
        _ if lower.starts_with("killed") => libc::SIGKILL,
        _ if lower.starts_with("hangup") => libc::SIGHUP,
        _ if lower.starts_with("quit") => libc::SIGQUIT,
        _ => 1,
    }
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 { format!("{s:.1}s") } else { format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60) }
}

/// Final status from how the process ended and what the wrapper reported.
fn final_status(kind: Kind, remote: bool, stopped: bool, done: Option<i32>, exit: i32) -> (Status, i32) {
    let code = done.unwrap_or(exit);
    let state = if stopped || code == 130 {
        Status::Stopped
    } else if exit == 255 && done.is_none() && (remote || kind == Kind::Session) {
        Status::Disconnected
    } else if code == 0 {
        Status::Succeeded
    } else {
        Status::Failed
    };
    (state, code)
}

fn finish<R: Runtime>(
    app: AppHandle<R>,
    run: Arc<Run>,
    record: Option<RunRecord>,
    status: std::io::Result<portable_pty::ExitStatus>,
    elapsed: Duration,
    opts: StartOptions,
) {
    let exit = status.as_ref().map(exit_code).unwrap_or(1);
    // A keep-running run: read its state file once more to learn whether the job finished.
    if let (Some(bookmark), Some(ctl)) = (&run.tmux_bookmark, &run.remote)
        && let Ok(lines) = bk_core::remote::detached_progress(&ctl.batch(), bookmark)
    {
        for m in lines {
            if let Some(c) = m.strip_prefix("done;").and_then(|c| c.trim().parse().ok()) {
                *run.done_code.lock().expect("done lock") = Some(c);
            }
        }
    }
    let done = *run.done_code.lock().expect("done lock");
    let kind = run.kind();
    let stopped = run.stop_requested.load(Ordering::SeqCst);
    let (mut state, mut code) = final_status(kind, run.remote.is_some(), stopped, done, exit);
    if run.tmux_bookmark.is_some() && done.is_none() && !stopped {
        state = Status::Detached;
        code = 0;
    }
    let server = run.info().server.unwrap_or_default();
    let took = fmt_duration(elapsed);
    let footer = match (kind, state) {
        (Kind::Session, Status::Disconnected) => format!("\r\n\x1b[33m⚠ connection to {server} lost\x1b[0m\r\n"),
        (Kind::Session, _) => format!("\r\n\x1b[2m■ session closed after {took}\x1b[0m\r\n"),
        (_, Status::Succeeded) => format!("\r\n\x1b[32m✓ done in {took}\x1b[0m\r\n"),
        (_, Status::Failed) => format!("\r\n\x1b[31m✗ exit {code} after {took}\x1b[0m\r\n"),
        (_, Status::Stopped) => format!("\r\n\x1b[33m■ stopped after {took}\x1b[0m\r\n"),
        (_, Status::Detached) => {
            format!("\r\n\x1b[36m⏸ still running on {server} (tmux). Run it again to reattach.\x1b[0m\r\n")
        }
        (_, Status::Disconnected) => {
            format!(
                "\r\n\x1b[33m⚠ connection to {server} lost after {took}; the command may still be running there\x1b[0m\r\n"
            )
        }
        (_, Status::Running) => String::new(),
    };
    run.output.lock().expect("output lock").push(footer.as_bytes());
    run.writer.lock().expect("writer lock").take();
    run.killer.lock().expect("killer lock").take();
    run.tunnels.lock().expect("tunnels lock").close();

    let ended = history::now_ms();
    run.update(&app, |info| {
        info.status = state;
        info.exit_code = Some(code);
        info.ended_ms = Some(ended);
    });

    let info = run.info();
    if opts.record_history
        && let Some(mut rec) = record
    {
        rec.started_ms = info.started_ms;
        rec.id = format!("{:x}-{}", info.started_ms, info.id);
        rec.duration_ms = (ended - info.started_ms).max(0) as u64;
        rec.exit_code = if state == Status::Detached { None } else { Some(code) };
        let _ = history::append(&opts.history_file, &rec);
    }
    let watching = app
        .get_webview_window("main")
        .is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false));
    if kind == Kind::Bookmark && (opts.notify || opts.background || !watching) {
        notify_finished(&app, &info, &took);
    }
}

fn notify_finished<R: Runtime>(app: &AppHandle<R>, info: &RunInfo, took: &str) {
    use tauri_plugin_notification::NotificationExt;
    if app.try_state::<tauri_plugin_notification::Notification<R>>().is_none() {
        return;
    }
    let body = match info.status {
        Status::Succeeded => format!("Finished in {took}"),
        Status::Failed => format!("Failed with exit {} after {took}", info.exit_code.unwrap_or(1)),
        Status::Stopped => format!("Stopped after {took}"),
        Status::Disconnected => {
            format!("Lost the connection to {} after {took}", info.server.as_deref().unwrap_or("the server"))
        }
        Status::Detached => return,
        Status::Running => return,
    };
    let _ = app.notification().builder().title(&info.name).body(body).show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn signal_names() {
        assert_eq!(signal_number("Interrupt: 2"), 2);
        assert_eq!(signal_number("Interrupt"), 2);
        assert_eq!(signal_number("Terminated: 15"), 15);
        assert_eq!(signal_number("Killed"), 9);
    }

    #[test]
    fn output_buffer_trims_at_newline() {
        let mut o = Output { buf: Vec::new(), subs: Vec::new() };
        let line = vec![b'x'; 1023];
        for _ in 0..3000 {
            o.push(&line);
            o.push(b"\n");
        }
        assert!(o.buf.len() <= BUFFER_MAX);
        assert!(o.buf.len() >= BUFFER_KEEP - 1024);
        assert_eq!(o.buf[0], b'x');
    }

    #[test]
    fn statuses() {
        use Kind::*;
        assert_eq!(final_status(Bookmark, true, false, Some(0), 0), (Status::Succeeded, 0));
        assert_eq!(final_status(Bookmark, true, false, Some(3), 3), (Status::Failed, 3));
        // ssh exits 255 when the connection drops; no `done` marker came through.
        assert_eq!(final_status(Bookmark, true, false, None, 255), (Status::Disconnected, 255));
        // A local command that really exits 255 is just a failure.
        assert_eq!(final_status(Bookmark, false, false, None, 255), (Status::Failed, 255));
        assert_eq!(final_status(Bookmark, true, false, Some(130), 130), (Status::Stopped, 130));
        assert_eq!(final_status(Session, false, false, None, 255), (Status::Disconnected, 255));
        assert_eq!(final_status(Session, false, true, None, 255), (Status::Stopped, 255));
    }
}

/// The Windows run path (`bk __run` + progress file) works on every OS, so it's tested
/// everywhere, against the sidecar build of `bk` (skipped when it hasn't been built).
#[cfg(test)]
mod runner_e2e {
    use super::*;
    use bk_core::model::{Bookmark, Settings, Step};
    use bk_core::plan::RunRequest;
    use bk_core::store::{Entry, Scope};

    fn sidecar() -> Option<PathBuf> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("bk-")))
    }

    /// Waits for a run to end. No terminal view is attached, like a run started in the
    /// background, so on Windows this also checks that cursor queries get answered for it.
    fn wait_done(mgr: &RunManager, id: &str, secs: u64) -> RunInfo {
        let start = Instant::now();
        loop {
            let info = mgr.list().into_iter().find(|r| r.id == id).unwrap();
            if info.status != Status::Running {
                return info;
            }
            if start.elapsed() > Duration::from_secs(secs) {
                let out = String::from_utf8_lossy(&mgr.get(id).unwrap().output.lock().unwrap().buf).into_owned();
                panic!("run still going after {secs}s: {info:?}\noutput:\n{out}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn plan_for(dir: &std::path::Path, steps: Vec<Step>) -> RunPlan {
        let b = Bookmark {
            cwd: Some(".".into()),
            shell: Some(if cfg!(windows) { "powershell" } else { "sh" }.into()),
            steps,
            ..Default::default()
        };
        let mut e = Entry::new("runner", Scope::Global, &dir.join("b.toml"), b);
        e.base_dir = dir.to_path_buf();
        let req = RunRequest { default_cwd: dir.to_path_buf(), ..Default::default() };
        bk_core::plan::build(&e, &req, &bk_core::env::current(), &Settings::default(), bk_core::env::Mode::Run).unwrap()
    }

    /// The runner by itself, outside a pseudo-terminal: tells runner bugs from PTY ones.
    #[test]
    fn runner_without_a_terminal() {
        let Some(bk) = sidecar() else { return };
        let d = tempfile::tempdir().unwrap();
        let echo = if cfg!(windows) { "Write-Output one-$((1+1))" } else { "echo one-$((1+1))" };
        let plan = plan_for(d.path(), vec![Step::new(echo), Step::new("exit 3")]);
        let progress = d.path().join("progress");
        let steps = d.path().join("steps.json");
        std::fs::write(&steps, serde_json::to_string(&plan.steps_file(Some(progress.clone()))).unwrap()).unwrap();
        let mut child = std::process::Command::new(&bk)
            .arg("__run")
            .arg(&steps)
            .env_clear()
            .envs(&plan.env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let out = child.wait_with_output().unwrap();
                panic!(
                    "bk __run still going after 30s\nstdout: {}\nstderr: {}\nprogress: {:?}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                    std::fs::read_to_string(&progress)
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(status.code(), Some(3), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("one-2"), "{stdout}");
        let p = std::fs::read_to_string(&progress).unwrap();
        assert!(p.contains("step;2") && p.contains("done;3"), "{p}");
    }

    fn start(mgr: &RunManager, steps: Vec<Step>) -> (RunInfo, PathBuf) {
        let d = tempfile::tempdir().unwrap().keep();
        let plan = plan_for(&d, steps);
        let spawn = Spawn::local_runner(&plan);
        let progress = spawn.local_steps.as_ref().unwrap().progress_file.clone();
        let opts = StartOptions {
            cols: 100,
            rows: 30,
            background: false,
            notify: false,
            record_history: false,
            history_file: d.join("history.jsonl"),
        };
        let app = tauri::test::mock_app();
        (mgr.start(app.handle().clone(), spawn, opts, None).unwrap(), progress)
    }

    #[test]
    fn steps_progress_exit_codes_and_stop() {
        let Some(bk) = sidecar() else {
            eprintln!("skipping: no sidecar bk (node app/scripts/prepare-sidecar.mjs debug)");
            return;
        };
        // SAFETY: set before any run starts; the tests here read it only in `local_runner`.
        unsafe { std::env::set_var("CLIBOOKER_BK", &bk) };
        let mgr = RunManager::default();

        let echo = if cfg!(windows) { "Write-Output one-$((1+1))" } else { "echo one-$((1+1))" };
        let (info, progress) = start(&mgr, vec![Step::new(echo), Step::new("exit 3")]);
        let done = wait_done(&mgr, &info.id, 30);
        let out = String::from_utf8_lossy(&mgr.get(&info.id).unwrap().output.lock().unwrap().buf).into_owned();
        assert_eq!((done.status, done.exit_code), (Status::Failed, Some(3)), "{out}");
        assert_eq!(done.step, 2, "{out}");
        assert!(out.contains("one-2") && out.contains("[2/2]"), "{out}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!progress.exists(), "progress file left behind");

        let sleep = if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" };
        let (info, _) = start(&mgr, vec![Step::new(sleep), Step::new("echo never")]);
        std::thread::sleep(Duration::from_millis(1500));
        let t = Instant::now();
        mgr.stop(&info.id).unwrap();
        let done = wait_done(&mgr, &info.id, 15);
        let out = String::from_utf8_lossy(&mgr.get(&info.id).unwrap().output.lock().unwrap().buf).into_owned();
        assert_eq!(done.status, Status::Stopped, "{out}");
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(!out.contains("never"), "{out}");
    }
}

/// Headless end-to-end tests of remote runs against a real, unprivileged sshd on 127.0.0.1,
/// driven through Tauri's mock runtime (skipped when sshd is missing).
#[cfg(all(test, unix))]
mod ssh_e2e {
    use super::*;
    use bk_core::model::Bookmark;
    use bk_core::plan::RunRequest;
    use bk_core::remote::{self, RemoteInputs};
    use bk_core::store::{Entry, Scope};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

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

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn sshd() -> Option<Sshd> {
        if !Path::new("/usr/sbin/sshd").exists() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for k in ["hostkey", "client"] {
            Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(d.join(k)).status().unwrap();
        }
        std::fs::copy(d.join("client.pub"), d.join("authorized_keys")).unwrap();
        std::fs::write(
            d.join("ssh"),
            format!(
                "#!/bin/sh\nexec /usr/bin/ssh -o UserKnownHostsFile={d}/known_hosts -o StrictHostKeyChecking=accept-new -o LogLevel=ERROR \"$@\"\n",
                d = d.display()
            ),
        )
        .unwrap();
        Command::new("chmod").arg("+x").arg(d.join("ssh")).status().unwrap();
        // Parallel tests can be handed the same free port; if another test's sshd got it, ours
        // exits at once while the port still answers. Check ours is alive, else try another port.
        let mut started = None;
        for _ in 0..5 {
            let port = free_port();
            std::fs::write(
                d.join("sshd_config"),
                format!(
                    "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/hostkey\nAuthorizedKeysFile {d}/authorized_keys\n\
                     PidFile {d}/pid\nStrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
                     AllowTcpForwarding yes\nPermitTTY yes\nLogLevel ERROR\n\
                     SetEnv PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\n",
                    d = d.display()
                ),
            )
            .unwrap();
            let mut child = Command::new("/usr/sbin/sshd")
                .args(["-D", "-e", "-f"])
                .arg(d.join("sshd_config"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let start = Instant::now();
            while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() && start.elapsed() < Duration::from_secs(5)
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            std::thread::sleep(Duration::from_millis(150));
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() && matches!(child.try_wait(), Ok(None)) {
                started = Some((port, child));
                break;
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        let (port, child) = started?;
        Some(Sshd { dir, port, child })
    }

    struct Ctx {
        sshd: Sshd,
        paths: Paths,
        servers: BTreeMap<String, bk_core::Server>,
        env: EnvMap,
        _data: tempfile::TempDir,
    }

    impl Ctx {
        fn new() -> Option<Ctx> {
            let sshd = sshd()?;
            let data = tempfile::tempdir().unwrap();
            let paths = Paths { config_file: data.path().join("b.toml"), data_dir: data.path().join("data") };
            let mut servers = BTreeMap::new();
            servers.insert(
                "t".to_string(),
                bk_core::Server {
                    host: "127.0.0.1".into(),
                    port: Some(sshd.port),
                    identity: Some(sshd.dir.path().join("client").display().to_string()),
                    ..Default::default()
                },
            );
            let mut env = bk_core::env::current();
            env.insert("TERM".into(), "xterm-256color".into());
            Some(Ctx { sshd, paths, servers, env, _data: data })
        }

        fn plan(&self, name: &str, b: Bookmark) -> RemotePlan {
            let e =
                Entry::new(name, Scope::Global, &self.paths.config_file, Bookmark { server: Some("t".into()), ..b });
            let inp = RemoteInputs {
                servers: &self.servers,
                paths: &self.paths,
                ssh_program: self.sshd.dir.path().join("ssh").display().to_string(),
                base_env: &self.env,
                online: true,
                markers: true,
            };
            remote::build(&e, &RunRequest::default(), &inp).unwrap()
        }

        fn start(&self, mgr: &RunManager, spawn: Spawn) -> RunInfo {
            let app = tauri::test::mock_app();
            let ctl = spawn.remote.clone();
            let opts = StartOptions {
                cols: 100,
                rows: 30,
                background: false,
                notify: false,
                record_history: false,
                history_file: self.paths.history_file(),
            };
            mgr.start(app.handle().clone(), spawn, opts, ctl.as_ref()).unwrap()
        }
    }

    fn wait_done(mgr: &RunManager, id: &str, secs: u64) -> RunInfo {
        let start = Instant::now();
        loop {
            let info = mgr.list().into_iter().find(|r| r.id == id).unwrap();
            if info.status != Status::Running {
                return info;
            }
            assert!(start.elapsed() < Duration::from_secs(secs), "run still going after {secs}s");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn output(mgr: &RunManager, id: &str) -> String {
        String::from_utf8_lossy(&mgr.get(id).unwrap().output.lock().unwrap().buf).into_owned()
    }

    #[test]
    fn remote_runs_stop_tunnels_and_detach() {
        let Some(c) = Ctx::new() else { return };
        let mgr = RunManager::default();

        // 1. A plain remote workflow: markers come back through ssh.
        let plan = c.plan(
            "hello",
            Bookmark {
                steps: vec![bk_core::Step::new("echo one"), bk_core::Step::new("echo two")],
                ..Default::default()
            },
        );
        let info = c.start(&mgr, Spawn::remote(&plan, &c.paths));
        let done = wait_done(&mgr, &info.id, 20);
        assert_eq!(
            (done.status, done.exit_code, done.step),
            (Status::Succeeded, Some(0), 2),
            "{}",
            output(&mgr, &info.id)
        );
        let out = output(&mgr, &info.id);
        assert!(out.contains("one") && out.contains("two") && out.contains("[2/2]") && !out.contains("7701"), "{out}");

        // 2. Stop: ^C travels through ssh to the remote Python.
        let plan = c.plan(
            "sleeper",
            Bookmark { command: Some("python3 -c 'import time; time.sleep(60)'".into()), ..Default::default() },
        );
        let info = c.start(&mgr, Spawn::remote(&plan, &c.paths));
        std::thread::sleep(Duration::from_millis(2500));
        mgr.stop(&info.id).unwrap();
        let done = wait_done(&mgr, &info.id, 15);
        assert_eq!(done.status, Status::Stopped, "{}", output(&mgr, &info.id));
        assert!(output(&mgr, &info.id).contains("KeyboardInterrupt"), "{}", output(&mgr, &info.id));

        // 3. A process that ignores ^C is killed via the remote process group.
        let plan = c.plan("stubborn", Bookmark { command: Some("trap '' INT; sleep 60".into()), ..Default::default() });
        let info = c.start(&mgr, Spawn::remote(&plan, &c.paths));
        std::thread::sleep(Duration::from_millis(2000));
        let t0 = Instant::now();
        mgr.stop(&info.id).unwrap();
        let done = wait_done(&mgr, &info.id, 20);
        assert_eq!(done.status, Status::Stopped);
        assert!(t0.elapsed() < Duration::from_secs(12), "escalation took {:?}", t0.elapsed());

        // 4. A tunnel to a web server started on the "server".
        let (lport, rport) = (free_port(), free_port());
        let site = tempfile::tempdir().unwrap();
        std::fs::write(site.path().join("index.html"), "tunnel-ok").unwrap();
        let plan = c.plan(
            "web",
            Bookmark {
                cwd: Some(site.path().display().to_string()),
                command: Some(format!("python3 -m http.server {rport} --bind 127.0.0.1")),
                forward: vec![format!("{lport}:127.0.0.1:{rport}")],
                ..Default::default()
            },
        );
        let info = c.start(&mgr, Spawn::remote(&plan, &c.paths));
        assert!(info.forwards[0].error.is_none(), "{:?}", info.forwards);
        // Wait for the server to come up (a cold login shell and Python can be slow on CI).
        let t0 = Instant::now();
        while !output(&mgr, &info.id).contains("Serving HTTP") && t0.elapsed() < Duration::from_secs(40) {
            std::thread::sleep(Duration::from_millis(300));
        }
        let mut body = String::new();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(15) {
            if let Ok(o) =
                Command::new("curl").args(["-s", "--max-time", "2", &format!("http://127.0.0.1:{lport}/")]).output()
            {
                body = String::from_utf8_lossy(&o.stdout).into_owned();
                if body.contains("tunnel-ok") {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        assert!(
            body.contains("tunnel-ok"),
            "got {body:?}; run: {:?}\n{}\noutput:\n{}",
            mgr.list().into_iter().find(|r| r.id == info.id),
            mgr.get(&info.id).unwrap().tunnels.lock().unwrap().describe(),
            output(&mgr, &info.id)
        );
        mgr.stop(&info.id).unwrap();
        wait_done(&mgr, &info.id, 15);
        // The tunnel closes with the run.
        std::thread::sleep(Duration::from_millis(500));
        assert!(std::net::TcpListener::bind(("127.0.0.1", lport)).is_ok(), "tunnel port still held");
    }

    #[test]
    fn keep_running_detach_and_reattach() {
        let Some(c) = Ctx::new() else { return };
        let mgr = RunManager::default();
        let name = "e2e-detach";
        let plan = c.plan(
            name,
            Bookmark { command: Some("echo started; sleep 30".into()), detach: true, ..Default::default() },
        );
        let ctl = RemoteCtl {
            program: plan.program.clone(),
            target: plan.target.clone(),
            paths: c.paths.clone(),
            env: plan.env.clone(),
        };
        match remote::tmux_state(&ctl.batch(), name).unwrap() {
            remote::TmuxState::NoTmux => return,
            remote::TmuxState::Alive => remote::detached_kill(&ctl.batch(), name),
            remote::TmuxState::Absent => {}
        }
        let mut spawn = Spawn::remote(&plan, &c.paths);
        spawn.args = remote::detached_start(&plan, &ctl.batch(), &c.paths).unwrap();
        spawn.tmux = Some((name.to_string(), false));
        let info = c.start(&mgr, spawn);
        std::thread::sleep(Duration::from_secs(4));
        assert_eq!(mgr.list().iter().find(|r| r.id == info.id).unwrap().step, 1, "progress from the state file");
        mgr.detach_tmux(&info.id).unwrap();
        let done = wait_done(&mgr, &info.id, 15);
        assert_eq!(done.status, Status::Detached, "{}", output(&mgr, &info.id));
        assert_eq!(remote::tmux_state(&ctl.batch(), name).unwrap(), remote::TmuxState::Alive);
        // Reattach, then Stop ends the tmux session for good.
        let mut spawn = Spawn::remote(&plan, &c.paths);
        spawn.args = remote::detached_attach(&plan, &c.paths);
        spawn.tmux = Some((name.to_string(), true));
        let info = c.start(&mgr, spawn);
        assert!(info.reattached);
        std::thread::sleep(Duration::from_secs(2));
        mgr.stop(&info.id).unwrap();
        let done = wait_done(&mgr, &info.id, 20);
        assert_eq!(done.status, Status::Stopped, "{}", output(&mgr, &info.id));
        assert_eq!(remote::tmux_state(&ctl.batch(), name).unwrap(), remote::TmuxState::Absent);
        for ext in ["sh", "state", "log"] {
            let _ = std::fs::remove_file(bk_core::paths::home().join(format!(".local/state/bk/bk-{name}.{ext}")));
        }
    }
}
