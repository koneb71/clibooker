//! End-to-end CLI tests on Windows, where steps run in PowerShell by default.
#![cfg(windows)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox { dir: tempfile::tempdir().unwrap() }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn config(&self) -> PathBuf {
        self.path().join("cfg").join("bookmarks.toml")
    }

    fn bk(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bk"));
        c.env("BK_CONFIG", self.config())
            .env("BK_DATA_DIR", self.path().join("data"))
            .env_remove("VIRTUAL_ENV")
            .env_remove("CONDA_DEFAULT_ENV")
            .env_remove("BK_SHELL_INTEGRATION")
            .env("NO_COLOR", "1")
            .current_dir(self.path());
        c
    }

    fn add(&self, args: &[&str]) {
        self.bk().arg("add").args(args).assert().success();
    }
}

#[test]
fn powershell_steps_params_and_exit_codes() {
    let s = Sandbox::new();
    s.add(&["hello", "Write-Output \"hi {{who:there}}\"", "--cwd", "."]);
    s.bk().arg("hello").assert().success().stdout(contains("hi there"));
    s.bk().args(["hello", "who=you"]).assert().success().stdout(contains("hi you"));
    // A native program's exit code comes through (PowerShell on its own would report 1).
    s.add(&["seven", "cmd /c exit 7"]);
    s.bk().arg("seven").assert().code(7);
    // A workflow stops at the first failing step.
    s.add(&["wf", "--step", "Write-Output one", "--step", "exit 3", "--step", "Write-Output never"]);
    s.bk().arg("wf").assert().code(3).stdout(contains("one").and(contains("never").not()));
    // Crash codes (0xC0000005) are failures, never success.
    s.add(&["crash", "exit -1073741819"]);
    s.bk().arg("crash").assert().code(1);
    let hist = std::fs::read_to_string(s.path().join("data").join("history.jsonl")).unwrap();
    assert_eq!(hist.lines().count(), 5);
}

#[test]
fn cmd_shell_and_quoted_extra_args() {
    let s = Sandbox::new();
    s.add(&["c", "echo from cmd & exit /b 4", "--shell", "cmd"]);
    s.bk().arg("c").assert().code(4).stdout(contains("from cmd"));
    s.add(&["args", "Write-Output"]);
    s.bk()
        .args(["args", "--", "a b", "it's", "--flag"])
        .assert()
        .success()
        .stdout(contains("a b").and(contains("it's")).and(contains("--flag")));
}

#[test]
fn venv_scripts_folder_and_go_for_powershell() {
    let s = Sandbox::new();
    let v = s.path().join(".venv");
    std::fs::create_dir_all(v.join("Scripts")).unwrap();
    std::fs::write(v.join("pyvenv.cfg"), "home = C:\\Python\n").unwrap();
    s.add(&["py", "Write-Output \"$env:VIRTUAL_ENV|$(($env:PATH -split ';')[0])\"", "--cwd", ".", "--env", "venv"]);
    s.bk().arg("py").assert().success().stdout(contains(".venv|").and(contains("Scripts")));
    s.bk()
        .args(["go", "--print", "--shell", "powershell", "py"])
        .assert()
        .success()
        .stdout(contains("Set-Location -LiteralPath").and(contains("Activate.ps1")));
}

#[test]
fn powershell_hook_and_save() {
    let s = Sandbox::new();
    s.bk().args(["init", "powershell", "--record"]).assert().success().stdout(
        contains("function global:prompt").and(contains("BK_PS_SESSION")).and(contains("Register-ArgumentCompleter")),
    );
    s.bk()
        .args(["save", "ls-here", "-y"])
        .env("BK_SHELL_INTEGRATION", "powershell")
        .env("BK_LAST_CMD", "Get-ChildItem")
        .env("BK_LAST_PWD", s.path())
        .env("BK_LAST_STATUS", "0")
        .assert()
        .success();
    s.bk().args(["ls", "--names"]).assert().success().stdout("ls-here\n");
    let cfg = std::fs::read_to_string(s.config()).unwrap();
    assert!(cfg.contains("Get-ChildItem"), "{cfg}");

    // The PowerShell hook's session file: `bk save` reads the last command from it, and
    // `bk go` leaves its script there for the prompt.
    let session = s.path().join("ps-1.json");
    std::fs::write(&session, r#"{"cmd":"Get-Date","pwd":"","status":"0","kind":"builtin"}"#).unwrap();
    s.bk().args(["save", "when", "-y"]).env("BK_PS_SESSION", &session).assert().success();
    s.bk().args(["ls", "--names"]).assert().success().stdout("ls-here\nwhen\n");
    s.bk().args(["go", "when"]).env("BK_PS_SESSION", &session).assert().success();
    let go = std::fs::read_to_string(format!("{}.go", session.display())).unwrap();
    assert!(go.starts_with("Set-Location -LiteralPath"), "{go}");
}
