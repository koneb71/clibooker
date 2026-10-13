//! `edit`, `history`, `doctor`, `trust`, `export`, `import`, `app`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use bk_core::env::{conda, discover};
use bk_core::{Bookmark, doctor, history, paths, recent, store, trust};
use console::style;
use dialoguer::Confirm;

use crate::cli::OnConflict;
use crate::commands::{self, Ctx};

fn open_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("EDITOR").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "vi".into());
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("bk-edit")
        .arg(path)
        .status()
        .with_context(|| format!("starting editor `{editor}`"))?;
    if !status.success() {
        bail!("editor `{editor}` exited with {status}");
    }
    Ok(())
}

pub fn edit(ctx: &Ctx, name: Option<String>, project: bool) -> Result<i32> {
    let Some(name) = name else {
        let file = if project {
            ctx.project_file.clone().context("no .bk.toml in this folder or its parents")?
        } else {
            ctx.paths.config_file.clone()
        };
        trust::write_preserving(&ctx.paths, &file, || {
            store::touch(&file)?;
            open_editor(&file)
        })?;
        if let Err(e) = store::read_file(&file) {
            eprintln!("{} {e:#}", style("error:").red().bold());
            eprintln!("the file was saved, but bk can't read it until this is fixed (run `bk edit` again)");
            return Ok(1);
        }
        eprintln!("{} saved {}", style("✓").green().bold(), paths::contract_tilde(&file));
        return Ok(0);
    };

    let lib = ctx.library();
    let e = ctx.find(&lib, &name)?.clone();
    let tmp = tempfile::Builder::new().prefix(&format!("bk-{name}-")).suffix(".toml").tempfile()?;
    let header = format!(
        "# Editing `{name}` from {}.\n# Save and close to apply. Rename by changing the table name. Delete everything to cancel.\n\n",
        paths::contract_tilde(&e.file)
    );
    fs::write(tmp.path(), header + &store::render_bookmark(&name, &e.bookmark)?)?;

    loop {
        open_editor(tmp.path())?;
        let text = fs::read_to_string(tmp.path())?;
        if text.lines().all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')) {
            eprintln!("cancelled");
            return Ok(1);
        }
        let parsed = store::parse_str(&text).and_then(|f| {
            let mut books = f.bookmarks.into_iter();
            match (books.next(), books.next()) {
                (Some(one), None) => Ok(one),
                _ => bail!("expected exactly one [bookmarks.NAME] table"),
            }
        });
        let result = parsed.and_then(|(new_name, b)| {
            b.validate().map_err(anyhow::Error::msg)?;
            store::validate_name(&new_name)?;
            trust::write_preserving(&ctx.paths, &e.file, || {
                if new_name != name {
                    store::rename(&e.file, &name, &new_name)?;
                }
                store::upsert(&e.file, &new_name, &b)
            })?;
            Ok(new_name)
        });
        match result {
            Ok(n) => {
                eprintln!("{} saved {}", style("✓").green().bold(), style(n).bold());
                return Ok(0);
            }
            Err(err) => {
                eprintln!("{} {err:#}", style("error:").red().bold());
                if !commands::interactive()
                    || !Confirm::with_theme(&commands::theme()).with_prompt("Edit again?").default(true).interact()?
                {
                    return Ok(1);
                }
            }
        }
    }
}

pub fn history(ctx: &Ctx, name: Option<&str>, limit: usize, json: bool) -> Result<i32> {
    let records = history::read(&ctx.paths.history_file(), limit, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(0);
    }
    if records.is_empty() {
        eprintln!("No runs yet.");
        return Ok(0);
    }
    let now = history::now_ms();
    let width = console::Term::stdout().size_checked().map(|(_, w)| w as usize).unwrap_or(120);
    let name_w = records.iter().map(|r| r.name.len()).max().unwrap_or(4);
    for r in records {
        let mark = match r.exit_code {
            Some(0) => style("✓".to_string()).green(),
            Some(130) => style("■".to_string()).yellow(),
            Some(c) => style(format!("✗{c}")).red(),
            None => style("?".to_string()).yellow(),
        };
        let ago = commands::human_secs((now - r.started_ms) / 1000);
        let dur = format!("{:.1}s", r.duration_ms as f64 / 1000.0);
        let left = format!("{ago:>4} ago  {:<4} {:<name_w$}  {dur:>7}  ", "", r.name);
        let cmd = r.commands.join(" && ");
        let cmd = console::truncate_str(&cmd, width.saturating_sub(console::measure_text_width(&left)).max(12), "…");
        println!(
            "{}  {mark:<4} {}  {}  {}",
            style(format!("{ago:>4} ago")).dim(),
            style(format!("{:<name_w$}", r.name)).bold(),
            style(format!("{dur:>7}")).dim(),
            cmd
        );
    }
    Ok(0)
}

