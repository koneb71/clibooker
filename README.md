# Clibooker

Bookmark shell commands **together with the folder, Python environment and language runtimes they run in**, then replay them with one click, one keystroke, or `bk <name>`.

```sh
conda activate sam2 && cd ~/work/sam2
python train.py --epochs 20
bk save train          # saves the command + folder + conda env "sam2"
bk train               # later, from anywhere: cd + activate + run
```

It comes in two parts that share the same bookmarks file:

- **Clibooker**, a desktop app for macOS, Linux and Windows:
  - a bookmark library and editor;
  - an embedded terminal for runs;
  - a menu-bar / system-tray icon;
  - a global quick-run palette (⌥Space on macOS, Ctrl+Alt+Space elsewhere).
- **`bk`**, a CLI with zsh, bash and PowerShell integration: save the command you just ran, run bookmarks, enter a bookmark's environment.

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
- **Many devices, many OSes:** bookmarks sync between your machines by themselves. Each device says where synced folders are on it, and a bookmark can run a different command on Windows, macOS or Linux (see [Sync between devices](#sync-between-devices)).

## Install

Download the app for your platform from [Releases](https://github.com/koneb71/clibooker/releases):

| Platform | App | CLI only |
| --- | --- | --- |
| macOS | `.dmg` (Apple Silicon or Intel) | `bk-…-apple-darwin.tar.gz` |
| Linux | `.deb`, `.rpm` or `.AppImage` | `bk-…-linux-gnu.tar.gz` |
| Windows | `…-setup.exe` (per user) or `.msi` | `bk-…-windows-msvc.zip` |

The builds aren't signed yet:
- **macOS:** right-click the app and choose **Open** the first time.
- **Windows:** SmartScreen asks once; choose **More info → Run anyway**.

The app bundles `bk`:
- **macOS and Linux:** **Settings → Command line → Install bk command** links it into `~/.local/bin`. The `.deb`/`.rpm` packages already install it as `/usr/bin/bk`.
- **Windows:** **Add bk to PATH** adds the app's folder to your user PATH.

### Build from source

You need Rust (stable), Node 20+ and pnpm, plus each platform's prerequisites:
- **macOS:** Xcode command-line tools.
- **Linux:** `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`.
- **Windows:** the MSVC build tools; WebView2 already ships with Windows 10 and 11.

```sh
# CLI only
cargo install --path crates/bk-cli          # installs ~/.cargo/bin/bk

# Desktop app (installers in target/release/bundle/)
cd app && pnpm install && pnpm tauri build
```

### Shell integration

Add the line for your shell and open a new terminal:

| Shell | File | Line |
| --- | --- | --- |
| zsh | `~/.zshrc` (after `conda init` and `compinit`) | `eval "$(bk init zsh)"` |
| bash | `~/.bashrc` (after `conda init`) | `eval "$(bk init bash)"` |
| PowerShell | `$PROFILE` | `bk init powershell \| Out-String \| Invoke-Expression` |

This gives you `bk save`, `bk go` and tab completion:
- **The bash hook** has no DEBUG trap, so it coexists with starship, atuin and bash-preexec.
- **The PowerShell hook** wraps your existing prompt.

Add `--record` (e.g. `eval "$(bk init zsh --record)"`) to also feed the app's Inbox with your recent commands:
- Commands that start with a space, or that look like they contain a secret (`token`, `password`, `api_key`, …), are skipped.
- The log file is private to you and keeps only the last 500 commands.

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
| `bk sync` | Sync with your other devices now · `bk sync setup` · `bk sync status` (devices, what's waiting) · `bk sync auto on\|off` · `bk sync forget` |
| `bk locate NAME [DIR]` | Say where a bookmark's folder is on this device (DIR: the current folder) |
| `bk folders` | Folders from your other devices and where they are here · `bk folders add SHARED LOCAL` · `bk folders rm SHARED` |
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

[bookmarks.clean]
cwd = "~/work/web"
command = "rm -rf build dist && pnpm build"
windows = { command = "Remove-Item -Recurse -Force build, dist; pnpm build" }
```

### A different command on one OS

A command is written in a shell's language, and shells differ by OS: `rm -rf build` is zsh, `Remove-Item -Recurse build` is PowerShell. `macos`, `linux` and `windows` say what runs instead on that OS, the way VS Code's `tasks.json` does:
- They take `command` (or `steps`) and `shell`. A variant with its own command runs in that OS's default shell unless it names one: `windows = { command = "…", shell = "bash" }` runs in Git Bash.
- `windows = { shell = "cmd" }` changes only the shell.
- They sync with the bookmark, and server bookmarks don't take them (they run in the server's shell).
- In the app, the command field's **Different on Windows, macOS or Linux?** adds one, starting from the command for all systems.
- The editor and `bk doctor` flag steps written for another shell than the one that runs them, here and on the OSes of your other devices: `export A=1` that would run in PowerShell, `$env:A` that would run in zsh.
- A bookmark naming a POSIX shell that isn't installed (`zsh` from a Mac on Windows, `/bin/zsh` on a Linux box without it) runs in bash instead, with a note. On Windows that's Git Bash. Likewise, `powershell` means `pwsh` off Windows.

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

## Sync between devices

Keep bookmarks, servers, settings and run history the same on all your machines through a Telegram bot that only you use. There's no server to run, and everything is end-to-end encrypted with your passphrase before it leaves a device.

1. In Telegram, message [@BotFather](https://t.me/BotFather), send `/newbot`, and copy the token.
2. Run `bk sync setup` (or **Settings → Sync → Set up sync** in the app): paste the token, press **Start** in your bot's chat, and choose a passphrase.
3. On each other device, run setup with the same token and passphrase.

From then on each device syncs by itself:
- **The app** syncs shortly after it starts, a few seconds after you change something, and when another device has sent changes. It checks every couple of minutes, and whenever you come back to the window.
- **The CLI** (for machines without the app) syncs in the background after a command when there's something to send, or when it hasn't synced for ten minutes. It never makes a command wait. Running a bookmark that isn't here yet (`bk train` right after making it on your laptop) syncs first.
- **Offline**, syncing backs off and tries again; Settings → Sync says why it can't.

**Sync now** (or `bk sync`, or the tray menu) syncs right away. `bk sync auto off` (or the switch in Settings) makes a device sync only when you ask. `bk sync status` lists your devices with their OS and last change, and what's waiting to be sent; `bk sync forget` stops syncing on one device. History shows which device each run came from.

How it works:
- **One snapshot:** your bot's chat holds a single pinned, encrypted snapshot (Argon2id + XChaCha20-Poly1305). Each sync downloads it, merges, and uploads a new one if this device had changes.
- **Merging:** each bookmark, server and setting is merged on its own, and the most recent change wins, deletions included. Run history from all devices is combined.
- **Settings:** shell, pop-out terminal, ssh program and hotkey are kept separately per OS, so a Windows setting never lands on a Mac. Theme and the rest are shared.
- **Not synced:** project `.bk.toml` files (they live in their repos), trust approvals, the Inbox, launch-at-login, and [folder mappings](#folders-on-this-device).
- **Per-OS commands** (`windows = { … }`) sync with their bookmark. A device on an older Clibooker keeps them unless it edits that bookmark itself, so update every device to use them.
- **The bot token and derived key** stay in this device's data folder (`sync/config.json`, readable only by you). The passphrase is never stored, and it can't be recovered.

### Folders on this device

A bookmark synced from your Mac says `cwd = "~/work/sam2"`; on your Windows PC that project is in `D:\dev\sam2`. Changing the bookmark there would break it on the Mac, so each device keeps a small map of its own instead (`folders.toml` in its data folder, never synced):

```toml
"~/work" = "D:/dev"
```

- **Fixing it:** the first time a bookmark's folder isn't found, the app offers the folder of the same name it found next to your other projects ("Use D:\dev\sam2"), or a picker. `bk NAME` asks the same question, and `bk locate NAME [DIR]` says it directly.
- **One answer covers its neighbours:** locating `~/work/ml/sam2` at `D:\dev\ml\sam2` maps `~/work` to `D:/dev`, so every bookmark under `~/work` is found too. When that mapping already serves other bookmarks, only this folder is mapped, so nothing that works breaks.
- **Only a fallback:** a folder that exists as written is always used as written.
- **Any OS's paths:** a Windows path is matched on a Mac and the other way round (`/` or `\`, drive letters without regard to case).
- **Seeing them:** Settings → Folders on this device, or `bk folders`, lists the mappings and how many bookmarks each serves. The bookmark list, `bk ls`, `bk show` and `bk doctor` say when a folder isn't on this device or comes through a mapping.

Paths under your home folder are stored as `~/…` with `/`, and project folders relative to the project, so most bookmarks need no mapping at all.

## Platforms

Bookmarks files are plain TOML and work on every OS. Paths under your home folder are stored as `~/…`. Each platform behaves a little differently:

| | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Bookmarks file | `~/.config/bk/bookmarks.toml` | `~/.config/bk/bookmarks.toml` (`$XDG_CONFIG_HOME`) | `%APPDATA%\bk\bookmarks.toml` |
| History, trust, Inbox | `~/.local/share/bk` | `~/.local/share/bk` (`$XDG_DATA_HOME`) | `%LOCALAPPDATA%\bk` |
| Steps run in | `$SHELL` if zsh/bash, else `sh` | same | PowerShell (`pwsh` if installed) |
| Other step shells | any (`shell = "fish"`) | any | `cmd`, Git Bash (`shell = "bash"`), `powershell` |
| A POSIX shell that isn't installed | bash, else `sh` | same | Git Bash |
| `bk go`, Open in terminal | POSIX shell | POSIX shell | PowerShell |
| Open in terminal | Terminal, iTerm, Ghostty, Warp | `$TERMINAL`, GNOME Terminal, Konsole, kitty, … | Windows Terminal or a PowerShell window |

A bookmark can run a [different command on each OS](#a-different-command-on-one-os), and its folder can be [somewhere else on each device](#folders-on-this-device).

Envs and runtimes behave the same everywhere:
- Conda envs, venvs (`bin/` or `Scripts\`), uv and poetry all work.
- Runtimes are found where each OS keeps them:
  - Windows: nvm-windows, Volta, fnm, scoop, JDKs in Program Files, RubyInstaller.
  - Linux: `/usr/lib/jvm`, `/usr/local/go`, Linuxbrew, mise.

Known limits:
- **Linux, Wayland:** global shortcuts don't work. Bind a system shortcut to `Clibooker --palette` instead (the Settings page shows how).
- **Linux, GNOME:** the tray icon needs the AppIndicator extension. Without it, closing the window hides the app; open Clibooker again to bring it back, or turn off **Keep running when the window is closed**.
- **Windows, PowerShell 5.1:** it has no `&&`/`||`. Install PowerShell 7 (`winget install Microsoft.PowerShell`) for steps written that way.
- **Windows, SSH:** Windows' OpenSSH can't share connections, so server checks and keep-running progress are slower. Step progress for remote runs isn't shown; their exit status is.

## Development

```sh
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings   # SSH tests start a private sshd on 127.0.0.1
cd app && pnpm typecheck
cd app && pnpm tauri dev      # native app with hot reload
cd app && pnpm dev            # UI only, in a browser, against an in-memory mock backend
                              # (?platform=windows|linux shows another OS, ?synced starts with sync set up)
```

```
crates/bk-core     shared logic: model, store (toml_edit), env activation, plan, render, history, trust, doctor,
                   servers (ssh config, argv, status, discovery), remote (remote runs, tmux), script (run wrapper)
crates/bk-cli      the `bk` binary, its step runner (also the app's on Windows), and the zsh/bash/PowerShell
                   integrations (src/shell/)
app/src-tauri      Tauri backend: PTY runs (ConPTY + Job Objects on Windows), palette, tray, file watcher, commands
app/src            React UI (library, editor, terminal tabs, palette, history, inbox, doctor, settings)
```

## Contributing

Issues and pull requests are welcome. Before opening a PR, run the checks from [Development](#development) (`cargo test`, `clippy`, `pnpm typecheck`).

## License

[MIT](LICENSE)
