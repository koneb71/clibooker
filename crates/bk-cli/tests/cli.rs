//! End-to-end CLI tests with POSIX shell steps (Windows has its own in `windows.rs`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

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
        self.path().join("cfg/bookmarks.toml")
    }

    fn bk(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bk"));
        c.env("BK_CONFIG", self.config())
            .env("BK_DATA_DIR", self.path().join("data"))
            .env_remove("VIRTUAL_ENV")
            .env_remove("CONDA_DEFAULT_ENV")
            // Steps run in /bin/sh whatever the machine's login shell is.
            .env("SHELL", "/bin/sh")
            .env("NO_COLOR", "1")
            .current_dir(self.path());
        c
    }

    fn add(&self, args: &[&str]) {
        self.bk().arg("add").args(args).assert().success();
    }
}

#[test]
fn add_list_and_run() {
    let s = Sandbox::new();
    s.add(&["hello", "echo hi from $PWD", "--cwd", ".", "-t", "demo"]);
    s.add(&["other", "--", "printf", "%s|", "a b", "c"]);
    s.bk().args(["ls", "--names"]).assert().success().stdout("hello\nother\n");
    s.bk().args(["ls", "--tag", "demo", "--names"]).assert().success().stdout("hello\n");
    let dir = s.path().canonicalize().unwrap();
    s.bk().arg("hello").assert().success().stdout(contains(format!("hi from {}", dir.display())));
    // Words after `--` are re-quoted so "a b" stays one argument.
    s.bk().arg("other").assert().success().stdout("a b|c|");
    // The run was recorded.
    let hist = std::fs::read_to_string(s.path().join("data/history.jsonl")).unwrap();
    assert_eq!(hist.lines().count(), 2);
}

#[test]
fn duplicate_and_reserved_names() {
    let s = Sandbox::new();
    s.add(&["x", "ls"]);
    s.bk().args(["add", "x", "ls"]).assert().failure().stderr(contains("already exists"));
    s.bk().args(["add", "x", "pwd", "--force"]).assert().success();
    s.bk().args(["add", "run", "ls"]).assert().failure().stderr(contains("subcommand"));
}

#[test]
fn params_extra_args_and_errors() {
    let s = Sandbox::new();
    s.add(&["p", "echo {{greeting:hello}} {{who}}"]);
    s.bk().args(["p", "who=you"]).assert().success().stdout("hello you\n");
    s.bk().args(["p", "who=you", "greeting=hey", "--", "and 'more'"]).assert().success().stdout("hey you and 'more'\n");
    // No TTY: missing params are an error instead of a prompt.
    s.bk().arg("p").assert().failure().stderr(contains("missing value for `who`"));
    s.bk().args(["p", "typo=1"]).assert().failure().stderr(contains("no param `typo`"));
}

#[test]
fn exit_codes_and_workflows() {
    let s = Sandbox::new();
    s.add(&["fail", "exit 7"]);
    s.bk().arg("fail").assert().code(7);
    s.add(&["wf", "--step", "echo one", "--step", "false", "--step", "echo never"]);
    s.bk().arg("wf").assert().code(1).stdout("one\n").stderr(contains("stopped at step 2/3"));
    std::fs::write(
        s.config(),
        std::fs::read_to_string(s.config()).unwrap()
            + "\n[bookmarks.keepgoing]\nsteps = [{ run = \"false\", continue_on_error = true }, \"echo after\"]\n",
    )
    .unwrap();
    s.bk().arg("keepgoing").assert().success().stdout("after\n");
}

#[test]
fn dry_run_and_print_do_not_execute() {
    let s = Sandbox::new();
    s.add(&["touchy", "touch marker"]);
    s.bk().args(["run", "touchy", "--dry-run"]).assert().success().stderr(contains("touch marker"));
    s.bk().args(["run", "touchy", "--print"]).assert().success().stdout(contains("&& touch marker )"));
    assert!(!s.path().join("marker").exists());
}