pub fn doctor(ctx: &Ctx) -> Result<i32> {
    let lib = ctx.library();
    let report = doctor::check_library(&lib, &ctx.env, &ctx.cwd, &ctx.paths);
    let ok = |s: &str| eprintln!("{} {s}", style("✓").green());
    let warn = |s: &str| eprintln!("{} {s}", style("!").yellow());
    let bad = |s: &str| eprintln!("{} {s}", style("✗").red());

    eprintln!("{}", style("setup").bold());
    ok(&format!("bookmarks file {}", paths::contract_tilde(&ctx.paths.config_file)));
    match std::env::var("BK_SHELL_INTEGRATION") {
        Ok(_) => ok("zsh hook loaded (bk save / bk go work)"),
        Err(_) => warn("zsh hook not loaded: add `eval \"$(bk init zsh)\"` to ~/.zshrc for bk save / bk go"),
    }
    let inv = discover::inventory(&ctx.env);
    match conda::find_base(&ctx.env) {
        Some(b) => ok(&format!("conda at {} ({} envs)", paths::contract_tilde(&b), inv.conda_envs.len())),
        None => warn("conda not found"),
    }
    let tools: Vec<&str> = [("uv", inv.tools.uv), ("poetry", inv.tools.poetry), ("pyenv", inv.tools.pyenv)]
        .iter()
        .filter(|(_, on)| *on)
        .map(|(n, _)| *n)
        .collect();
    if !tools.is_empty() {
        ok(&format!("found {}", tools.join(", ")));
    }
    let ssh = bk_core::servers::ssh_program(lib.settings.ssh.as_deref());
    match Command::new(&ssh).arg("-V").output() {
        Ok(o) => ok(&format!("{} ({ssh})", String::from_utf8_lossy(&o.stderr).trim())),
        Err(_) => warn(&format!("ssh not found at {ssh}")),
    }
    let remote_used = lib.entries.iter().any(|e| e.bookmark.is_remote());
    if remote_used || !lib.servers.is_empty() {
        match Command::new("ssh-add").arg("-l").output() {
            Ok(o) if o.status.success() => {
                let n = String::from_utf8_lossy(&o.stdout).lines().count();
                ok(&format!("ssh-agent has {n} key{}", if n == 1 { "" } else { "s" }))
            }
            _ => warn("ssh-agent has no keys loaded (ssh-add), so background checks and env discovery may fail"),
        }
    }
    if let Some(pf) = &ctx.project_file {
        if trust::is_trusted(&ctx.paths, pf) {
            ok(&format!("project file {} is trusted", paths::contract_tilde(pf)));
        } else {
            warn(&format!("project file {} is not trusted yet (`bk trust`)", paths::contract_tilde(pf)));
        }
    }
    let _ = recent::trim(&ctx.paths.recent_file());

    eprintln!("\n{}", style(format!("bookmarks ({})", report.checks.len())).bold());
    for e in &report.file_errors {
        bad(e);
    }
    for c in &report.checks {
        if c.problems.is_empty() && c.warnings.is_empty() {
            ok(&format!("{} {}", c.name, style(&c.env).dim()));
            continue;
        }
        if c.problems.is_empty() {
            warn(&format!("{} {}", c.name, style(&c.env).dim()));
        } else {
            bad(&format!("{} {}", c.name, style(&c.env).dim()));
        }
        for p in &c.problems {
            eprintln!("    {}", style(p).red());
        }
        for w in &c.warnings {
            eprintln!("    {}", style(w).yellow());
        }
    }
    let n = report.problem_count();
    if n > 0 {
        eprintln!("\n{} {n} problem{}", style("✗").red().bold(), if n == 1 { "" } else { "s" });
        Ok(1)
    } else {
        eprintln!("\n{} all good", style("✓").green().bold());
        Ok(0)
    }
}

