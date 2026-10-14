use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bk_core::env::{self, EnvMap, Mode, detect, runtimes};
use bk_core::folders::Located;
use bk_core::plan::{self, RunRequest};
use bk_core::{
    Bookmark, Entry, EnvSpec, Library, Os, Paths, Scope, Step, history, paths, projects, render, store, template, trust,
};
use console::{Term, style};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input};

use crate::cli::{AddArgs, LsArgs, RunArgs};
use crate::exec;

pub struct Ctx {
    pub paths: Paths,
    pub cwd: PathBuf,
    pub env: EnvMap,
    pub project_file: Option<PathBuf>,
}

impl Ctx {
    pub fn new() -> Result<Self> {
        let cwd = std::env::current_dir().context("reading the current directory")?;
        Ok(Ctx { paths: Paths::from_env(), project_file: paths::find_project_file(&cwd), cwd, env: env::current() })
    }

    pub fn library(&self) -> Library {
        // Let the desktop app discover projects the CLI has seen.
        if let Some(pf) = &self.project_file {
            let _ = projects::register(&self.paths, pf);
        }
        let lib = Library::load(&self.paths, self.project_file.as_slice());
        for e in &lib.errors {
            eprintln!("{} {}", style("warning:").yellow().bold(), e.message);
        }
        lib
    }

    pub fn find<'a>(&self, lib: &'a Library, name: &str) -> Result<&'a Entry> {
        if let Some(e) = lib.find(name) {
            return Ok(e);
        }
        let similar: Vec<&str> =
            lib.entries.iter().map(|e| e.name.as_str()).filter(|n| n.contains(name) || name.contains(n)).collect();
        if similar.is_empty() {
            bail!("no bookmark named `{name}` (see `bk ls`)")
        }
        bail!("no bookmark named `{name}`; did you mean: {}?", similar.join(", "))
    }
}

pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

pub fn parse_kv(s: &str) -> Result<(String, String)> {
    let (k, v) = s.split_once('=').with_context(|| format!("expected KEY=VALUE, got `{s}`"))?;
    if k.is_empty() {
        bail!("expected KEY=VALUE, got `{s}`");
    }
    Ok((k.to_string(), v.to_string()))
}

/// One quoted argument is taken verbatim; several words are re-quoted as needed.
fn command_from_words(words: &[String]) -> String {
    match words {
        [one] => one.clone(),
        many => many.iter().map(|w| render::q(w)).collect::<Vec<_>>().join(" "),
    }
}

/// How a working dir is written to the file: relative inside a project, else with `~`.
pub fn stored_dir(abs: &Path, project_dir: Option<&Path>) -> String {
    if let Some(pd) = project_dir
        && let Ok(rest) = abs.strip_prefix(pd)
    {
        return if rest.as_os_str().is_empty() { ".".into() } else { paths::portable(rest) };
    }
    paths::contract_tilde(abs)
}

/// Target file for a new bookmark, plus the project dir when it goes into a project.
pub fn target_file(ctx: &Ctx, project: bool) -> (PathBuf, Option<PathBuf>) {
    if project {
        let f = ctx.project_file.clone().unwrap_or_else(|| ctx.cwd.join(paths::PROJECT_FILE));
        let d = f.parent().map(Path::to_path_buf);
        (f, d)
    } else {
        (ctx.paths.config_file.clone(), None)
    }
}

pub fn ensure_free(file: &Path, name: &str, force: bool) -> Result<()> {
    store::validate_name(name)?;
    if store::read_file(file)?.bookmarks.contains_key(name) && !force {
        bail!("`{name}` already exists in {} (use --force to overwrite)", paths::contract_tilde(file));
    }
    Ok(())
}

/// Explicit spec, else the env active in `active`, else folder markers.
pub fn choose_env(given: Option<&str>, active: &EnvMap, dir: &Path) -> Result<(EnvSpec, &'static str)> {
    if let Some(s) = given {
        return Ok((EnvSpec::parse(s).map_err(anyhow::Error::msg)?, "given"));
    }
    if let Some(s) = detect::from_active(active, dir) {
        return Ok((s, "active in this shell"));
    }
    Ok(match detect::from_folder(dir, active) {
        Some(s) => (s, "detected in folder"),
        None => (EnvSpec::Inherit, "default"),
    })
}