#[test]
fn unknown_bookmark_suggests() {
    let s = Sandbox::new();
    s.add(&["train-big", "ls"]);
    s.bk().arg("train").assert().failure().stderr(contains("did you mean: train-big"));
}

#[test]
fn project_file_shadows_global() {
    let s = Sandbox::new();
    s.add(&["dev", "echo global"]);
    let proj = s.path().join("proj");
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    s.bk().current_dir(&proj).args(["add", "dev", "echo project from $PWD", "--project"]).assert().success();
    let proj_canon = proj.canonicalize().unwrap();
    // From a subdirectory, the project's .bk.toml is found and wins.
    s.bk()
        .current_dir(proj.join("sub"))
        .arg("dev")
        .assert()
        .success()
        .stdout(format!("project from {}\n", proj_canon.display()));
    s.bk().arg("dev").assert().success().stdout("global\n");
    let text = std::fs::read_to_string(proj.join(".bk.toml")).unwrap();
    assert!(text.contains("cwd = \".\""), "{text}");
}

#[test]
fn venv_activation_and_cleaning() {
    let s = Sandbox::new();
    let ok = StdCommand::new("python3").args(["-m", "venv", "--without-pip", ".venv"]).current_dir(s.path()).status();
    if !ok.is_ok_and(|st| st.success()) {
        eprintln!("skipping: python3 -m venv unavailable");
        return;
    }
    s.add(&[
        "py",
        "--env",
        "venv:.venv",
        "python -c 'import sys,os; print(sys.prefix); print(os.environ.get(\"CONDA_PREFIX\"))'",
    ]);
    let venv = s.path().canonicalize().unwrap().join(".venv");
    s.bk()
        .arg("py")
        .env("CONDA_PREFIX", "/some/conda")
        .env("CONDA_DEFAULT_ENV", "x")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(format!("{}\nNone\n", venv.display())));
}

#[test]
fn venv_detected_on_add() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.path().join("venv/bin")).unwrap();
    std::fs::write(s.path().join("venv/pyvenv.cfg"), "").unwrap();
    s.bk().args(["add", "d", "ls"]).assert().success().stderr(contains("venv:venv [detected in folder]"));
}

#[test]
fn conda_env_if_available() {
    let s = Sandbox::new();
    let has_test_env = Path::new("/opt/miniconda3/envs/test/conda-meta").is_dir();
    if !has_test_env {
        eprintln!("skipping: conda env `test` not present");
        return;
    }
    s.add(&["c", "--env", "conda:test", "python -c 'import sys; print(sys.prefix)'"]);
    s.bk()
        .arg("c")
        .env("VIRTUAL_ENV", "/fake/venv")
        .env("PATH", format!("/fake/venv/bin:{}", std::env::var("PATH").unwrap()))
        .assert()
        .success()
        .stdout("/opt/miniconda3/envs/test\n");
}

#[test]
fn save_uses_hook_variables() {
    let s = Sandbox::new();
    s.bk().args(["save", "x"]).assert().failure().stderr(contains("needs the shell hook"));
    let dir = s.path().join("work");
    std::fs::create_dir_all(&dir).unwrap();
    s.bk()
        .args(["save", "last", "--yes"])
        .env("BK_SHELL_INTEGRATION", "zsh")
        .env("BK_LAST_CMD", "ll -h")
        .env("BK_LAST_CMD_EXPANDED", "ls -la -h")
        .env("BK_LAST_KIND", "alias")
        .env("BK_LAST_PWD", &dir)
        .env("BK_LAST_STATUS", "0")
        .assert()
        .success()
        .stderr(contains("alias"));
    let text = std::fs::read_to_string(s.config()).unwrap();
    assert!(text.contains("command = \"ls -la -h\""), "{text}");
    assert!(text.contains("work"), "{text}");
}

#[test]
fn go_prints_enter_script() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.path().join(".venv/bin")).unwrap();
    std::fs::write(s.path().join(".venv/pyvenv.cfg"), "").unwrap();
    s.add(&["v", "--env", "venv:.venv", "python x.py"]);
    s.bk()
        .args(["go", "v", "--print"])
        .assert()
        .success()
        .stdout(contains("&& . ").and(contains(".venv/bin/activate")));
    s.bk().args(["go", "v"]).assert().code(1).stderr(contains("shell hook"));
}

