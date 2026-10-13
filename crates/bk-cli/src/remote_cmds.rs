//! SSH: `bk server …`, `bk ssh`, and running / entering remote bookmarks.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use bk_core::plan::RunRequest;
use bk_core::remote::{self, RemoteInputs, RemotePlan};
use bk_core::render::Dialect;
use bk_core::servers::{self, Batch, Reach};
use bk_core::{Entry, Library, Server, history, render, store};
use console::style;

use crate::cli::{ServerAddArgs, ServerCmd};
use crate::commands::{self, Ctx};
use crate::exec;

pub fn ssh_program(lib: &Library) -> String {
    servers::ssh_program(lib.settings.ssh.as_deref())
}

pub fn inputs<'a>(ctx: &'a Ctx, lib: &'a Library, online: bool) -> RemoteInputs<'a> {
    RemoteInputs {
        servers: &lib.servers,
        paths: &ctx.paths,
        ssh_program: ssh_program(lib),
        base_env: &ctx.env,
        online,
        markers: false,
    }
}

/// Shell-quoted command line, for `--print` (in this OS's shell language).
pub fn command_line(program: &str, args: &[String]) -> String {
    render::command_line(Dialect::native(), program, args)
}

/// Hand the terminal to ssh (interactive sessions).
fn exec_ssh(program: &str, args: &[String], env: &bk_core::env::EnvMap) -> Result<i32> {
    exec::replace_process(program, args, env)
}

pub fn print_plan(plan: &RemotePlan) {
    let s = plan.summary();
    let label = |t: &str| style(format!("  {t:<7}")).dim().to_string();
    eprintln!(
        "{} {} {}",
        style("plan").bold(),
        style(&s.name).bold(),
        style(format!("(on {})", plan.target.name)).dim()
    );
    eprintln!("{}{}", label("cwd"), s.cwd);
    eprintln!("{}{}", label("env"), s.env_description);
    eprintln!("{}{}", label("ssh"), plan.target.destination);
    for f in &plan.forwards {
        eprintln!("{}localhost:{} → {}", label("forward"), servers::forward_local_port(f).unwrap_or(0), f);
    }
    for (i, st) in s.steps.iter().enumerate() {
        eprintln!("{}{}", label(&format!("{}.", i + 1)), style(&st.command).cyan());
    }
    for n in &plan.notes {
        eprintln!("{}{}", label("note"), style(n).yellow());
    }
}

/// `bk run` for a bookmark with a `server`.
pub fn run(
    ctx: &Ctx,
    lib: &Library,
    e: &Entry,
    req: &RunRequest,
    dry_run: bool,
    print: bool,
    confirm: bool,
) -> Result<i32> {
    let online = !dry_run && !print;
    let plan = remote::build(e, req, &inputs(ctx, lib, online))?;
    if print {
        println!("{}", command_line(&plan.program, &plan.args_with_forwards()));
        return Ok(0);
    }
    if dry_run || confirm {
        print_plan(&plan);
    }
    if dry_run {
        return Ok(0);
    }
    if confirm {
        if !commands::interactive() {
            bail!("`{}` asks for confirmation; pass --yes to run non-interactively", e.name);
        }
        if !dialoguer::Confirm::with_theme(&commands::theme()).with_prompt("Run it?").default(true).interact()? {
            return Ok(1);
        }
    }
    for n in &plan.notes {
        eprintln!("{} {n}", style("note:").yellow());
    }
    if plan.detach {
        eprintln!(
            "{} `detach` (keep running in tmux) is used by the desktop app; the CLI runs it attached",
            style("note:").yellow()
        );
    }
    let started = history::now_ms();
    let outcome = exec::run_remote(&plan)?;
    if lib.settings.history_enabled() {
        let mut rec = history::RunRecord::from_remote(&plan, "cli", started);
        rec.duration_ms = (history::now_ms() - started).max(0) as u64;
        rec.exit_code = Some(outcome.code);
        if let Err(err) = history::append(&ctx.paths.history_file(), &rec) {
            eprintln!("{} could not write history: {err:#}", style("warning:").yellow());
        }
    }
    if outcome.interrupted {
        exec::reraise_sigint();
    }
    Ok(outcome.code)
}

