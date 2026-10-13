//! Execute a plan in the current terminal.

use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Once;

use anyhow::{Context, Result};
use bk_core::plan::RunPlan;
use console::style;

pub struct Outcome {
    pub code: i32,
    pub interrupted: bool,
}

/// Catch (not ignore) SIGINT while children run: the child still gets the terminal's
/// SIGINT, and a caught handler resets to default on exec, so Python sees KeyboardInterrupt.
fn catch_sigint() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = ctrlc::set_handler(|| {});
    });
}

pub fn run(plan: &RunPlan) -> Result<Outcome> {
    catch_sigint();
    let total = plan.steps.len();
    let mut code = 0;
    for (i, step) in plan.steps.iter().enumerate() {
        if total > 1 {
            eprintln!("{} {}", style(format!("▶ [{}/{}]", i + 1, total)).cyan().bold(), style(&step.label).bold());
        }
        let status = Command::new(&plan.shell)
            .args(&plan.shell_args)
            .arg(&step.command)
            .current_dir(&step.cwd)
            .env_clear()
            .envs(&plan.env)
            .status()
            .with_context(|| format!("starting {}", plan.shell.display()))?;
        code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
        if status.signal() == Some(libc::SIGINT) || code == 130 {
            return Ok(Outcome { code: 130, interrupted: true });
        }
        if code != 0 {
            if step.continue_on_error {
                eprintln!("{}", style(format!("  step failed (exit {code}), continuing")).yellow());
                continue;
            }
            if total > 1 {
                eprintln!("{}", style(format!("✗ stopped at step {}/{} (exit {code})", i + 1, total)).red());
            }
            return Ok(Outcome { code, interrupted: false });
        }
    }
    Ok(Outcome { code, interrupted: false })
}

/// Run a remote plan: ssh with the terminal attached. Ctrl+C reaches the server because
/// ssh puts the local terminal in raw mode.
pub fn run_remote(plan: &bk_core::remote::RemotePlan) -> Result<Outcome> {
    catch_sigint();
    let status = Command::new(&plan.program)
        .args(plan.args_with_forwards())
        .env_clear()
        .envs(&plan.env)
        .status()
        .with_context(|| format!("starting {}", plan.program))?;
    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    if code == 255 {
        eprintln!(
            "{}",
            style(format!("✗ ssh to {} failed or the connection dropped (exit 255)", plan.target.name)).red()
        );
    }
    Ok(Outcome { code, interrupted: status.signal() == Some(libc::SIGINT) })
}

/// Die from SIGINT so calling shells/loops see the interrupt.
pub fn reraise_sigint() -> ! {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::raise(libc::SIGINT);
    }
    std::process::exit(130)
}