#[test]
fn init_zsh_script() {
    let s = Sandbox::new();
    s.bk().args(["init", "zsh"]).assert().success().stdout(contains("__BK_ZSH_LOADED").and(contains("__BK_RECORD=0")));
    s.bk().args(["init", "zsh", "--record"]).assert().success().stdout(contains("__BK_RECORD=1"));
}

#[test]
fn export_import_round_trip() {
    let s = Sandbox::new();
    s.add(&["a", "echo a", "-t", "x"]);
    s.add(&["b", "--step", "echo 1", "--step", "echo 2"]);
    let out = s.path().join("export.toml");
    s.bk().args(["export", "-o"]).arg(&out).assert().success();
    let json = s.bk().args(["export", "--json"]).assert().success().get_output().stdout.clone();
    std::fs::write(s.path().join("export.json"), json).unwrap();

    let t = Sandbox::new();
    t.bk().arg("import").arg(&out).assert().success().stderr(contains("imported 2"));
    t.bk().arg("import").arg(&out).assert().success().stderr(contains("2 skipped"));
    t.bk().arg("import").arg(s.path().join("export.json")).args(["--on-conflict", "rename"]).assert().success();
    t.bk().args(["ls", "--names"]).assert().stdout("a\na-2\nb\nb-2\n");
    t.bk().arg("b-2").assert().success().stdout("1\n2\n");
}

#[test]
fn untrusted_project_files_do_not_run() {
    let s = Sandbox::new();
    let proj = s.path().join("cloned");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join(".bk.toml"), "[bookmarks.evil]\ncommand = \"echo pwned\"\n").unwrap();
    s.bk().current_dir(&proj).arg("evil").assert().failure().stderr(contains("bk trust"));
    // --dry-run and --print are always allowed (they don't execute).
    s.bk().current_dir(&proj).args(["run", "evil", "--dry-run"]).assert().success();
    s.bk().current_dir(&proj).arg("trust").assert().success().stderr(contains("evil"));
    s.bk().current_dir(&proj).arg("evil").assert().success().stdout("pwned\n");
    // Changing the file behind bk's back revokes trust.
    std::fs::write(proj.join(".bk.toml"), "[bookmarks.evil]\ncommand = \"echo changed\"\n").unwrap();
    s.bk().current_dir(&proj).arg("evil").assert().failure();
}

#[test]
fn doctor_and_history() {
    let s = Sandbox::new();
    s.add(&["ok", "true"]);
    s.bk().arg("doctor").assert().success().stderr(contains("all good"));
    s.bk().arg("ok").assert().success();
    s.bk().arg("history").assert().success().stdout(contains("ok"));
    let gone = s.path().join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    s.add(&["broken", "ls", "--cwd", "gone"]);
    std::fs::remove_dir(&gone).unwrap();
    s.bk().arg("doctor").assert().code(1).stderr(contains("does not exist"));
}

#[test]
fn edit_with_scripted_editor() {
    let s = Sandbox::new();
    s.add(&["e", "echo before"]);
    // A fake $EDITOR that rewrites the command and renames the bookmark.
    let script = s.path().join("ed.sh");
    // (`sed -i` differs between BSD and GNU, so write a copy and move it back.)
    std::fs::write(
        &script,
        "#!/bin/sh\nsed -e 's/before/after/' -e 's/bookmarks.e]/bookmarks.e2]/' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"\n",
    )
    .unwrap();
    std::process::Command::new("chmod").arg("+x").arg(&script).status().unwrap();
    s.bk().args(["edit", "e"]).env("VISUAL", &script).assert().success();
    s.bk().arg("e2").assert().success().stdout("after\n");
}

