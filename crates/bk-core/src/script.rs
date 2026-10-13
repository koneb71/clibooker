//! The POSIX `sh` wrapper that runs a bookmark's steps — used for app runs (locally in a
//! PTY) and for remote runs (on the server, through ssh).
//!
//! It prints the step headers itself and reports progress with private OSC markers
//! (`ESC ] 7701 ; payload BEL`, see the app's `markers.rs`):
//! `pgid;N`, `step;N`, `exit;N;CODE`, and finally `done;CODE`.
//! The body is a function that `return`s: calling `exit` under `bash -l` would run
//! `~/.bash_logout`. Ctrl+C is trapped so the wrapper survives to report it while the
//! step itself still gets a normal SIGINT.

use crate::render::q;

#[derive(Debug, Clone, PartialEq)]
pub struct WrapperStep {
    pub label: String,
    /// Shell code that runs the step (e.g. `'/bin/zsh' '-f' '-c' 'make test'`).
    pub run: String,
    /// Shell expression to `cd` into first (already quoted), if any.
    pub cd: Option<String>,
    /// Shown as `$ …` for single-step runs.
    pub display: String,
    pub continue_on_error: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WrapperOptions {
    /// Emit OSC progress markers (the app parses them; the CLI turns them off).
    pub markers: bool,
    /// Also append progress lines to this file (shell expression), for tmux runs where
    /// markers can't reach the app.
    pub state_file: Option<String>,
    /// Dim title line printed first.
    pub title: String,
    /// Setup commands (cd, activation, exports) run before the steps; a failure aborts.
    pub prelude: Vec<String>,
}

pub fn wrapper(steps: &[WrapperStep], o: &WrapperOptions) -> String {
    let mut s = String::new();
    let state = o.state_file.clone().unwrap_or_default();
    s.push_str(&format!("bk_state={state}\n"));
    s.push_str("bk_mark() {\n");
    if o.markers {
        s.push_str("  printf '\\033]7701;%s\\007' \"$1\"\n");
    }
    s.push_str("  [ -n \"$bk_state\" ] && printf '%s\\n' \"$1\" >> \"$bk_state\"\n  return 0\n}\n");
    s.push_str("bk_main() {\n  bk_int=0\n  s=0\n  trap 'bk_int=1' INT\n");
    s.push_str(
        "  bk_pg=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')\n  [ -n \"$bk_pg\" ] && bk_mark \"pgid;$bk_pg\"\n",
    );
    if !o.title.is_empty() {
        s.push_str(&format!("  printf '\\033[2m%s\\033[0m\\n' {}\n", q(&o.title)));
    }
    if !o.prelude.is_empty() {
        s.push_str(&format!(
            "  {{ {}; }} || {{ s=$?; printf '\\033[31mbk: setup failed (exit %s)\\033[0m\\n' \"$s\"; return $s; }}\n",
            o.prelude.join(" && ")
        ));
    }
    let total = steps.len();
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        s.push_str(&format!("  bk_mark 'step;{n}'\n"));
        if total > 1 {
            s.push_str(&format!(
                "  printf '\\033[1;36m▶ [{n}/{total}]\\033[0m \\033[1m%s\\033[0m\\n' {}\n",
                q(&step.label)
            ));
        } else {
            s.push_str(&format!("  printf '\\033[2m$ %s\\033[0m\\n' {}\n", q(&step.display)));
        }
        let run = match &step.cd {
            Some(cd) => format!("( cd {cd} && {} )", step.run),
            None => step.run.clone(),
        };
        s.push_str(&format!("  {run}\n  s=$?\n  bk_mark \"exit;{n};$s\"\n"));
        s.push_str("  [ \"$bk_int\" = 1 ] && return 130\n");
        if step.continue_on_error {
            s.push_str(
                "  [ $s -ne 0 ] && printf '\\033[33m  step failed (exit %s), continuing\\033[0m\\n' \"$s\" && s=0\n",
            );
        } else {
            s.push_str("  [ $s -ne 0 ] && return $s\n");
        }
    }
    s.push_str("  return $s\n}\n");
    // `(exit N)` sets the status without the `exit` builtin ending a login shell.
    s.push_str("bk_main\nbk_rc=$?\nbk_mark \"done;$bk_rc\"\n(exit $bk_rc)\n");
    s
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    fn step(cmd: &str, keep: bool) -> WrapperStep {
        WrapperStep {
            label: cmd.into(),
            run: format!("sh -c {}", q(cmd)),
            cd: None,
            display: cmd.into(),
            continue_on_error: keep,
        }
    }

    fn run(script: &str) -> (i32, String) {
        let out = Command::new("/bin/sh").arg("-c").arg(script).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn markers_and_exit_codes() {
        let o = WrapperOptions { markers: true, title: "t".into(), ..Default::default() };
        let script = wrapper(
            &[step("echo one", false), step("false", true), step("exit 3", false), step("echo never", false)],
            &o,
        );
        assert!(Command::new("/bin/sh").args(["-n", "-c", &script]).status().unwrap().success());
        let (code, out) = run(&script);
        assert_eq!(code, 3);
        assert!(out.contains("one") && !out.contains("never") && out.contains("continuing"), "{out}");
        for m in ["step;1", "exit;1;0", "exit;2;1", "exit;3;3", "done;3", "pgid;"] {
            assert!(out.contains(&format!("\x1b]7701;{m}")), "missing {m}: {out:?}");
        }
    }

    #[test]
    fn setup_failure_and_state_file() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let o = WrapperOptions {
            markers: false,
            state_file: Some(q(&state.display().to_string())),
            prelude: vec!["cd /definitely/missing".into()],
            ..Default::default()
        };
        let (code, out) = run(&wrapper(&[step("echo x", false)], &o));
        assert_ne!(code, 0);
        assert!(out.contains("setup failed"), "{out}");
        let st = std::fs::read_to_string(&state).unwrap();
        assert!(st.contains("done;"), "{st}");
        assert!(!out.contains("\x1b]7701"), "markers off: {out:?}");
    }

    #[test]
    fn step_cd() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let mut st = step("pwd", false);
        st.cd = Some(q(&d.path().join("sub").display().to_string()));
        let (code, out) = run(&wrapper(&[st], &WrapperOptions::default()));
        assert_eq!(code, 0);
        assert!(out.trim_end().ends_with("sub"), "{out}");
    }
}
