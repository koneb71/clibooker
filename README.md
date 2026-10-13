# Clibooker

Bookmark shell commands **together with the folder, Python environment and language runtimes they run in**, then replay them with one click, one keystroke, or `bk <name>`.

```sh
conda activate sam2 && cd ~/work/sam2
python train.py --epochs 20
bk save train          # saves the command + folder + conda env "sam2"
bk train               # later, from anywhere: cd + activate + run
```

It comes in two parts that share the same bookmarks file:

- **Clibooker.app**, a macOS desktop app:
  - a bookmark library and editor;
  - an embedded terminal for runs;
  - a menu-bar icon;
  - a global quick-run palette (⌥Space).
- **`bk`**, a CLI with zsh integration: save the command you just ran, run bookmarks, enter a bookmark's environment.

## Features

- **Python environments:**
  - conda (by name or prefix; `activate.d` hooks run);
  - venv;
  - uv projects (optionally `uv sync` first);
  - poetry;
  - pyenv;
  - auto-detect at run time;
  - "clean", which strips whatever env is active;
  - inherit.
- **Leaked envs are removed:** an active venv or conda env is stripped before the target env is activated.
- **Language runtimes on top:** `runtimes = { node = "20", java = "21" }` (see below).
- **Import project scripts:** `bk scan` (or the 📦 button in the app) turns a project's scripts into bookmarks. Sources: package.json (with the right npm/pnpm/yarn/bun), deno.json, composer.json, pyproject.toml (uv/poetry/poe/pdm), Makefile, justfile, Taskfile.yml and Procfile.
- **Extra PATH entries:** `path = ["vendor/bin"]`.
- **Params:** `{{name}}` and `{{name:default}}` placeholders. The app shows a form; the CLI takes `bk train epochs=30` or prompts you. Go/Jinja/docker templates such as `{{.Names}}` are left alone.
- **Workflows:** multiple steps, each with an optional subfolder and *continue on error*. The run stops at the first failure.
- **`.env` files and per-bookmark variables** (`${OTHER}` expands).
- **Project bookmarks:** a `.bk.toml` in a repo, shareable through git.
  - Paths in it are relative to the repo.
  - Like direnv, a project file has to be **trusted** before its commands run. Changing it outside bk asks again.
- **Run history:** commands, params, exit code and duration. Environment values are never stored.
- **Doctor:** finds bookmarks whose folder, env, `.env` file or shell no longer exists.
- **Inbox (opt-in):** recently run terminal commands with their folder and env, one click from becoming a bookmark.
- **Import / export** as TOML or JSON.
- **Pop-out:** open a bookmark in Terminal, iTerm, Ghostty or Warp, or copy it as a one-liner.
- **SSH servers:** connect to your servers, run bookmarks on them, forward ports, and keep long jobs running after you disconnect (see below).

## Build & install

You need Rust (stable), Node and pnpm, and Xcode command-line tools.

```sh
# CLI only
cargo install --path crates/bk-cli          # installs ~/.cargo/bin/bk

# Desktop app (.app + .dmg in target/release/bundle/)
cd app && pnpm install && pnpm tauri build
```

The app bundles `bk`. **Settings → Command line → Install bk command** links it into `~/.local/bin`.

### Shell integration (zsh)

Add this to `~/.zshrc`, after `conda init` and `compinit`:

```sh
eval "$(bk init zsh)"            # bk save, bk go, completion
# or
eval "$(bk init zsh --record)"   # also feed the app's Inbox with your recent commands
```

`--record` skips any command that starts with a space or looks like it contains a secret (`token`, `password`, `api_key`, …). The log file is mode 0600 and keeps only the last 500 commands.

## CLI