/// Explicit `LANG=VERSION` pairs win over pins detected from version files / `nvm use`.
pub fn choose_runtimes(
    given: &[String],
    active: &EnvMap,
    dir: &Path,
    env_spec: &EnvSpec,
) -> Result<BTreeMap<String, String>> {
    // mise already manages every tool for the folder.
    let detected = if matches!(env_spec, EnvSpec::Mise) { BTreeMap::new() } else { detect::runtimes(active, dir) };
    parse_runtimes(given, detected)
}

/// `LANG=VERSION` pairs over `base` (`LANG=none` removes one).
pub fn parse_runtimes(given: &[String], base: BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let mut out = base;
    for kv in given {
        let (lang, version) = parse_kv(kv)?;
        let lang = lang.to_ascii_lowercase();
        if !runtimes::SUPPORTED.contains(&lang.as_str()) {
            bail!("unknown runtime `{lang}` (supported: {})", runtimes::SUPPORTED.join(", "));
        }
        if version.eq_ignore_ascii_case("none") {
            out.remove(&lang);
        } else {
            out.insert(lang, version);
        }
    }
    Ok(out)
}

/// Write a bookmark (keeping project trust) and register the project.
pub fn persist(ctx: &Ctx, file: &Path, name: &str, project: bool, b: &Bookmark) -> Result<Entry> {
    trust::write_preserving(&ctx.paths, file, || store::upsert(file, name, b))?;
    if project {
        projects::register(&ctx.paths, file)?;
    }
    Ok(Entry::new(name, if project { Scope::Project } else { Scope::Global }, file, b.clone()))
}

pub fn add(ctx: &Ctx, a: AddArgs) -> Result<i32> {
    let (file, project_dir) = target_file(ctx, a.project);
    ensure_free(&file, &a.name, a.force)?;

    let given = match (a.command, a.words.is_empty()) {
        (Some(_), false) => bail!("give the command either quoted or after `--`, not both"),
        (Some(c), true) => Some(c),
        (None, false) => Some(command_from_words(&a.words)),
        (None, true) => None,
    };
    let (command, steps) = match (given, a.steps.is_empty()) {
        (Some(c), true) => (Some(c), vec![]),
        (None, false) => (None, a.steps.iter().map(Step::new).collect()),
        (None, true) => bail!("give a command (`bk add NAME 'cmd ...'`) or --step"),
        (Some(_), false) => bail!("use either a COMMAND or --step, not both"),
    };

    if let Some(server) = a.server.as_deref().filter(|s| !s.trim().is_empty()) {
        // Everything refers to the server: nothing is detected or resolved locally.
        let b = Bookmark {
            description: a.description,
            server: Some(server.to_string()),
            cwd: a.cwd,
            env: match &a.env {
                Some(s) => EnvSpec::parse(s).map_err(anyhow::Error::msg)?,
                None => EnvSpec::Inherit,
            },
            runtimes: parse_runtimes(&a.runtimes, BTreeMap::new())?,
            path: a.path,
            command,
            steps,
            macos: None,
            linux: None,
            windows: None,
            tags: a.tags,
            dotenv: a.dotenv,
            vars: a.vars.iter().map(|v| parse_kv(v)).collect::<Result<BTreeMap<_, _>>>()?,
            shell: a.shell,
            confirm: a.confirm,
            notify: a.notify,
            pinned: false,
            forward: a.forward,
            detach: a.detach,
        };
        bk_core::servers::resolve_target(server, &ctx.library().servers)?;
        let entry = persist(ctx, &file, &a.name, a.project, &b)?;
        report_saved(ctx, &entry, "on the server");
        return Ok(0);
    }
    if !a.forward.is_empty() || a.detach {
        bail!("--forward and --detach need --server");
    }
    let abs_cwd = match &a.cwd {
        Some(c) => paths::resolve(c, &ctx.cwd),
        None => ctx.cwd.clone(),
    };
    if !a.no_cwd && !abs_cwd.is_dir() {
        bail!("{} is not a directory", abs_cwd.display());
    }
    let cwd = (!a.no_cwd).then(|| stored_dir(&abs_cwd, project_dir.as_deref()));
    let (env_spec, env_note) = choose_env(a.env.as_deref(), &ctx.env, &abs_cwd)?;

    let vars = a.vars.iter().map(|v| parse_kv(v)).collect::<Result<BTreeMap<_, _>>>()?;
    let runtimes = choose_runtimes(&a.runtimes, &ctx.env, &abs_cwd, &env_spec)?;
    let b = Bookmark {
        description: a.description,
        cwd,
        env: env_spec,
        runtimes,
        path: a.path,
        command,
        steps,
        macos: None,
        linux: None,
        windows: None,
        tags: a.tags,
        dotenv: a.dotenv,
        vars,
        shell: a.shell,
        confirm: a.confirm,
        notify: a.notify,
        pinned: false,
        server: None,
        forward: Vec::new(),
        detach: false,
    };
    let entry = persist(ctx, &file, &a.name, a.project, &b)?;
    report_saved(ctx, &entry, env_note);
    Ok(0)
}