/// A fake nvm tree whose `node` prints its version, so runs don't depend on real installs.
fn fake_nvm(root: &Path, versions: &[&str]) -> PathBuf {
    let nvm = root.join("nvm");
    for v in versions {
        let bin = nvm.join("versions/node").join(format!("v{v}")).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("node"), format!("#!/bin/sh\necho fake-node-{v}\n")).unwrap();
        std::process::Command::new("chmod").arg("+x").arg(bin.join("node")).status().unwrap();
    }
    nvm
}

#[test]
fn node_runtime_from_nvmrc_and_explicit() {
    let s = Sandbox::new();
    let nvm = fake_nvm(s.path(), &["18.20.1", "22.14.0"]);
    let app = s.path().join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();
    std::fs::write(app.join(".nvmrc"), "22\n").unwrap();
    std::fs::write(app.join("package.json"), "{}").unwrap();
    // `add` picks up .nvmrc as node=auto.
    s.bk()
        .current_dir(&app)
        .env("NVM_DIR", &nvm)
        .args(["add", "n", "node"])
        .assert()
        .success()
        .stderr(contains("node auto"));
    s.bk().env("NVM_DIR", &nvm).arg("n").assert().success().stdout("fake-node-22.14.0\n");
    // Explicit version wins; `none` removes a detected pin.
    s.bk().current_dir(&app).env("NVM_DIR", &nvm).args(["add", "old", "-r", "node=18", "node"]).assert().success();
    s.bk().env("NVM_DIR", &nvm).arg("old").assert().success().stdout("fake-node-18.20.1\n");
    s.bk().current_dir(&app).env("NVM_DIR", &nvm).args(["add", "sys", "-r", "node=none", "echo ok"]).assert().success();
    assert!(!std::fs::read_to_string(s.config()).unwrap().contains("[bookmarks.sys]\ncwd = \"app\"\nruntimes"));
    // Missing versions are reported clearly.
    s.bk().current_dir(&app).env("NVM_DIR", &nvm).args(["add", "bad", "-r", "node=16", "node"]).assert().success();
    s.bk().env("NVM_DIR", &nvm).arg("bad").assert().failure().stderr(contains("node 16 is not installed"));
    s.bk()
        .env("NVM_DIR", &nvm)
        .args(["add", "x", "-r", "cobol=85", "ls"])
        .assert()
        .failure()
        .stderr(contains("unknown runtime"));
}

#[test]
fn scan_imports_project_scripts() {
    let s = Sandbox::new();
    let web = s.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(
        web.join("package.json"),
        r#"{"scripts":{"hello":"echo hello from npm","build:prod":"echo build","postinstall":"echo hook"}}"#,
    )
    .unwrap();
    std::fs::write(web.join("Makefile"), "greet: ## say hi\n\techo hi from make\n").unwrap();
    s.bk()
        .args(["scan", "--list"])
        .arg(&web)
        .assert()
        .success()
        .stdout(contains("npm run hello").and(contains("make greet")));
    s.bk().args(["scan", "--yes"]).arg(&web).assert().success().stderr(contains("bookmarked 3 scripts"));
    s.bk().args(["ls", "--names"]).assert().stdout("web-build-prod\nweb-greet\nweb-hello\n");
    s.bk().arg("web-greet").assert().success().stdout(contains("hi from make"));
    // Into the project file, without a prefix.
    s.bk().args(["scan", "--yes", "--project"]).arg(&web).assert().success();
    s.bk().current_dir(&web).arg("greet").assert().success().stdout(contains("hi from make"));
    // Nothing new on a second run.
    s.bk().args(["scan", "--yes"]).arg(&web).assert().success().stderr(contains("Nothing imported"));
}

#[test]
fn extra_path_entries() {
    let s = Sandbox::new();
    let bin = s.path().join("tools");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("mytool"), "#!/bin/sh\necho mytool ran\n").unwrap();
    std::process::Command::new("chmod").arg("+x").arg(bin.join("mytool")).status().unwrap();
    s.add(&["t", "--path", "tools", "mytool"]);
    s.bk().arg("t").assert().success().stdout("mytool ran\n");
}
