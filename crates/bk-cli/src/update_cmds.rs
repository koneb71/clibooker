//! `bk update`: replace a standalone bk with the newest release, once its signature checks out
//! against Clibooker's release key. A bk that came with the app, a package or cargo is updated
//! the way it was installed.

use std::time::Duration;

use anyhow::Result;
use bk_core::paths;
use bk_core::update::{self, InstallKind};
use console::style;

/// How to update a bk that doesn't replace itself.
pub fn elsewhere(kind: InstallKind) -> Option<String> {
    match kind {
        InstallKind::Standalone => None,
        InstallKind::App => Some(format!(
            "this bk comes with the Clibooker app: update the app (Settings → Updates, or {}/latest)",
            update::releases_base()
        )),
        InstallKind::Cargo => Some("this bk was installed with cargo: run the same `cargo install` again".into()),
    }
}

pub fn update(check: bool) -> Result<i32> {
    let current = update::current();
    let latest = update::latest(Duration::from_secs(20))?;
    if latest <= current {
        eprintln!("{} bk {current} is the newest", style("✓").green().bold());
        return Ok(0);
    }
    eprintln!("{} bk {latest} is out (this is {current})", style("↑").cyan().bold());
    let exe = std::env::current_exe()?;
    if let Some(how) = elsewhere(update::install_kind(&exe)) {
        eprintln!("  {how}");
        return Ok(if check { 0 } else { 1 });
    }
    if check {
        eprintln!("  update: {}", style("bk update").cyan());
        return Ok(0);
    }
    let binary = update::download(&latest)?;
    let path = update::replace_self(&binary)?;
    eprintln!(
        "{} bk {current} → {latest} {}",
        style("✓").green().bold(),
        style(format!("({})", paths::contract_tilde(&path))).dim()
    );
    Ok(0)
}