| Command | What it does |
| --- | --- |
| `bk save NAME` | Bookmark the command you just ran, with its folder and active env |
| `bk add NAME 'cmd …'` | Add a bookmark. Options: `--env conda:NAME` · `venv[:PATH]` · `uv` · `uv-sync` · `poetry` · `pyenv:VER` · `mise` · `auto` · `none`, `-r node=20` (repeatable), `--path DIR`, `--cwd`, `--step` (repeatable), `--var K=V`, `--dotenv`, `-t TAG`, `--project` |
| `bk scan [DIR]` | Bookmark a project's scripts (`--list`, `--project`, `--prefix`, `--yes`) |
| `bk NAME [k=v…] [-- extra args]` | Run (same as `bk run NAME`). `-n` dry run, `--print` one-liner, `-e K=V`, `-y` |
| `bk go NAME` | `cd` + activate the env in your current shell |
| `bk ls` · `show` · `edit` · `rm` · `mv` | Manage bookmarks (`edit` opens `$EDITOR`) |
| `bk history [NAME]` | Past runs (from the app and the CLI) |
| `bk doctor` | Check setup and every bookmark |
| `bk trust [DIR]` | Approve a project's `.bk.toml` (`--revoke` to withdraw) |
| `bk export [NAMES] [-o FILE] [--json]` · `bk import FILE [--on-conflict skip\|overwrite\|rename]` | Move bookmarks between machines |
| `bk app` | Open the desktop app |
| `bk ssh SERVER [-- cmd]` | Interactive SSH session (or run one command) |
| `bk server add NAME user@host [-p PORT] [-i KEY] [-J JUMP] [-L 8888:127.0.0.1:8888] [-o K=V]` | Save a server · `bk server rm NAME` · `bk server unhide NAME` · `bk server scan NAME` |
| `bk servers [--check]` | List servers (yours and `~/.ssh/config`) with connection status |
| `bk add NAME -s SERVER [--cwd ~/proj] [--env conda:x] [-L …] [--detach] 'cmd'` | A bookmark that runs on a server |
| `bk scan SERVER:~/proj` | Bookmark a remote project's scripts |

Exit codes are passed through from the command. Ctrl+C reaches the command (Python raises `KeyboardInterrupt`) and stops a workflow.

## The bookmarks file

The global file is `~/.config/bk/bookmarks.toml`. `BK_CONFIG` or `XDG_CONFIG_HOME` override the location, and run data lives in `~/.local/share/bk/`. Both the app and the CLI write it with comment-preserving edits, so editing it by hand is fine. The app picks up changes live.

```toml
[settings]
theme = "system"
hotkey = "Alt+Space"
terminal = "iterm"

[bookmarks.train]
description = "Fine-tune SAM2"
cwd = "~/work/sam2"
env = { type = "conda", name = "sam2" }        # or the short form: env = "conda:sam2"
runtimes = { node = "auto", java = "21" }
command = "python train.py --epochs {{epochs:10}} --data {{dataset}}"
vars = { CUDA_VISIBLE_DEVICES = "0" }
notify = true
tags = ["ml"]

[bookmarks.dev]
cwd = "~/work/api"
env = { type = "uv", sync = true }
dotenv = ".env"
steps = [
  "alembic upgrade head",
  { run = "npm run dev", cwd = "web", continue_on_error = true },
]
```

### Language runtimes

| Runtime | Example | Found in | Sets |
| --- | --- | --- | --- |
| `node` | `"22"`, `"lts"`, `">=18"`, `"auto"` | nvm, fnm, volta, mise, asdf, Homebrew `node@N` | PATH: the project's `node_modules/.bin`, then that node |
| `java` | `"21"`, `"8"` / `"1.8"` | `/Library/Java/JavaVirtualMachines`, SDKMAN, Homebrew `openjdk@N`, mise, asdf | `JAVA_HOME`, PATH |
| `ruby` | `"3.3"` | rbenv, chruby, rvm, mise, asdf, Homebrew | PATH |
| `go` | `"1.22"`, `"1.22.3"` | `~/sdk`, goenv, mise, asdf, Homebrew; a full version can also be downloaded via `GOTOOLCHAIN` | `GOROOT`/`GOTOOLCHAIN`, PATH |
| `rust` | `"stable"`, `"nightly"`, `"1.80"` | rustup toolchains | `RUSTUP_TOOLCHAIN` |
| `php` | `"8.3"` | Homebrew `php@X`, mise, asdf | PATH: the project's `vendor/bin`, then that php |

How versions are matched:
- A version like `22` picks the newest installed 22.x.
- `auto` follows the project's pin: `.nvmrc`, `.node-version`, `package.json` `engines`/`volta`, `.java-version`, `.sdkmanrc`, `.ruby-version`, `.go-version`, `.php-version`, `.tool-versions`. With no pin, `auto` uses whatever is on PATH.
- `system` changes nothing.

Bookmarks created with `bk add`, `bk save` or `bk scan` pin `auto` automatically when the folder has such a file. An `nvm use` in the current shell is picked up too. A version that isn't installed is reported with what *is* installed.

For projects managed by mise (`mise.toml` / `.tool-versions`), use `env = "mise"` and every tool is activated at once.