pub fn trust_cmd(ctx: &Ctx, path: Option<PathBuf>, revoke: bool) -> Result<i32> {
    let file = match path {
        Some(p) if p.is_dir() => p.join(paths::PROJECT_FILE),
        Some(p) => p,
        None => ctx.project_file.clone().context("no .bk.toml in this folder or its parents")?,
    };
    let file = fs::canonicalize(&file).with_context(|| format!("{} not found", file.display()))?;
    let shown = paths::contract_tilde(&file);
    if revoke {
        trust::untrust(&ctx.paths, &file)?;
        eprintln!("{} {shown} is no longer trusted", style("✓").green().bold());
        return Ok(0);
    }
    let f = store::read_file(&file)?;
    eprintln!("{shown} defines {} bookmark{}:", f.bookmarks.len(), if f.bookmarks.len() == 1 { "" } else { "s" });
    for (name, b) in &f.bookmarks {
        eprintln!("  {}  {}", style(name).bold(), style(b.summary()).cyan());
    }
    trust::trust(&ctx.paths, &file)?;
    bk_core::projects::register(&ctx.paths, &file)?;
    eprintln!("{} trusted", style("✓").green().bold());
    Ok(0)
}

pub fn export(ctx: &Ctx, names: &[String], output: Option<PathBuf>, json: bool) -> Result<i32> {
    let lib = ctx.library();
    let entries: Vec<_> = if names.is_empty() {
        lib.entries.iter().filter(|e| !lib.is_shadowed(e)).collect()
    } else {
        names.iter().map(|n| ctx.find(&lib, n)).collect::<Result<_>>()?
    };
    let map: BTreeMap<String, Bookmark> = entries.iter().map(|e| (e.name.clone(), e.bookmark.clone())).collect();
    let text = if json {
        serde_json::to_string_pretty(&map)? + "\n"
    } else {
        let mut out = String::from("# Exported by bk. Import with: bk import FILE\n\n");
        for (n, b) in &map {
            out.push_str(&store::render_bookmark(n, b)?);
            out.push('\n');
        }
        out
    };
    match output {
        Some(p) => {
            fs::write(&p, text)?;
            eprintln!("{} exported {} bookmarks to {}", style("✓").green().bold(), map.len(), p.display());
        }
        None => print!("{text}"),
    }
    Ok(0)
}

pub fn import(ctx: &Ctx, file: &Path, project: bool, on_conflict: OnConflict) -> Result<i32> {
    let text = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let incoming: BTreeMap<String, Bookmark> =
        if file.extension().is_some_and(|e| e == "json") || text.trim_start().starts_with('{') {
            serde_json::from_str(&text).context("parsing JSON export")?
        } else {
            store::parse_str(&text)?.bookmarks
        };
    let (target, _) = commands::target_file(ctx, project);
    let existing = store::read_file(&target)?.bookmarks;
    let (mut added, mut skipped) = (0, 0);
    trust::write_preserving(&ctx.paths, &target, || {
        for (name, b) in &incoming {
            let mut dest = name.clone();
            if existing.contains_key(name) {
                match on_conflict {
                    OnConflict::Skip => {
                        skipped += 1;
                        continue;
                    }
                    OnConflict::Overwrite => {}
                    OnConflict::Rename => {
                        let mut i = 2;
                        while existing.contains_key(&format!("{name}-{i}"))
                            || incoming.contains_key(&format!("{name}-{i}"))
                        {
                            i += 1;
                        }
                        dest = format!("{name}-{i}");
                    }
                }
            }
            store::upsert(&target, &dest, b).with_context(|| format!("importing `{name}`"))?;
            added += 1;
        }
        Ok(())
    })?;
    if project {
        bk_core::projects::register(&ctx.paths, &target)?;
    }
    eprintln!(
        "{} imported {added} bookmark{} into {}{}",
        style("✓").green().bold(),
        if added == 1 { "" } else { "s" },
        paths::contract_tilde(&target),
        if skipped > 0 { format!(" ({skipped} skipped: already exist; use --on-conflict)") } else { String::new() }
    );
    Ok(0)
}

