use clap::{Args, Parser, Subcommand};

/// Bookmark shell commands together with their folder and Python env, and replay them.
#[derive(Debug, Parser)]
#[command(name = "bk", version, about, max_term_width = 100)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Save a new bookmark
    Add(Box<AddArgs>),
    /// Run a bookmark (`bk <name>` is a shortcut)
    Run(RunArgs),
    /// List bookmarks
    #[command(visible_alias = "list")]
    Ls(LsArgs),
    /// Show a bookmark's details
    Show { name: String },
    /// Delete bookmarks
    #[command(visible_alias = "remove")]
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Rename a bookmark
    #[command(visible_alias = "rename")]
    Mv { old: String, new: String },
    /// Bookmark the command you just ran (needs the shell hook)
    Save(SaveArgs),
    /// Enter a bookmark's folder and env in the current shell (needs the shell hook)
    Go {
        name: String,
        /// Print the shell code instead (used by the hook)
        #[arg(long)]
        print: bool,
    },
    /// Edit a bookmark (or the whole file) in $EDITOR
    Edit {
        name: Option<String>,
        /// With no NAME: edit the project's .bk.toml instead of the global file
        #[arg(short, long)]
        project: bool,
    },
    /// Show past runs
    History {
        name: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Check that folders, envs, .env files and shells still resolve
    Doctor,
    /// Approve a project's .bk.toml so its bookmarks can run
    Trust {
        /// Project dir or .bk.toml [default: nearest .bk.toml]
        path: Option<std::path::PathBuf>,
        /// Withdraw approval
        #[arg(long)]
        revoke: bool,
    },
    /// Export bookmarks as TOML (or JSON)
    Export {
        names: Vec<String>,
        /// Write to a file instead of stdout
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Import bookmarks from a TOML or JSON export
    Import {
        file: std::path::PathBuf,
        /// Import into the project's .bk.toml
        #[arg(short, long)]
        project: bool,
        /// What to do when a name already exists
        #[arg(long, value_enum, default_value_t = OnConflict::Skip)]
        on_conflict: OnConflict,
    },
    /// Print shell integration: add `eval "$(bk init zsh)"` to ~/.zshrc
    Init {
        #[arg(value_enum)]
        shell: InitShell,
        /// Also record every command for the desktop app's Inbox
        #[arg(long)]
        record: bool,
    },
    /// Turn a project's scripts (package.json, Makefile, justfile, composer.json, ...) into bookmarks
    Scan(ScanArgs),
    /// Manage SSH servers (`bk servers` lists them)
    #[command(visible_alias = "servers")]
    Server {
        #[command(subcommand)]
        cmd: Option<ServerCmd>,
        /// Check each server's connection
        #[arg(long)]
        check: bool,
    },
    /// Open an interactive SSH session to a server
    Ssh {
        /// Server name, ~/.ssh/config alias, or user@host[:port]
        server: String,
        /// Command to run instead of a login shell
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Open the Clibooker desktop app
    App,
    /// `bk <name> [key=value...]`
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    pub name: String,
    /// The command, quoted as one argument (or give the words after `--`)
    #[arg(value_name = "COMMAND", allow_hyphen_values = true)]
    pub command: Option<String>,
    #[arg(last = true, value_name = "WORDS", hide = true)]
    pub words: Vec<String>,
    /// Add a workflow step (repeatable; use instead of COMMAND)
    #[arg(long = "step", value_name = "CMD")]
    pub steps: Vec<String>,
    /// Working directory [default: current directory]
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<String>,
    /// Don't pin a working directory (run wherever bk is called)
    #[arg(long, conflicts_with = "cwd")]
    pub no_cwd: bool,
    /// conda:NAME | conda-prefix:PATH | venv[:PATH] | uv | uv-sync | poetry | pyenv:VER | auto | none | inherit
    /// [default: detected from the active env, then the folder]
    #[arg(long, value_name = "SPEC")]
    pub env: Option<String>,
    /// Language runtime: node=20, java=21, ruby=3.3, go=1.22.3, rust=nightly, php=8.3, or LANG=auto
    /// (repeatable) [default: pins found in .nvmrc / .java-version / ... files]
    #[arg(short = 'r', long = "runtime", value_name = "LANG=VERSION")]
    pub runtimes: Vec<String>,
    /// Extra PATH entry, relative to the working directory (repeatable), e.g. vendor/bin
    #[arg(long = "path", value_name = "DIR")]
    pub path: Vec<String>,
    /// Run on this SSH server (a `bk server` name, an ~/.ssh/config alias, or user@host[:port]).
    /// --cwd, --env, --runtime and --dotenv then refer to the server.
    #[arg(short = 's', long, value_name = "SERVER")]
    pub server: Option<String>,
    /// Port forward while it runs, e.g. 8888:127.0.0.1:8888 (repeatable; remote only)
    #[arg(short = 'L', long = "forward", value_name = "LPORT:HOST:PORT")]
    pub forward: Vec<String>,
    /// Run inside tmux on the server so it survives disconnects (remote only)
    #[arg(long)]
    pub detach: bool,
    #[arg(short, long)]
    pub description: Option<String>,
    #[arg(short, long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
    /// Environment variable for the run (repeatable)
    #[arg(long = "var", value_name = "KEY=VALUE")]
    pub vars: Vec<String>,
    /// .env file to load (relative to the working directory)
    #[arg(long, value_name = "FILE")]
    pub dotenv: Option<String>,
    /// Shell for the steps (zsh, bash, sh, ...)
    #[arg(long)]
    pub shell: Option<String>,
    /// Ask before running
    #[arg(long)]
    pub confirm: bool,
    /// Desktop notification when finished (app)
    #[arg(long)]
    pub notify: bool,
    /// Save to the project's .bk.toml instead of the global file
    #[arg(short, long)]
    pub project: bool,
    /// Overwrite an existing bookmark
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum ServerCmd {
    /// Save a server
    Add(ServerAddArgs),
    /// Delete a server (an ~/.ssh/config host is hidden from the list instead)
    #[command(visible_alias = "remove")]
    Rm { name: String },
    /// List a hidden ~/.ssh/config host again
    Unhide { name: String },
    /// List servers (Clibooker + ~/.ssh/config)
    #[command(visible_alias = "list")]
    Ls {
        #[arg(long)]
        check: bool,
    },
    /// Scan a server for conda envs, node/java/... versions and tools (cached for runs)
    Scan { name: String },
}

#[derive(Debug, Args)]
pub struct ServerAddArgs {
    pub name: String,
    /// user@host or host (an ~/.ssh/config alias works too)
    pub destination: String,
    #[arg(short, long)]
    pub port: Option<u16>,
    /// Private key file
    #[arg(short, long, value_name = "FILE")]
    pub identity: Option<String>,
    /// Jump host (another server name or user@host[:port])
    #[arg(short = 'J', long, value_name = "HOST")]
    pub jump: Option<String>,
    /// Port forward for interactive sessions, e.g. 8888:127.0.0.1:8888 (repeatable)
    #[arg(short = 'L', long = "forward", value_name = "LPORT:HOST:PORT")]
    pub forward: Vec<String>,
    /// Allowed ssh option, e.g. -o ServerAliveInterval=30 (repeatable)
    #[arg(short = 'o', long = "option", value_name = "KEY=VALUE")]
    pub options: Vec<String>,
    #[arg(short, long)]
    pub description: Option<String>,
    /// Shell for runs on the server: bash (default if present) or sh
    #[arg(long)]
    pub shell: Option<String>,
    /// Overwrite an existing server
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OnConflict {
    Skip,
    Overwrite,
    Rename,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum InitShell {
    Zsh,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Project folder [default: current directory], or SERVER:PATH for a folder on a server
    pub dir: Option<String>,
    /// Save into the project's .bk.toml (names without a prefix)
    #[arg(short, long)]
    pub project: bool,
    /// Name prefix [default: folder name for global bookmarks, none for --project]
    #[arg(long)]
    pub prefix: Option<String>,
    /// Import everything new without asking
    #[arg(short, long)]
    pub yes: bool,
    /// Only list what was found
    #[arg(short, long)]
    pub list: bool,
}

#[derive(Debug, Args)]
pub struct SaveArgs {
    pub name: String,
    #[arg(short, long)]
    pub description: Option<String>,
    #[arg(short, long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
    /// Override the env (same forms as `bk add --env`)
    #[arg(long, value_name = "SPEC", conflicts_with = "no_env")]
    pub env: Option<String>,
    /// Don't pin an env
    #[arg(long)]
    pub no_env: bool,
    /// Don't pin the folder
    #[arg(long)]
    pub no_cwd: bool,
    /// Save to the project's .bk.toml
    #[arg(short, long)]
    pub project: bool,
    #[arg(short, long)]
    pub force: bool,
    /// Don't ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    pub name: String,
    /// Parameter values: key=value
    #[arg(value_name = "KEY=VALUE")]
    pub params: Vec<String>,
    /// Extra arguments appended to the (last) command
    #[arg(last = true, value_name = "ARGS")]
    pub extra: Vec<String>,
    /// Show what would run without running it
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Print a shell one-liner instead of running
    #[arg(long, conflicts_with = "dry_run")]
    pub print: bool,
    /// Use defaults for params and skip confirmation
    #[arg(short, long)]
    pub yes: bool,
    /// Set an environment variable for this run (repeatable)
    #[arg(short = 'e', long = "env-var", value_name = "KEY=VALUE")]
    pub vars: Vec<String>,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// Only bookmarks with this tag
    #[arg(short, long)]
    pub tag: Option<String>,
    /// Print names only
    #[arg(long, conflicts_with = "json")]
    pub names: bool,
    #[arg(long)]
    pub json: bool,
}
