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
