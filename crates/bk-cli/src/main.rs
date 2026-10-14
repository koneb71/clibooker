mod cli;
mod commands;
mod exec;
mod folders_cmds;
mod manage;
mod remote_cmds;
mod shellcmds;
mod sync_cmds;

use std::process::ExitCode;

use clap::Parser;
use console::style;

use cli::{Cli, Cmd};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => ExitCode::from(bk_core::proc::normalize_code(code) as u8),
        Err(e) => {
            if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
                let _ = console::Term::stderr().show_cursor();
            }
            eprintln!("{} {e:#}", style("error:").red().bold());
            ExitCode::from(1)
        }
    }
}

fn dispatch(cmd: Option<Cmd>) -> anyhow::Result<i32> {
    // These don't need bookmarks loaded.
    match &cmd {
        Some(Cmd::Init { shell, record }) => return shellcmds::init(*shell, *record),
        Some(Cmd::App) => return manage::app(),
        Some(Cmd::RunSteps { file }) => return exec::run_steps_file(file),
        _ => {}
    }
    let ctx = commands::Ctx::new()?;
    match cmd {
        None => commands::ls(&ctx, &cli::LsArgs { tag: None, names: false, json: false }),
        Some(Cmd::Add(a)) => commands::add(&ctx, *a),
        Some(Cmd::Run(r)) => commands::run(&ctx, r),
        Some(Cmd::Ls(a)) => commands::ls(&ctx, &a),
        Some(Cmd::Show { name }) => commands::show(&ctx, &name),
        Some(Cmd::Rm { names, yes }) => commands::rm(&ctx, &names, yes),
        Some(Cmd::Mv { old, new }) => commands::mv(&ctx, &old, &new),
        Some(Cmd::Save(a)) => shellcmds::save(&ctx, a),
        Some(Cmd::Go { name, print, shell }) => shellcmds::go(&ctx, &name, print, shell),
        Some(Cmd::Edit { name, project }) => manage::edit(&ctx, name, project),
        Some(Cmd::History { name, limit, json }) => manage::history(&ctx, name.as_deref(), limit, json),
        Some(Cmd::Doctor) => manage::doctor(&ctx),
        Some(Cmd::Locate { name, dir }) => folders_cmds::locate(&ctx, &name, dir),
        Some(Cmd::Folders { cmd }) => folders_cmds::folders(&ctx, cmd),
        Some(Cmd::Trust { path, revoke }) => manage::trust_cmd(&ctx, path, revoke),
        Some(Cmd::Export { names, output, json }) => manage::export(&ctx, &names, output, json),
        Some(Cmd::Import { file, project, on_conflict }) => manage::import(&ctx, &file, project, on_conflict),
        Some(Cmd::Scan(a)) => manage::scan(&ctx, a),
        Some(Cmd::Server { cmd, check }) => remote_cmds::server(&ctx, cmd, check),
        Some(Cmd::Ssh { server, command }) => remote_cmds::ssh(&ctx, &server, &command),
        Some(Cmd::Sync { cmd }) => sync_cmds::sync(&ctx, cmd),
        Some(Cmd::Init { .. } | Cmd::App | Cmd::RunSteps { .. }) => unreachable!("handled above"),
        Some(Cmd::External(args)) => {
            // `bk train k=v -- extra` → `bk run train k=v -- extra`
            let argv = ["bk", "run"].into_iter().map(String::from).chain(args);
            match Cli::try_parse_from(argv) {
                Ok(Cli { cmd: Some(Cmd::Run(r)) }) => commands::run(&ctx, r),
                Ok(_) => unreachable!("re-parse always yields `run`"),
                Err(e) => e.exit(),
            }
        }
    }
}