/// `bk go` for a remote bookmark: an interactive shell on the server in its folder and env.
pub fn go(ctx: &Ctx, lib: &Library, e: &Entry, print: bool, dialect: Dialect) -> Result<i32> {
    let inp = inputs(ctx, lib, false);
    let (_, args) = remote::shell_here(e, &inp)?;
    let program = ssh_program(lib);
    if print {
        println!("{}", render::command_line(dialect, &program, &args));
        return Ok(0);
    }
    exec_ssh(&program, &args, &servers::ssh_env(&ctx.env))
}

/// `bk ssh SERVER [-- command]`.
pub fn ssh(ctx: &Ctx, name: &str, command: &[String]) -> Result<i32> {
    let lib = ctx.library();
    let target = servers::resolve_target(name, &lib.servers)?;
    let mut args = Vec::new();
    for f in &target.forward {
        args.push("-L".to_string());
        args.push(f.clone());
    }
    args.extend(remote::session_args(&target, &ctx.paths));
    if !command.is_empty() {
        args.push(command.iter().map(|a| render::q(a)).collect::<Vec<_>>().join(" "));
    }
    exec_ssh(&ssh_program(&lib), &args, &servers::ssh_env(&ctx.env))
}

fn reach_text(r: &Reach) -> String {
    match r {
        Reach::Reachable { ms } => style(format!("● reachable ({ms} ms)")).green().to_string(),
        Reach::AuthRequired => style("● needs a password / key (connect once with `bk ssh`)").yellow().to_string(),
        Reach::UnknownHostKey => style("● host key not trusted yet (connect once with `bk ssh`)").yellow().to_string(),
        Reach::ChangedHostKey => style("● HOST KEY CHANGED — check ~/.ssh/known_hosts").red().bold().to_string(),
        Reach::Unreachable { reason } => style(format!("● unreachable: {reason}")).red().to_string(),
    }
}

pub fn server(ctx: &Ctx, cmd: Option<ServerCmd>, check: bool) -> Result<i32> {
    match cmd.unwrap_or(ServerCmd::Ls { check }) {
        ServerCmd::Add(a) => add(ctx, a),
        ServerCmd::Rm { name } => remove(ctx, &name),
        ServerCmd::Unhide { name } => {
            if !store::set_server_hidden(&ctx.paths.config_file, &name, false)? {
                bail!("`{name}` isn't hidden");
            }
            eprintln!("{} {name} is listed again", style("✓").green().bold());
            Ok(0)
        }
        ServerCmd::Ls { check: c } => list(ctx, c || check),
        ServerCmd::Scan { name } => scan(ctx, &name),
    }
}

/// Delete a Clibooker server, or hide an ~/.ssh/config host (that file is never edited).
fn remove(ctx: &Ctx, name: &str) -> Result<i32> {
    let in_config = servers::ssh_config_hosts().iter().any(|h| h == name);
    if store::remove_server(&ctx.paths.config_file, name)? {
        servers::forget_cached(&ctx.paths, name);
        eprintln!("{} deleted server {name}", style("✓").green().bold());
        if in_config {
            eprintln!("  `{name}` from ~/.ssh/config is listed again; run this again to hide it too");
        }
    } else if in_config {
        store::set_server_hidden(&ctx.paths.config_file, name, true)?;
        eprintln!("{} {name} is hidden from Clibooker's lists", style("✓").green().bold());
        eprintln!(
            "  It stays in ~/.ssh/config, so `bk ssh {name}` and its bookmarks keep working. Undo: {}",
            style(format!("bk server unhide {name}")).cyan()
        );
    } else {
        bail!("no server named `{name}`");
    }
    let used = ctx.library().entries.iter().filter(|e| e.bookmark.server.as_deref() == Some(name)).count();
    if used > 0 {
        eprintln!("  {} {used} bookmark(s) run on {name}", style("note:").yellow());
    }
    Ok(0)
}

