//! `bk sync` end to end: two sandboxed devices syncing through the fake Telegram Bot API.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use bk_core::sync::fake::{FakeTelegram, TOKEN};
use predicates::prelude::*;
use predicates::str::contains;

struct Device {
    dir: PathBuf,
    api: String,
}

impl Device {
    fn new(root: &Path, name: &str, api: &str) -> Device {
        Device { dir: root.join(name), api: api.to_string() }
    }

    fn bk(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bk"));
        c.env("BK_CONFIG", self.dir.join("bookmarks.toml"))
            .env("BK_DATA_DIR", self.dir.join("data"))
            .env("BK_TELEGRAM_API", &self.api)
            .env("BK_SYNC_PASSPHRASE", "correct horse")
            // Background syncs are tested on their own (`automatic_sync_in_the_background`).
            .env("BK_NO_AUTO_SYNC", "1")
            .env("NO_COLOR", "1")
            .current_dir(std::env::temp_dir());
        c
    }

    fn names(&self) -> String {
        String::from_utf8(self.bk().args(["ls", "--names"]).output().unwrap().stdout).unwrap()
    }
}

#[test]
fn setup_sync_status_and_forget() {
    let tg = FakeTelegram::start();
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (Device::new(root.path(), "a", &tg.url()), Device::new(root.path(), "b", &tg.url()));

    a.bk().args(["sync"]).assert().failure().stderr(contains("bk sync setup"));
    a.bk().args(["add", "hello", "echo hi"]).assert().success();
    tg.press_start();
    a.bk()
        .args(["sync", "setup", "--token", TOKEN, "--device", "laptop"])
        .assert()
        .success()
        .stderr(contains("laptop is set up").and(contains("uploaded").or(contains("sent"))));

    // Setting up again needs --force; a wrong passphrase is refused.
    a.bk().args(["sync", "setup", "--token", TOKEN]).assert().failure().stderr(contains("--force"));
    tg.press_start();
    b.bk()
        .args(["sync", "setup", "--token", TOKEN, "--device", "desktop"])
        .env("BK_SYNC_PASSPHRASE", "wrong horse!")
        .assert()
        .failure()
        .stderr(contains("wrong passphrase"));

    b.bk().args(["add", "other", "make"]).assert().success();
    tg.press_start();
    b.bk()
        .args(["sync", "setup", "--token", TOKEN, "--device", "desktop", "--yes"])
        .assert()
        .success()
        .stderr(contains("from laptop"));
    assert_eq!(b.names(), "hello\nother\n");
    a.bk().arg("sync").assert().success().stderr(contains("↓ 1 updated from desktop"));
    assert_eq!(a.names(), "hello\nother\n");
    a.bk().arg("sync").assert().success().stderr(contains("already up to date"));

    a.bk()
        .args(["sync", "status"])
        .assert()
        .success()
        .stdout(contains("device    laptop").and(contains("@clibooker_test_bot")).and(contains("from desktop")));
    // Bookmarks can't take the subcommand's name.
    a.bk().args(["add", "sync", "ls"]).assert().failure();

    b.bk().args(["sync", "forget"]).assert().success();
    b.bk().args(["sync", "status"]).assert().code(1).stderr(contains("isn't set up"));
}

fn set_up(tg: &FakeTelegram, d: &Device, name: &str) {
    tg.press_start();
    d.bk().args(["sync", "setup", "--token", TOKEN, "--device", name, "--yes"]).assert().success();
}

/// Running a bookmark another device just made, devices in the status, runs that say where
/// they ran, and turning automatic syncing off.
#[test]
fn sync_on_demand_devices_and_auto() {
    let tg = FakeTelegram::start();
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (Device::new(root.path(), "a", &tg.url()), Device::new(root.path(), "b", &tg.url()));
    a.bk().args(["add", "hello", "echo hi", "--no-cwd"]).assert().success();
    set_up(&tg, &a, "laptop");
    set_up(&tg, &b, "desktop");

    // Made on the laptop after the desktop last synced: `bk NAME` fetches it.
    a.bk().args(["add", "greet", "echo greetings", "--no-cwd"]).assert().success();
    a.bk().arg("sync").assert().success();
    b.bk().arg("greet").assert().success().stdout(contains("greetings")).stderr(contains("isn't here yet; syncing"));

    b.bk().arg("hello").assert().success();
    b.bk().arg("sync").assert().success();
    a.bk().arg("sync").assert().success();
    a.bk().arg("history").assert().success().stdout(contains("@desktop"));
    a.bk()
        .args(["sync", "status"])
        .assert()
        .success()
        .stdout(contains("auto      on").and(contains("laptop").and(contains("(this one)"))).and(contains("desktop")));

    b.bk().args(["sync", "auto", "off"]).assert().success().stderr(contains("only when you run `bk sync`"));
    a.bk().args(["add", "later", "echo later", "--no-cwd"]).assert().success();
    a.bk().arg("sync").assert().success();
    b.bk().arg("later").assert().failure().stderr(contains("isn't here yet").not());
    b.bk().args(["sync", "status"]).assert().stdout(contains("auto      off"));
}

/// With nothing opted out, a change is sent by a detached `bk sync --background`.
#[test]
fn automatic_sync_in_the_background() {
    let tg = FakeTelegram::start();
    let root = tempfile::tempdir().unwrap();
    let a = Device::new(root.path(), "a", &tg.url());
    set_up(&tg, &a, "laptop");
    let state = a.dir.join("data/sync/state.json");
    let synced_at = |p: &Path| -> Option<i64> {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        v["last_sync_ms"].as_i64()
    };
    let before = synced_at(&state).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5_100));
    a.bk().args(["add", "hello", "echo hi", "--no-cwd"]).env_remove("BK_NO_AUTO_SYNC").assert().success();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while synced_at(&state) == Some(before) {
        assert!(std::time::Instant::now() < deadline, "the background sync never ran");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Let the background process finish writing before the folder goes away.
    std::thread::sleep(std::time::Duration::from_millis(300));
    a.bk().args(["sync", "status"]).assert().success().stdout(contains("to send").not());
}

/// Saved SSH keys reach the other device with the servers that use them, and deleting one
/// deletes it there too.
#[test]
fn saved_keys_travel() {
    let keygen = bk_core::keys::tool(&bk_core::servers::ssh_program(None), "ssh-keygen");
    if std::process::Command::new(&keygen).arg("-?").output().is_err() {
        eprintln!("skipping: no ssh-keygen");
        return;
    }
    let tg = FakeTelegram::start();
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (Device::new(root.path(), "a", &tg.url()), Device::new(root.path(), "b", &tg.url()));
    set_up(&tg, &a, "laptop");
    set_up(&tg, &b, "desktop");
    a.bk()
        .args(["keys", "new", "lab", "--no-passphrase"])
        .assert()
        .success()
        .stderr(contains("protected by your sync passphrase alone"));
    a.bk().args(["server", "add", "gpu", "me@10.0.0.5", "-i", "key:lab"]).assert().success();
    a.bk().arg("sync").assert().success();
    b.bk().arg("sync").assert().success();
    b.bk().arg("keys").assert().success().stdout(contains("lab").and(contains("used by gpu")));
    let key = |d: &Device| std::fs::read_to_string(d.dir.join("data").join("keys").join("lab")).ok();
    assert!(key(&b).is_some() && key(&b) == key(&a));

    b.bk().args(["keys", "rm", "lab", "--yes"]).assert().success();
    b.bk().arg("sync").assert().success();
    a.bk().arg("sync").assert().success();
    assert_eq!(key(&a), None);
}
