//! Execute a plan in the current terminal, and `bk __run` (the desktop app's step runner on
//! Windows, where there is no POSIX `sh` for the app's wrapper script).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use bk_core::env::{self, EnvMap};
use bk_core::plan::{PlannedStep, RunPlan, StepsFile};
use bk_core::proc;
use bk_core::shell::{self, ShellKind};
use console::style;

pub struct Outcome {
    pub code: i32,
    pub interrupted: bool,
}

static CTRL_C: AtomicBool = AtomicBool::new(false);

/// Catch (not ignore) Ctrl+C while children run. The child still gets it (the terminal sends
/// SIGINT / CTRL_C_EVENT to the whole foreground group), and on Unix a caught handler resets
/// to default on exec, so Python sees KeyboardInterrupt. We only note that it happened.
fn catch_ctrl_c() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = ctrlc::set_handler(|| CTRL_C.store(true, Ordering::SeqCst));
    });
}

/// How a run reports itself: the CLI's own headers, or the app wrapper's look plus progress
/// lines in a state file.
enum Report {
    Cli,
    App { state: Option<File> },
}

impl Report {
    fn mark(&mut self, line: &str) {
        if let Report::App { state: Some(f) } = self {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

struct Job<'a> {
    shell: &'a Path,
    shell_args: &'a [String],
    kind: ShellKind,
    env: &'a EnvMap,
    steps: &'a [PlannedStep],
}

impl Job<'_> {
    fn process(&self, step: &PlannedStep) -> Command {
        shell::step_process(self.shell, self.shell_args, self.kind, &step.command, &step.cwd, self.env)
    }
}

pub fn run(plan: &RunPlan) -> Result<Outcome> {
    let job = Job {
        shell: &plan.shell,
        shell_args: &plan.shell_args,
        kind: plan.shell_kind,
        env: &plan.env,
        steps: &plan.steps,
    };
    run_job(&job, &mut Report::Cli)
}

/// `bk __run FILE`: run a [`StepsFile`] with this process's environment, printing like the
/// app's wrapper and appending progress to its state file. Exits with the run's status.
pub fn run_steps_file(file: &Path) -> Result<i32> {
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let sf: StepsFile = serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
    let state = match &sf.state_file {
        Some(p) => Some(
            OpenOptions::new().create(true).append(true).open(p).with_context(|| format!("opening {}", p.display()))?,
        ),
        None => None,
    };
    let mut report = Report::App { state };
    if !sf.title.is_empty() {
        println!("\x1b[2m{}\x1b[0m", sf.title);
    }
    let env = env::current();
    let job = Job { shell: &sf.shell, shell_args: &sf.shell_args, kind: sf.shell_kind, env: &env, steps: &sf.steps };
    let outcome = run_job(&job, &mut report)?;
    report.mark(&format!("done;{}", outcome.code));
    Ok(outcome.code)
}

fn run_job(job: &Job, report: &mut Report) -> Result<Outcome> {
    catch_ctrl_c();
    let total = job.steps.len();
    let mut code = 0;
    for (i, step) in job.steps.iter().enumerate() {
        let n = i + 1;
        report.mark(&format!("step;{n}"));
        match report {
            Report::Cli if total > 1 => {
                eprintln!("{} {}", style(format!("▶ [{n}/{total}]")).cyan().bold(), style(&step.label).bold());
            }
            Report::Cli => {}
            Report::App { .. } if total > 1 => {
                println!("\x1b[1;36m▶ [{n}/{total}]\x1b[0m \x1b[1m{}\x1b[0m", step.label)
            }
            Report::App { .. } => println!("\x1b[2m$ {}\x1b[0m", step.command),
        }
        CTRL_C.store(false, Ordering::SeqCst);
        let status = job.process(step).status().with_context(|| format!("starting {}", job.shell.display()))?;
        code = proc::exit_code(status);
        report.mark(&format!("exit;{n};{code}"));
        if proc::interrupted(status) || (code != 0 && CTRL_C.load(Ordering::SeqCst)) {
            return Ok(Outcome { code: 130, interrupted: true });
        }
        if code != 0 {
            if step.continue_on_error {
                let msg = format!("  step failed (exit {code}), continuing");
                match report {
                    Report::Cli => eprintln!("{}", style(msg).yellow()),
                    Report::App { .. } => println!("\x1b[33m{msg}\x1b[0m"),
                }
                if matches!(report, Report::App { .. }) {
                    code = 0;
                }
                continue;
            }
            if total > 1 && matches!(report, Report::Cli) {
                eprintln!("{}", style(format!("✗ stopped at step {n}/{total} (exit {code})")).red());
            }
            return Ok(Outcome { code, interrupted: false });
        }
    }
    Ok(Outcome { code, interrupted: false })
}

/// Run a remote plan: ssh with the terminal attached. Ctrl+C reaches the server because
/// ssh puts the local terminal in raw mode.
pub fn run_remote(plan: &bk_core::remote::RemotePlan) -> Result<Outcome> {
    catch_ctrl_c();
    CTRL_C.store(false, Ordering::SeqCst);
    let status = Command::new(&plan.program)
        .args(plan.args_with_forwards())
        .env_clear()
        .envs(&plan.env)
        .status()
        .with_context(|| format!("starting {}", plan.program))?;
    let code = proc::exit_code(status);
    if code == 255 {
        eprintln!(
            "{}",
            style(format!("✗ ssh to {} failed or the connection dropped (exit 255)", plan.target.name)).red()
        );
    }
    Ok(Outcome { code, interrupted: proc::interrupted(status) })
}

/// Run a program attached to this terminal and return its status (`exec` where the OS has it).
pub fn replace_process(program: &str, args: &[String], env: &EnvMap) -> Result<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(program).args(args).env_clear().envs(env).exec();
        Err(err).with_context(|| format!("starting {program}"))
    }
    #[cfg(not(unix))]
    {
        catch_ctrl_c();
        let status = Command::new(program)
            .args(args)
            .env_clear()
            .envs(env)
            .status()
            .with_context(|| format!("starting {program}"))?;
        Ok(proc::exit_code(status))
    }
}

/// Run a program attached to this terminal and carry on when it's done (ssh-keygen, ssh-add, an
/// ssh that does one thing).
pub fn run_attached(program: &str, args: &[String], env: &EnvMap) -> Result<i32> {
    catch_ctrl_c();
    let status = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .status()
        .with_context(|| format!("starting {program}"))?;
    Ok(proc::exit_code(status))
}

/// End like the interrupted child did, so calling shells and loops see the interrupt.
pub fn reraise_sigint() -> ! {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::raise(libc::SIGINT);
    }
    std::process::exit(130)
}