fn add(ctx: &Ctx, a: ServerAddArgs) -> Result<i32> {
    let existing = store::read_file(&ctx.paths.config_file)?;
    if existing.servers.contains_key(&a.name) && !a.force {
        bail!("server `{}` already exists (use --force to overwrite)", a.name);
    }
    let (user, host) = match a.destination.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h.to_string()),
        None => (None, a.destination.clone()),
    };
    let options = a.options.iter().map(|o| commands::parse_kv(o)).collect::<Result<BTreeMap<_, _>>>()?;
    let server = Server {
        host,
        user,
        port: a.port,
        identity: a.identity,
        jump: a.jump,
        forward: a.forward,
        options,
        description: a.description,
        shell: a.shell,
        login: None,
    };
    store::upsert_server(&ctx.paths.config_file, &a.name, &server)?;
    eprintln!("{} saved server {}", style("✓").green().bold(), style(&a.name).bold());
    eprintln!(
        "  connect: {}   check: {}",
        style(format!("bk ssh {}", a.name)).cyan(),
        style("bk servers --check").cyan()
    );
    Ok(0)
}

fn list(ctx: &Ctx, check: bool) -> Result<i32> {
    let lib = ctx.library();
    let config_hosts = servers::listed_config_hosts(&lib.servers, &lib.settings.hidden_servers);
    let hidden = &lib.settings.hidden_servers;
    let hidden_note = || {
        if !hidden.is_empty() {
            eprintln!("{}", style(format!("hidden: {} (bk server unhide NAME)", hidden.join(", "))).dim());
        }
    };
    if lib.servers.is_empty() && config_hosts.is_empty() {
        eprintln!("No servers yet. Add one with `bk server add NAME user@host`.");
        hidden_note();
        return Ok(0);
    }
    let mut rows: Vec<(String, String, String)> = lib
        .servers
        .iter()
        .map(|(n, s)| {
            let dest = format!(
                "{}{}{}",
                s.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(),
                s.host,
                s.port.map(|p| format!(":{p}")).unwrap_or_default()
            );
            (n.clone(), dest, s.description.clone().unwrap_or_default())
        })
        .collect();
    rows.extend(config_hosts.into_iter().map(|h| (h.clone(), h, "~/.ssh/config".to_string())));
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(4);
    let statuses: Vec<Option<Reach>> = if check {
        let program = ssh_program(&lib);
        let env = servers::ssh_env(&ctx.env);
        std::thread::scope(|s| {
            let handles: Vec<_> = rows
                .iter()
                .map(|(name, _, _)| {
                    let (program, env, lib) = (&program, &env, &lib);
                    s.spawn(move || {
                        let target = servers::resolve_target(name, &lib.servers).ok()?;
                        let b =
                            Batch { program: program.clone(), target: &target, paths: &ctx.paths, env: env.clone() };
                        Some(servers::check(&b))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().ok().flatten()).collect()
        })
    } else {
        vec![None; rows.len()]
    };
    for ((name, dest, about), status) in rows.iter().zip(statuses) {
        let status = status.map(|r| format!("  {}", reach_text(&r))).unwrap_or_default();
        println!("{}  {}  {}{}", style(format!("{name:<w$}")).bold(), dest, style(about).dim(), status);
    }
    hidden_note();
    Ok(0)
}

fn scan(ctx: &Ctx, name: &str) -> Result<i32> {
    let lib = ctx.library();
    let target = servers::resolve_target(name, &lib.servers)?;
    let b = Batch { program: ssh_program(&lib), target: &target, paths: &ctx.paths, env: servers::ssh_env(&ctx.env) };
    let inv = servers::inventory(&b, true)?;
    eprintln!("{} {} {}", style("✓").green().bold(), style(&target.name).bold(), style(&inv.os).dim());
    eprintln!("  shell   {}", inv.shell);
    eprintln!("  tools   {}", inv.tools.join(", "));
    if let Some(base) = &inv.conda_base {
        let envs: Vec<&str> = inv.conda_envs.iter().map(|e| e.name.as_str()).collect();
        eprintln!("  conda   {base}: {}", envs.join(", "));
    }
    for (lang, installs) in &inv.runtimes {
        let v: Vec<&str> = installs.iter().map(|i| i.version.as_str()).collect();
        eprintln!("  {lang:<7} {}", v.join(", "));
    }
    Ok(0)
}