pub fn report_saved(ctx: &Ctx, entry: &Entry, env_note: &str) {
    let scope = if entry.scope == Scope::Project { "project" } else { "global" };
    eprintln!(
        "{} saved {} {}",
        style("✓").green().bold(),
        style(&entry.name).bold(),
        style(format!("({scope})")).dim()
    );
    print_details(ctx, entry, Some(env_note), false);
}

/// Project bookmarks only run once their `.bk.toml` content is approved.
fn ensure_trusted(ctx: &Ctx, e: &Entry, yes: bool) -> Result<()> {
    if e.scope != Scope::Project || trust::is_trusted(&ctx.paths, &e.file) {
        return Ok(());
    }
    let file = paths::contract_tilde(&e.file);
    if !interactive() || yes {
        bail!("{file} is new or changed since you last approved it; review it, then run `bk trust` in that folder");
    }
    eprintln!("{} {file} is new or has changed. It can run any command; this one is:", style("!").yellow().bold());
    for s in e.bookmark.effective_steps() {
        eprintln!("    {}", style(&s.run).cyan());
    }
    if Confirm::with_theme(&theme()).with_prompt(format!("Trust {file}?")).default(false).interact()? {
        trust::trust(&ctx.paths, &e.file)?;
        Ok(())
    } else {
        bail!("not trusted")
    }
}