pub fn app() -> Result<i32> {
    let status = Command::new("/usr/bin/open").args(["-b", "com.clibooker.app"]).status()?;
    if !status.success() {
        bail!("Clibooker.app is not installed (build it with `pnpm -C app tauri build`)");
    }
    Ok(0)
}

pub fn scan(ctx: &Ctx, a: crate::cli::ScanArgs) -> Result<i32> {
    use bk_core::scripts;

    if let Some(spec) = &a.dir {
        let lib = ctx.library();
        if let Some((server, path)) = bk_core::remote::split_server_path(spec, &lib.servers) {
            return scan_remote(ctx, &lib, server, path, &a);
        }
    }
    let dir = match &a.dir {
        Some(d) => fs::canonicalize(d).with_context(|| format!("{d} not found"))?,
        None => ctx.cwd.clone(),
    };
    let found = scripts::discover(&dir);
    if found.is_empty() {
        eprintln!(
            "No scripts found in {} (looked for package.json, Makefile, justfile, composer.json, deno.json, pyproject.toml, Taskfile.yml, Procfile)",
            paths::contract_tilde(&dir)
        );
        return Ok(1);
    }
    if a.list {
        let w = found.iter().map(|s| s.suggested_name.len()).max().unwrap_or(4);
        for s in &found {
            println!(
                "{:<w$}  {}  {}",
                s.suggested_name,
                style(&s.command).cyan(),
                style(format!("{} {}", s.source, s.detail.as_deref().unwrap_or(""))).dim()
            );
        }
        return Ok(0);
    }

    let (file, project_dir) = if a.project {
        let f = paths::find_project_file(&dir).unwrap_or_else(|| dir.join(paths::PROJECT_FILE));
        let d = f.parent().map(Path::to_path_buf);
        (f, d)
    } else {
        (ctx.paths.config_file.clone(), None)
    };
    let prefix = match (&a.prefix, a.project) {
        (Some(p), _) => Some(scripts::sanitize_name(p)),
        (None, true) => None,
        (None, false) => dir.file_name().map(|n| scripts::sanitize_name(&n.to_string_lossy())),
    };
    let name_for = |s: &scripts::ProjectScript| match &prefix {
        Some(p) => format!("{p}-{}", s.suggested_name),
        None => s.suggested_name.clone(),
    };
    let existing = store::read_file(&file)?.bookmarks;

    let chosen: Vec<usize> = if a.yes || !commands::interactive() {
        (0..found.len()).filter(|i| !existing.contains_key(&name_for(&found[*i]))).collect()
    } else {
        let labels: Vec<String> = found
            .iter()
            .map(|s| {
                let exists = if existing.contains_key(&name_for(s)) { "  (exists, will be replaced)" } else { "" };
                format!("{:<24} {}{}", name_for(s), style(&s.command).cyan(), style(exists).yellow())
            })
            .collect();
        let defaults: Vec<bool> = found.iter().map(|s| !existing.contains_key(&name_for(s))).collect();
        dialoguer::MultiSelect::with_theme(&commands::theme())
            .with_prompt(format!(
                "Bookmark which scripts from {}? (space toggles, enter confirms)",
                paths::contract_tilde(&dir)
            ))
            .items(&labels)
            .defaults(&defaults)
            .interact()?
    };
    if chosen.is_empty() {
        eprintln!("Nothing imported.");
        return Ok(0);
    }

    let env_spec = bk_core::env::detect::from_folder(&dir, &ctx.env).unwrap_or_default();
    // Scanning another folder shouldn't pick up `nvm use` from the calling shell.
    let runtimes = commands::choose_runtimes(&[], &bk_core::env::EnvMap::new(), &dir, &env_spec)?;
    let cwd = commands::stored_dir(&dir, project_dir.as_deref());
    for i in &chosen {
        let s = &found[*i];
        let b = Bookmark {
            description: Some(match &s.detail {
                Some(d) => format!("{} · {d}", s.source),
                None => s.source.clone(),
            }),
            cwd: Some(cwd.clone()),
            env: env_spec.clone(),
            runtimes: runtimes.clone(),
            command: Some(s.command.clone()),
            tags: vec![dir.file_name().map(|n| scripts::sanitize_name(&n.to_string_lossy())).unwrap_or_default()]
                .into_iter()
                .filter(|t| !t.is_empty() && !a.project)
                .collect(),
            ..Default::default()
        };
        commands::persist(ctx, &file, &name_for(s), a.project, &b)?;
    }
    eprintln!(
        "{} bookmarked {} script{} from {} ({}{})",
        style("✓").green().bold(),
        chosen.len(),
        if chosen.len() == 1 { "" } else { "s" },
        paths::contract_tilde(&dir),
        env_spec.label(),
        runtimes.iter().map(|(l, v)| format!(", {l} {v}")).collect::<String>()
    );
    for i in &chosen {
        eprintln!("  bk {}", name_for(&found[*i]));
    }
    Ok(0)
}