### How a run's environment is built

In order:
1. Start from your shell env (in the app, from your **login shell**, so Homebrew, `~/.local/bin` and `conda` are found even when it's launched from Finder).
2. Strip any active conda env or venv.
3. Activate the target env.
4. Add runtimes and `path` entries (these go first on PATH).
5. Load `.env`.
6. Apply `vars`.
7. Apply `-e` overrides.

Steps run with `zsh -f -c` by default (no rc files, so aliases aren't available). Set `shell = "bash"` (or another shell) to change it.

## SSH servers

Servers come from Clibooker (`[servers.NAME]` in the global file) plus every host in `~/.ssh/config`, including its `Include`d files.

To remove a server, right-click it in the sidebar, use the trash button on its page, or run `bk server rm NAME`:
- A Clibooker server is deleted.
- An `~/.ssh/config` host is only hidden from Clibooker's lists (`[settings] hidden_servers`). The file isn't edited, and the host still works by name.
- Bring a hidden host back with Settings → Hidden servers or `bk server unhide NAME`. Authentication uses your keys and ssh-agent (macOS Keychain and 1Password agents work). Password prompts can be typed into the terminal; Clibooker never stores them.

```toml
[servers.gpu]
host = "10.0.0.5"
user = "ubuntu"
identity = "~/.ssh/lab_ed25519"
jump = "bastion"                     # another server, or user@host[:port]
forward = ["8888:127.0.0.1:8888"]    # opened with interactive sessions

[bookmarks.train-remote]
server = "gpu"                       # a server name, an ~/.ssh/config alias, or user@host[:port]
cwd = "~/sam2"                       # path on the server
env = "conda:sam2"                   # activated on the server
runtimes = { node = "auto" }
command = "python train.py --epochs {{epochs:50}}"
forward = ["6006:127.0.0.1:6006"]    # TensorBoard while it runs
detach = true                        # keep running in tmux if you disconnect
```

How remote bookmarks work:
- **Same features as local ones:** params, workflows, `.env` files (read from the server, parsed like local ones), vars, Stop / Ctrl+C, history, doctor.
- **Envs and runtimes resolve on the server.** Clibooker scans it once for conda envs, nvm/sdkman/rbenv/rustup versions and tmux, and caches the result (`bk server scan` refreshes it).
- **The script is sent base64-encoded**, so any login shell on the server works (bash, zsh, fish, csh, dash).

Interactive use:
- **Sessions:** **Connect** in the app (or `bk ssh gpu`) opens an interactive shell.
- **Shell in context:** **Open shell here** (or `bk go train-remote`) gives a shell in a bookmark's folder and env.

Port forwards:
- Each forward runs as its own tunnel, so a busy local port is reported without breaking the run.
- The tab shows `localhost:PORT ↗` links.

Keep running (`detach`, needs tmux on the server):
- The run lives in tmux. **Detach** leaves it going; running the bookmark again reattaches.
- Progress comes from a state file in `~/.local/state/bk/` on the server.

Connection status:
- Each server shows one of: reachable, needs password/key, host key not trusted yet (the app shows the fingerprint and can trust it), host key CHANGED (refused), or unreachable.
- Background checks use `BatchMode`, so they never prompt.

Safety:
- Only an allowlist of `-o` options is accepted (ProxyCommand and similar belong in `~/.ssh/config`).
- Forwards can only bind to loopback.
- Hosts and users are validated.
- Project `.bk.toml` files can reference servers but not define them, and they still need `bk trust`.

## Development

```sh
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings   # SSH tests start a private sshd on 127.0.0.1
cd app && pnpm typecheck
cd app && pnpm tauri dev      # native app with hot reload
cd app && pnpm dev            # UI only, in a browser, against an in-memory mock backend
```

```
crates/bk-core     shared logic: model, store (toml_edit), env activation, plan, render, history, trust, doctor,
                   servers (ssh config, argv, status, discovery), remote (remote runs, tmux), script (run wrapper)
crates/bk-cli      the `bk` binary + zsh integration (src/shell/init.zsh)
app/src-tauri      Tauri backend: PTY runs, palette, tray, file watcher, commands
app/src            React UI (library, editor, terminal tabs, palette, history, inbox, doctor, settings)
```

## Contributing

Issues and pull requests are welcome. Before opening a PR, run the checks from [Development](#development) (`cargo test`, `clippy`, `pnpm typecheck`).

## License

[MIT](LICENSE)