pub fn print_details(ctx: &Ctx, e: &Entry, env_note: Option<&str>, with_file: bool) {
    let b = &e.bookmark;
    let label = |s: &str| style(format!("  {s:<8}")).dim().to_string();
    if with_file {
        eprintln!("{}{}", label("file"), paths::contract_tilde(&e.file));
    }
    if let Some(d) = &b.description {
        eprintln!("{}{d}", label("about"));
    }
    if let Some(server) = b.server.as_deref().filter(|s| !s.is_empty()) {
        let dest = bk_core::servers::resolve_target(server, &ctx.library().servers)
            .map(|t| style(t.destination).dim().to_string())
            .unwrap_or_else(|err| style(format!("✗ {err:#}")).red().to_string());
        eprintln!("{}{server}  {dest}", label("server"));
        eprintln!("{}{}", label("cwd"), b.cwd.as_deref().unwrap_or("~"));
        let env_note = env_note.map(|n| format!(" [{n}]")).unwrap_or_default();
        eprintln!("{}{}{}", label("env"), b.env.label(), style(env_note).dim());
        for (lang, spec) in &b.runtimes {
            eprintln!("{}{lang} {spec}", label("runtime"));
        }
        for f in &b.forward {
            eprintln!("{}{f}", label("forward"));
        }
        if b.detach {
            eprintln!("{}keeps running in tmux on the server", label("detach"));
        }
        for s in b.effective_steps() {
            eprintln!("{}{}", label("run"), style(&s.run).cyan());
        }
        return;
    }
    let cwd = e.cwd();
    let folder = match e.folder() {
        None => "(wherever bk runs)".to_string(),
        Some(Located::Here(p)) => paths::contract_tilde(&p),
        Some(Located::Mapped { path, shared, local }) => {
            format!("{}  {}", paths::contract_tilde(&path), style(format!("({shared} → {local} on this device)")).dim())
        }
        Some(missing) => style(format!("✗ {}", missing.missing_reason().unwrap_or_default())).yellow().to_string(),
    };
    eprintln!("{}{folder}", label("cwd"));
    let check_dir = cwd.clone().unwrap_or_else(|| ctx.cwd.clone());
    let env_status = match env::activate(&b.env, &check_dir, &ctx.env, Mode::Check) {
        Ok(a) => style(a.description).dim().to_string(),
        Err(err) => style(format!("✗ {err:#}")).red().to_string(),
    };
    let note = env_note.map(|n| format!(" [{n}]")).unwrap_or_default();
    eprintln!("{}{}{}  {}", label("env"), b.env.label(), style(note).dim(), env_status);
    for (lang, spec) in &b.runtimes {
        let status = match runtimes::resolve(lang, spec, &check_dir, &ctx.env) {
            Ok(Some(r)) => style(r.description).dim().to_string(),
            Ok(None) => style("nothing pinned, uses PATH".to_string()).dim().to_string(),
            Err(err) => style(format!("✗ {err:#}")).red().to_string(),
        };
        eprintln!("{}{lang} {spec}  {status}", label("runtime"));
    }
    if !b.path.is_empty() {
        eprintln!("{}{}", label("path"), b.path.join(":"));
    }
    if let Some(f) = &b.dotenv {
        eprintln!("{}{f}", label("dotenv"));
    }
    for (k, v) in &b.vars {
        eprintln!("{}{k}={v}", label("var"));
    }
    if !b.tags.is_empty() {
        eprintln!("{}{}", label("tags"), b.tags.join(", "));
    }
    let steps = b.effective_steps();
    for (i, s) in steps.iter().enumerate() {
        let tag = if steps.len() == 1 { "run".to_string() } else { format!("{}.", i + 1) };
        let mut extra = Vec::new();
        if let Some(c) = &s.cwd {
            extra.push(format!("in {c}"));
        }
        if s.continue_on_error {
            extra.push("continue on error".into());
        }
        let extra = if extra.is_empty() { String::new() } else { format!("  ({})", extra.join(", ")) };
        eprintln!("{}{}{}", label(&tag), style(&s.run).cyan(), style(extra).dim());
    }
    // What runs instead on the other OSes.
    for os in Os::ALL.into_iter().filter(|os| *os != Os::current()) {
        if let Some(v) = b.variant(os) {
            let what = match b.steps_on(os).as_slice() {
                [one] if v.replaces_steps() => one.run.clone(),
                many if v.replaces_steps() => format!("{} steps", many.len()),
                _ => "same command".into(),
            };
            let shell = v.shell.as_deref().map(|s| format!("  (in {s})")).unwrap_or_default();
            eprintln!("{}{}{}", label(os.key()), style(what).cyan(), style(shell).dim());
        }
    }
    let params = template::bookmark_params(b);
    if !params.is_empty() {
        let list: Vec<String> = params
            .iter()
            .map(|p| match &p.default {
                Some(d) => format!("{}={d}", p.name),
                None => format!("{} (required)", p.name),
            })
            .collect();
        eprintln!("{}{}", label("params"), list.join(", "));
    }
}