fn scan_remote(ctx: &Ctx, lib: &bk_core::Library, server: &str, path: &str, a: &crate::cli::ScanArgs) -> Result<i32> {
    use bk_core::scripts;
    let target = bk_core::servers::resolve_target(server, &lib.servers)?;
    let b = bk_core::servers::Batch {
        program: crate::remote_cmds::ssh_program(lib),
        target: &target,
        paths: &ctx.paths,
        env: bk_core::servers::ssh_env(&ctx.env),
    };
    let found = bk_core::remote::scan_scripts(&b, path)?;
    if found.scripts.is_empty() {
        eprintln!("No scripts found in {path} on {server}");
        return Ok(1);
    }
    if a.list {
        for s in &found.scripts {
            println!("{:<20}  {}  {}", s.suggested_name, style(&s.command).cyan(), style(&s.source).dim());
        }
        return Ok(0);
    }
    if a.project {
        bail!("remote scripts are saved as global bookmarks (drop --project)");
    }
    let folder_name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("project").trim_start_matches('~');
    let prefix = a
        .prefix
        .clone()
        .unwrap_or_else(|| scripts::sanitize_name(if folder_name.is_empty() { server } else { folder_name }));
    let file = ctx.paths.config_file.clone();
    let existing = store::read_file(&file)?.bookmarks;
    let name_for = |s: &scripts::ProjectScript| format!("{prefix}-{}", s.suggested_name);
    let chosen: Vec<usize> = if a.yes || !commands::interactive() {
        (0..found.scripts.len()).filter(|i| !existing.contains_key(&name_for(&found.scripts[*i]))).collect()
    } else {
        let labels: Vec<String> =
            found.scripts.iter().map(|s| format!("{:<24} {}", name_for(s), style(&s.command).cyan())).collect();
        let defaults: Vec<bool> = found.scripts.iter().map(|s| !existing.contains_key(&name_for(s))).collect();
        dialoguer::MultiSelect::with_theme(&commands::theme())
            .with_prompt(format!("Bookmark which scripts from {server}:{path}?"))
            .items(&labels)
            .defaults(&defaults)
            .interact()?
    };
    for i in &chosen {
        let s = &found.scripts[*i];
        let b = Bookmark {
            description: Some(match &s.detail {
                Some(d) => format!("{} · {d}", s.source),
                None => s.source.clone(),
            }),
            server: Some(server.to_string()),
            cwd: Some(path.to_string()),
            env: found.env.clone(),
            runtimes: found.runtimes.clone(),
            command: Some(s.command.clone()),
            tags: vec![prefix.clone()],
            ..Default::default()
        };
        commands::persist(ctx, &file, &name_for(s), false, &b)?;
    }
    eprintln!(
        "{} bookmarked {} script{} from {server}:{path} ({})",
        style("✓").green().bold(),
        chosen.len(),
        if chosen.len() == 1 { "" } else { "s" },
        found.env.label()
    );
    Ok(0)
}