pub fn ls(ctx: &Ctx, a: &LsArgs) -> Result<i32> {
    let lib = ctx.library();
    let entries: Vec<&Entry> = lib
        .entries
        .iter()
        .filter(|e| a.tag.as_ref().is_none_or(|t| e.bookmark.tags.contains(t)))
        .filter(|e| !lib.is_shadowed(e))
        .collect();
    if a.names {
        for e in entries {
            println!("{}", e.name);
        }
        return Ok(0);
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(0);
    }
    if entries.is_empty() {
        eprintln!("No bookmarks yet. Add one with `bk add NAME 'command'`.");
        return Ok(0);
    }
    let width = Term::stdout().size_checked().map(|(_, w)| w as usize).unwrap_or(120);
    let name_w = entries.iter().map(|e| e.name.len()).max().unwrap_or(4).max(4);
    let env_w = entries.iter().map(|e| e.bookmark.env_summary().len()).max().unwrap_or(3).clamp(3, 28);
    for e in entries {
        let marker = if e.scope == Scope::Project { style("•").magenta().to_string() } else { " ".into() };
        let folder = e.folder();
        let missing = folder.as_ref().is_some_and(Located::is_missing);
        let dir = match folder {
            Some(Located::Missing { written, .. }) => format!("{written} (not here)"),
            _ => e.cwd().as_deref().map(paths::contract_tilde).unwrap_or_else(|| "-".into()),
        };
        let env_label = console::truncate_str(&e.bookmark.env_summary(), env_w, "…").into_owned();
        let used = 2 + name_w + 2 + env_w + 2 + console::measure_text_width(&dir) + 2;
        let summary = e.bookmark.summary();
        let cmd = console::truncate_str(&summary, width.saturating_sub(used).max(12), "…");
        println!(
            "{marker} {}  {}  {}  {}",
            style(format!("{:<name_w$}", e.name)).bold(),
            style(format!("{env_label:<env_w$}")).green(),
            if missing { style(dir).yellow() } else { style(dir).dim() },
            cmd
        );
    }
    Ok(0)
}

pub fn show(ctx: &Ctx, name: &str) -> Result<i32> {
    let lib = ctx.library();
    let e = ctx.find(&lib, name)?;
    let scope = match e.scope {
        Scope::Global => "global",
        Scope::Project => "project",
    };
    eprintln!("{} {}", style(&e.name).bold(), style(format!("({scope})")).dim());
    print_details(ctx, e, None, true);
    if let Some(last) = history::last_for(&ctx.paths.history_file(), &e.id) {
        let ago = (history::now_ms() - last.started_ms) / 1000;
        let status = match last.exit_code {
            Some(0) => style("ok".to_string()).green(),
            Some(c) => style(format!("exit {c}")).red(),
            None => style("unknown".to_string()).yellow(),
        };
        eprintln!("{}{} ago, {status}", style("  last    ").dim(), human_secs(ago));
    }
    let defaults: BTreeMap<String, String> = template::bookmark_params(&e.bookmark)
        .into_iter()
        .map(|p| (p.name.clone(), p.default.unwrap_or_else(|| format!("<{}>", p.name))))
        .collect();
    if let Ok(line) = render::one_liner_for(render::Dialect::native(), e, &defaults, &[], &ctx.cwd, &ctx.env) {
        eprintln!("{}{}", style("  shell   ").dim(), line);
    }
    Ok(0)
}

pub fn human_secs(s: i64) -> String {
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

pub fn rm(ctx: &Ctx, names: &[String], yes: bool) -> Result<i32> {
    let lib = ctx.library();
    let entries = names.iter().map(|n| ctx.find(&lib, n)).collect::<Result<Vec<_>>>()?;
    if !yes && interactive() {
        let list = entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", ");
        let ok = Confirm::with_theme(&theme()).with_prompt(format!("Delete {list}?")).default(false).interact()?;
        if !ok {
            return Ok(1);
        }
    }
    for e in entries {
        trust::write_preserving(&ctx.paths, &e.file, || store::remove(&e.file, &e.name))?;
        eprintln!("{} deleted {}", style("✓").green().bold(), e.name);
    }
    Ok(0)
}

pub fn mv(ctx: &Ctx, old: &str, new: &str) -> Result<i32> {
    let lib = ctx.library();
    let e = ctx.find(&lib, old)?;
    trust::write_preserving(&ctx.paths, &e.file, || store::rename(&e.file, &e.name, new))?;
    eprintln!("{} renamed {old} → {new}", style("✓").green().bold());
    Ok(0)
}

/// Fill in params: CLI values, then prompts (interactive) for anything without a default.
fn collect_params(e: &Entry, given: &[String], yes: bool) -> Result<BTreeMap<String, String>> {
    let declared = template::bookmark_params(&e.bookmark);
    let mut values = BTreeMap::new();
    for kv in given {
        let (k, v) = parse_kv(kv)?;
        if !declared.iter().any(|p| p.name == k) {
            let names: Vec<_> = declared.iter().map(|p| p.name.as_str()).collect();
            bail!(
                "`{}` has no param `{k}`{}",
                e.name,
                if names.is_empty() { String::new() } else { format!(" (params: {})", names.join(", ")) }
            );
        }
        values.insert(k, v);
    }
    let missing = template::missing(&declared, &values);
    if missing.is_empty() {
        return Ok(values);
    }
    if yes || !interactive() {
        bail!(
            "missing value for {} (pass {})",
            missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", "),
            missing.iter().map(|m| format!("{m}=...")).collect::<Vec<_>>().join(" ")
        );
    }
    for name in missing {
        let v: String = Input::with_theme(&theme()).with_prompt(&name).allow_empty(true).interact_text()?;
        values.insert(name, v);
    }
    Ok(values)
}

pub fn run(ctx: &Ctx, r: RunArgs) -> Result<i32> {
    let mut lib = ctx.library();
    if lib.find(&r.name).is_none() && crate::sync_cmds::fetch_missing(ctx, &r.name) {
        lib = ctx.library();
    }
    if let Some(fresh) = crate::folders_cmds::ensure_here(ctx, &lib, ctx.find(&lib, &r.name)?)? {
        lib = fresh;
    }
    let e = ctx.find(&lib, &r.name)?;
    if !r.dry_run && !r.print {
        ensure_trusted(ctx, e, r.yes)?;
    }
    let params = collect_params(e, &r.params, r.yes)?;
    let req = RunRequest {
        params,
        extra_args: r.extra.clone(),
        overrides: r.vars.iter().map(|v| parse_kv(v)).collect::<Result<_>>()?,
        default_cwd: ctx.cwd.clone(),
    };
    if e.bookmark.is_remote() {
        return crate::remote_cmds::run(ctx, &lib, e, &req, r.dry_run, r.print, e.bookmark.confirm && !r.yes);
    }
    if r.print {
        println!("{}", render::one_liner_for(render::Dialect::native(), e, &req.params, &r.extra, &ctx.cwd, &ctx.env)?);
        return Ok(0);
    }
    let mode = if r.dry_run { Mode::Check } else { Mode::Run };
    let plan = plan::build(e, &req, &ctx.env, &lib.settings, mode)?;

    if r.dry_run || (e.bookmark.confirm && !r.yes) {
        print_plan(&plan);
    }
    if r.dry_run {
        return Ok(0);
    }
    if e.bookmark.confirm && !r.yes {
        if !interactive() {
            bail!("`{}` asks for confirmation; pass --yes to run non-interactively", e.name);
        }
        if !Confirm::with_theme(&theme()).with_prompt("Run it?").default(true).interact()? {
            return Ok(1);
        }
    }

    for n in &plan.notes {
        eprintln!("{} {n}", style("note:").yellow());
    }
    let started = history::now_ms();
    let outcome = exec::run(&plan)?;
    if lib.settings.history_enabled() {
        let mut rec = history::RunRecord::from_plan(&plan, "cli", started);
        rec.device = Some(bk_core::sync::this_device(&ctx.paths));
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

fn print_plan(plan: &plan::RunPlan) {
    let s = plan.summary();
    let label = |t: &str| style(format!("  {t:<7}")).dim().to_string();
    eprintln!("{} {}", style("plan").bold(), style(&s.name).bold());
    eprintln!("{}{}", label("cwd"), s.cwd);
    eprintln!("{}{}  {}", label("env"), s.env_label, style(&s.env_description).dim());
    if !s.env_changes.is_empty() {
        eprintln!("{}{}", label("vars"), style(s.env_changes.join(" ")).dim());
    }
    eprintln!("{}{}", label("shell"), s.shell);
    for (i, st) in s.steps.iter().enumerate() {
        let dir = if st.cwd != plan.cwd { format!("  (in {})", paths::contract_tilde(&st.cwd)) } else { String::new() };
        eprintln!("{}{}{}", label(&format!("{}.", i + 1)), style(&st.command).cyan(), style(dir).dim());
    }
}
