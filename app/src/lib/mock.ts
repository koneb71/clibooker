// In-memory backend used when the UI runs in a plain browser (vite dev without Tauri).
// Lets the layout be developed and checked without the native shell.

import { emitLocal, type OutputSink } from "./api";
import { platform } from "./platform";
import type { Bookmark, Entry, FolderInfo, Library, Param, RunInfo, RunRecord, Settings, SshKey, Step } from "./types";

const GLOBAL = "~/.config/bk/bookmarks.toml";
const PROJECT = "~/work/fastcap/.bk.toml";

const now = Date.now();
const mockPlatform = platform;
const books: Record<string, { file: string; bookmark: Bookmark }> = {
  train: {
    file: GLOBAL,
    bookmark: {
      description: "Fine-tune SAM2 on the shop dataset",
      cwd: "~/work/sam2",
      env: { type: "conda", name: "sam2" },
      command: "python train.py --epochs {{epochs:10}} --lr {{lr:3e-4}} --data {{dataset}}",
      tags: ["ml", "sam"],
      vars: { CUDA_VISIBLE_DEVICES: "0" },
      runtimes: { node: "22" },
      notify: true,
      pinned: true,
    },
  },
  "scrape-yelp": {
    file: GLOBAL,
    bookmark: {
      cwd: "~/work/yelp_scraper",
      env: { type: "conda", name: "yelp_scraper" },
      command: "python -m scraper.run --city {{city:manila}} --pages 20",
      tags: ["scrapers"],
      dotenv: ".env",
    },
  },
  dev: {
    file: PROJECT,
    bookmark: {
      description: "Migrate, seed and start the API",
      env: { type: "uv" },
      steps: ["alembic upgrade head", "python scripts/seed.py", { run: "uvicorn app.main:app --reload --port {{port:8000}}", name: "API server" }],
      tags: ["api"],
      pinned: true,
    },
  },
  "train-a100": {
    file: GLOBAL,
    bookmark: {
      description: "Full training run on the lab A100",
      server: "gpu",
      cwd: "~/sam2",
      env: { type: "conda", name: "sam2" },
      command: "python train.py --epochs {{epochs:50}}",
      forward: ["6006:127.0.0.1:6006"],
      detach: true,
      tags: ["ml"],
    },
  },
  "pdf-crawl": {
    file: GLOBAL,
    bookmark: {
      cwd: "~/work/pdfcrawler",
      env: { type: "venv", path: ".venv" },
      command: "python crawl.py --out data/",
    },
  },
  "disk-usage": {
    file: GLOBAL,
    bookmark: { command: "du -sh ~/work/* | sort -h | tail -20", tags: ["sys"] },
  },
  // Synced from a device on another OS: its folder isn't here until it's located.
  report: {
    file: GLOBAL,
    bookmark: {
      description: "Weekly metrics report",
      cwd: mockPlatform === "windows" ? "/Users/you/work/reports" : "D:\\work\\reports",
      env: { type: "uv" },
      command: "python make_report.py --week {{week:last}}",
      tags: ["ml"],
    },
  },
  // Found through a folder mapping on this device.
  "api-tests": {
    file: GLOBAL,
    bookmark: { cwd: "/home/you/projects/api", env: { type: "venv", path: ".venv" }, command: "pytest -x tests" },
  },
  // A different command on Windows.
  clean: {
    file: GLOBAL,
    bookmark: {
      cwd: "~/work/fastcap",
      command: "rm -rf build dist && pnpm build",
      windows: { command: "Remove-Item -Recurse -Force build, dist; pnpm build" },
    },
  },
};

/** This device's folder mappings. */
const rules: { shared: string; local: string }[] = [{ shared: "/home/you/projects", local: "~/work" }];

const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
const isForeign = (p: string) => (mockPlatform === "windows" ? p.startsWith("/") : /^[A-Za-z]:[\\/]/.test(p));

function folderOf(file: string, b: Bookmark): FolderInfo | null {
  if (b.server) return null;
  if (!b.cwd) return file === GLOBAL ? null : { state: "here", path: "~/work/fastcap", via: null, foreign: null, suggestion: null };
  for (const r of rules) {
    if (norm(b.cwd).startsWith(norm(r.shared) + "/")) {
      return { state: "mapped", path: r.local + b.cwd.slice(r.shared.length).replace(/\\/g, "/"), via: r, foreign: null, suggestion: null };
    }
  }
  if (isForeign(b.cwd)) {
    const name = b.cwd.split(/[\\/]/).pop();
    return { state: "missing", path: b.cwd, via: null, foreign: mockPlatform === "windows" ? "macOS" : "Windows", suggestion: `~/work/${name}` };
  }
  return { state: "here", path: b.cwd, via: null, foreign: null, suggestion: null };
}

/** What runs on this OS: its variant's command, else the bookmark's. */
function stepsHere(b: Bookmark): Step[] {
  const v = b[mockPlatform];
  return v?.command ? [v.command] : v?.steps?.length ? v.steps : stepsOf(b);
}

const history: RunRecord[] = [
  rec("train", 0, now - 3600_000 * 5, 1_820_000),
  rec("dev", 1, now - 600_000, 4_000),
  rec("scrape-yelp", 0, now - 86400_000, 95_000),
  { ...rec("train", 0, now - 3600_000 * 26, 2_400_000), id: "other", device: "desktop-pc" },
];
let settings: Settings = { theme: "system", hotkey: "Alt+Space", terminal: "iterm" };
const deletedServers = new Set<string>();
const thisDevice = mockPlatform === "windows" ? "desktop-pc" : "MacBook-Pro";
// `?synced` starts the preview with sync set up.
let syncState: { device: string; bot: string; last_sync_ms: number | null; last_summary: string | null; auto: boolean } | null =
  typeof window !== "undefined" && new URLSearchParams(window.location.search).has("synced")
    ? { device: thisDevice, bot: "clibooker_sync_bot", last_sync_ms: now - 120_000, last_summary: "↓ 2 updated from desktop-pc", auto: true }
    : null;

function rec(name: string, exit: number, started: number, dur: number): RunRecord {
  const b = books[name];
  return {
    id: String(started),
    started_ms: started,
    duration_ms: dur,
    name,
    entry_id: `${b.file}#${name}`,
    source: "app",
    cwd: b.bookmark.cwd ?? "~",
    env: b.bookmark.env?.type ?? "inherit",
    params: {},
    commands: [b.bookmark.command ?? ""],
    exit_code: exit,
  };
}

function stepsOf(b: Bookmark): Step[] {
  return b.command ? [b.command] : (b.steps ?? []);
}

function paramsOf(b: Bookmark): Param[] {
  const out: Param[] = [];
  const re = /\{\{\s*([A-Za-z_]\w*)\s*(?::([^}]*?))?\s*\}\}/g;
  for (const s of stepsOf(b)) {
    const text = typeof s === "string" ? s : s.run;
    for (const m of text.matchAll(re)) {
      if (!out.some((p) => p.name === m[1])) out.push({ name: m[1], default: m[2] ?? null });
    }
  }
  return out;
}

function library(): Library {
  const entries: Entry[] = Object.entries(books).map(([name, { file, bookmark }]) => {
    const id = `${file}#${name}`;
    const first = stepsHere(bookmark);
    const folder = folderOf(file, bookmark);
    const firstText = first.length ? (typeof first[0] === "string" ? first[0] : first[0].run) : "";
    return {
      id,
      name,
      scope: file === GLOBAL ? "global" : "project",
      file,
      project_dir: file === GLOBAL ? null : "~/work/fastcap",
      cwd: folder?.path ?? bookmark.cwd ?? null,
      folder,
      bookmark,
      params: paramsOf(bookmark),
      summary: first.length > 1 ? `${firstText} (+${first.length - 1} more)` : firstText,
      shadowed: false,
      trusted: true,
      last_run: history.find((h) => h.entry_id === id) ?? null,
    };
  });
  return {
    settings,
    entries,
    errors: [],
    global_file: GLOBAL,
    home: "/Users/you",
    platform: mockPlatform,
    default_hotkey: mockPlatform === "macos" ? "Alt+Space" : "Ctrl+Alt+Space",
    default_terminal: { macos: "terminal", windows: "windows-terminal", linux: "auto" }[mockPlatform],
    shell: { macos: "zsh", windows: "powershell", linux: "bash" }[mockPlatform],
    default_step_shell: settings.shell ?? { macos: "zsh", windows: "pwsh", linux: "bash" }[mockPlatform],
    device: syncState?.device ?? thisDevice,
    projects: [{ file: PROJECT, dir: "~/work/fastcap", name: "fastcap" }],
  };
}

// ---- fake runs ----
const runs = new Map<string, { info: RunInfo; buf: string; sinks: Map<string, OutputSink> }>();
/** Saved SSH keys (`?agent=off` previews a device without ssh-agent). */
type MockKey = Omit<SshKey, "used_by" | "clash" | "file">;
const agentOff = typeof window !== "undefined" && new URLSearchParams(window.location.search).get("agent") === "off";
const sshKeys: MockKey[] = [
  { name: "github", kind: "ED25519", fingerprint: "SHA256:7Hq2mvR0c1pXk9sE4tYw8bZ3nLfA6uJdQ5oVg2iKx0M", comment: "you@MacBook-Pro", encrypted: false, unlocked: false,
    public: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGl0aHViLWZha2Uta2V5LWZvci10aGUtcHJldmlldw you@MacBook-Pro" },
  { name: "lab", kind: "ED25519", fingerprint: "SHA256:Lb4QeV1xR8sN2mK7cT0yHq9wZ3fJ6aPdU5oXg1iEk2Y", comment: "you@MacBook-Pro", encrypted: true, unlocked: !agentOff,
    public: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGxhYi1mYWtlLWtleS1mb3ItdGhlLXByZXZpZXctb25seQ you@MacBook-Pro" },
];
let gpuIdentity = "key:lab";

function fakeFingerprint(): string {
  const abc = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  return `SHA256:${Array.from({ length: 43 }, () => abc[Math.floor(Math.random() * abc.length)]).join("")}`;
}

/** A key task's tab: OpenSSH's prompts (answered by itself), then `done`. */
function startFakeTask(name: string, label: string, lines: string[], done: () => string | null): RunInfo {
  const runId = `run-${++runCounter}`;
  const info: RunInfo = {
    id: runId, kind: "task", server: null, tmux: null, reattached: false, forwards: [], name, entry_id: "",
    status: "running", exit_code: null, started_ms: Date.now(), ended_ms: null, step: 0, total_steps: 0, step_label: "",
    cwd: label, env_label: label, params: {}, background: false,
  };
  runs.set(runId, { info, buf: "", sinks: new Map() });
  let t = 0;
  lines.forEach((l) => setTimeout(() => push(runId, `${l}\r\n`), (t += 350)));
  setTimeout(() => {
    const said = done();
    if (said) push(runId, `\r\n${said}\r\n`);
    push(runId, `\r\n\x1b[32m✓ done in 1.4s\x1b[0m\r\n`);
    Object.assign(info, { status: "succeeded", exit_code: 0, ended_ms: Date.now() });
    emitLocal("run-updated", { ...info });
  }, t + 600);
  setTimeout(() => emitLocal("run-updated", { ...info }), 0);
  return { ...info };
}

/** `?update` previews a newer release (`?update=download`: one this copy can't install itself). */
const updateParam = typeof window !== "undefined" ? new URLSearchParams(window.location.search).get("update") : null;
let lastUpdateCheck: number | null = now - 25 * 60_000;
function updateStatus(installing = false) {
  return {
    current: "0.3.0",
    available: updateParam !== null ? { version: "0.4.0", date: new Date(now - 3600_000).toISOString() } : null,
    can_install: updateParam !== "download",
    bundle: mockPlatform === "linux" ? "deb" : mockPlatform === "windows" ? "nsis" : "app",
    checking: false,
    installing,
    last_checked_ms: lastUpdateCheck,
    error: null,
    auto: settings.check_updates ?? true,
    releases_url: "https://github.com/koneb71/clibooker/releases/latest",
  };
}

let runCounter = 0;
const enc = new TextEncoder();

function push(id: string, text: string) {
  const r = runs.get(id);
  if (!r) return;
  r.buf += text;
  r.sinks.forEach((s) => s(enc.encode(text)));
}

function startFakeRun(id: string, params: Record<string, string>): RunInfo {
  const [file, name] = id.split("#");
  const b = books[name].bookmark;
  const steps = stepsOf(b).map((s) => (typeof s === "string" ? s : s.run));
  const runId = `run-${++runCounter}`;
  const info: RunInfo = {
    id: runId,
    kind: "bookmark",
    server: b.server ?? null,
    tmux: b.detach ? `bk-${name}` : null,
    reattached: false,
    forwards: (b.forward ?? []).map((spec) => ({ spec, local_port: Number(spec.split(":")[0]), remote: spec.split(":").slice(1).join(":"), error: null })),
    name,
    entry_id: `${file}#${name}`,
    status: "running",
    exit_code: null,
    started_ms: Date.now(),
    ended_ms: null,
    step: 1,
    total_steps: steps.length,
    step_label: steps[0],
    cwd: b.cwd ?? "~",
    env_label: b.env?.type ?? "inherit",
    params,
    background: false,
  };
  runs.set(runId, { info, buf: "", sinks: new Map() });
  const render = (s: string) =>
    s.replace(/\{\{\s*(\w+)\s*(?::([^}]*?))?\s*\}\}/g, (_, k, d) => params[k] ?? d ?? "");
  let t = 0;
  const at = (ms: number, f: () => void) => setTimeout(f, (t += ms));
  at(50, () => push(runId, `\x1b[2m${name} · ${info.cwd} · ${info.env_label}\x1b[0m\r\n`));
  steps.forEach((s, i) => {
    at(150, () => {
      info.step = i + 1;
      info.step_label = s;
      emitLocal("run-updated", { ...info });
      push(runId, steps.length > 1 ? `\x1b[1;36m▶ [${i + 1}/${steps.length}]\x1b[0m \x1b[1m${render(s)}\x1b[0m\r\n` : `\x1b[2m$ ${render(s)}\x1b[0m\r\n`);
    });
    for (let k = 1; k <= 6; k++) {
      at(220, () => push(runId, `  working… ${"█".repeat(k * 4)}${"░".repeat(24 - k * 4)} ${Math.round((k / 6) * 100)}%\r\n`));
    }
  });
  at(200, () => {
    push(runId, `\x1b[32m✓ done in ${(t / 1000).toFixed(1)}s\x1b[0m\r\n`);
    info.status = "succeeded";
    info.exit_code = 0;
    info.ended_ms = Date.now();
    emitLocal("run-updated", { ...info });
  });
  setTimeout(() => emitLocal("run-updated", { ...info }), 0);
  return { ...info };
}

export function mockAttach(runId: string, key: string, sink: OutputSink) {
  const r = runs.get(runId);
  if (!r) return;
  if (r.buf) sink(enc.encode(r.buf));
  r.sinks.set(key, sink);
}

export async function mockCall(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 30));
  switch (cmd) {
    case "get_library":
      return library();
    case "save_bookmark": {
      const { file, originalName, name, bookmark } = args as { file: string | null; originalName: string | null; name: string; bookmark: Bookmark };
      if (originalName && originalName !== name) delete books[originalName];
      books[name] = { file: file ?? GLOBAL, bookmark };
      setTimeout(() => emitLocal("library-changed", null), 0);
      return `${file ?? GLOBAL}#${name}`;
    }
    case "delete_bookmark": {
      const name = String(args.id).split("#")[1];
      delete books[name];
      setTimeout(() => emitLocal("library-changed", null), 0);
      return null;
    }
    case "set_pinned": {
      const name = String(args.id).split("#")[1];
      books[name].bookmark.pinned = Boolean(args.pinned);
      setTimeout(() => emitLocal("library-changed", null), 0);
      return null;
    }
    case "get_inventory":
      return {
        conda_base: "/opt/miniconda3",
        conda_envs: ["base", "sam2", "yelp_scraper", "fastapi_backend", "pdfcrawler", "test"].map((name) => ({
          name,
          prefix: name === "base" ? "/opt/miniconda3" : `/opt/miniconda3/envs/${name}`,
        })),
        pyenv_versions: [],
        tools: { conda: true, uv: true, poetry: false, pyenv: false, mise: false },
        runtimes: {
          node: ["26.8.1", "23.10.0", "22.14.0", "18.17.1"].map((version) => ({ version, home: `~/.nvm/versions/node/v${version}`, source: version === "26.8.1" ? "homebrew" : "nvm" })),
          java: [{ version: "23", home: "/Library/Java/JavaVirtualMachines/jdk-23.jdk/Contents/Home", source: "system" }],
          rust: [{ version: "stable-aarch64-apple-darwin", home: "~/.rustup/toolchains/stable-aarch64-apple-darwin", source: "rustup" }],
          go: [],
          ruby: [],
          php: [{ version: "8.4.12", home: "/opt/homebrew/Cellar/php/8.4.12", source: "homebrew" }],
        },
      };
    case "scan_folder":
      return {
        exists: !isForeign(String(args.path)) || rules.some((r) => norm(String(args.path)).startsWith(norm(r.shared))),
        detected: { type: "venv", path: ".venv" },
        venvs: [".venv"],
        has_uv_lock: false,
        has_poetry_lock: false,
        dotenv_files: [".env"],
        conda_env_file: null,
        python_version: null,
        runtime_hints: { node: ["22", ".nvmrc"] },
        package_manager: "pnpm",
        script_count: 4,
      };
    case "preview_bookmark": {
      const b = args.bookmark as Bookmark;
      const steps = stepsHere(b).map((s) => (typeof s === "string" ? s : s.run));
      const base = stepsOf(b).map((s) => (typeof s === "string" ? s : s.run));
      const warnings: string[] = [];
      const posixy = base.find((s) => /rm -rf|export \w+=/.test(s));
      if (posixy && !b.windows?.command && !b.windows?.steps?.length && !b.shell) {
        const here = mockPlatform === "windows";
        warnings.push(
          `\`${posixy}\` looks like POSIX shell (bash/zsh) syntax, but it runs in PowerShell ${here ? "here" : "on Windows"}; add a Windows command, or set shell = "bash" (Git Bash)`,
        );
      }
      const envLabel = b.env?.type === "conda" ? `conda:${b.env.name}` : (b.env?.type ?? "inherit");
      const cwd = folderOf(GLOBAL, b)?.path ?? b.cwd ?? "~";
      return {
        one_liner: `( cd ${cwd} && ${steps.join(" && ")} )`,
        plan: steps.length
          ? {
              name: args.name,
              cwd,
              shell: "/bin/zsh",
              env_label: envLabel,
              env_description: b.env?.type === "conda" ? `conda env ${b.env.name} (/opt/miniconda3/envs/${b.env.name})` : "inherited environment",
              env_changes: b.env?.type === "conda" ? ["~PATH (+/opt/miniconda3/envs/x/bin)", "+CONDA_PREFIX"] : [],
              steps: steps.map((s) => ({ label: s, command: s, cwd, continue_on_error: false, synthetic: false })),
              notes: [],
            }
          : null,
        error: !steps.length
          ? "bookmark has no `command` or `steps`"
          : b.cwd && isForeign(b.cwd) && !rules.some((r) => norm(b.cwd!).startsWith(norm(r.shared)))
            ? `\`${args.name}\`'s folder ${b.cwd} is a ${mockPlatform === "windows" ? "macOS" : "Windows"} path, not on this device`
            : null,
        params: paramsOf(b),
        warnings,
      };
    }
    case "one_liner":
      return "( cd ~/work/sam2 && conda activate sam2 && python train.py --epochs 10 )";
    case "save_settings":
      settings = args.settings as Settings;
      setTimeout(() => emitLocal("library-changed", null), 0);
      return null;
    case "reload_shell_env":
      return 84;
    case "get_history":
      return history;
    case "run_doctor":
      return {
        file_errors: [],
        checks: Object.entries(books).map(([name, { file, bookmark }]) => ({
          entry_id: `${file}#${name}`,
          name,
          file,
          env: bookmark.env?.type ?? "inherit",
          problems:
            name === "pdf-crawl"
              ? ["no virtualenv at ~/work/pdfcrawler/.venv (create it with `python -m venv .venv`)"]
              : folderOf(file, bookmark)?.state === "missing"
                ? [`folder ${bookmark.cwd} is a ${mockPlatform === "windows" ? "macOS" : "Windows"} path, not on this device`]
                : [],
          warnings: [],
          missing_folder:
            folderOf(file, bookmark)?.state === "missing"
              ? { written: bookmark.cwd!, foreign: folderOf(file, bookmark)!.foreign, suggestion: folderOf(file, bookmark)!.suggestion }
              : undefined,
        })),
      };
    case "scan_scripts":
      return {
        dir: String(args.dir),
        name: String(args.dir).split("/").pop() ?? "project",
        env: { type: "inherit" },
        runtimes: { node: "auto" },
        project_file: null,
        server: (args.server as string | null) ?? null,
        scripts: [
          { source: "package.json", name: "dev", command: "pnpm run dev", detail: "vite", suggested_name: "dev", runtime: "node" },
          { source: "package.json", name: "build", command: "pnpm run build", detail: "tsc && vite build", suggested_name: "build", runtime: "node" },
          { source: "package.json", name: "test:e2e", command: "pnpm run 'test:e2e'", detail: "playwright test", suggested_name: "test-e2e", runtime: "node" },
          { source: "Makefile", name: "deploy", command: "make deploy", detail: "Ship to staging", suggested_name: "deploy", runtime: null },
        ],
      };
    case "sync_status":
      return {
        configured: !!syncState,
        device: syncState?.device ?? null,
        bot: syncState?.bot ?? null,
        last_sync_ms: syncState?.last_sync_ms ?? null,
        last_summary: syncState?.last_summary ?? null,
        default_device: thisDevice,
        auto: syncState?.auto ?? true,
        syncing: false,
        pending: 0,
        last_error: null,
        devices: syncState
          ? [
              { name: syncState.device, os: { macos: "macOS", windows: "Windows", linux: "Linux" }[mockPlatform], last_change_ms: syncState.last_sync_ms ?? now, version: "0.3.0", this: true },
              { name: mockPlatform === "windows" ? "MacBook-Pro" : "desktop-pc", os: mockPlatform === "windows" ? "macOS" : "Windows", last_change_ms: now - 3600_000 * 3, version: "0.3.0", this: false },
              { name: "gpu-box", os: "Linux", last_change_ms: now - 86400_000 * 4, version: "0.2.0", this: false },
            ]
          : [],
      };
    case "sync_set_auto":
      if (syncState) syncState.auto = Boolean(args.on);
      return null;
    case "folders_list":
      return rules.map((r) => ({
        ...r,
        local_exists: true,
        used_by: Object.values(books).filter(({ file, bookmark }) => folderOf(file, bookmark)?.via?.shared === r.shared).length,
      }));
    case "folders_set": {
      const i = rules.findIndex((r) => norm(r.shared) === norm(String(args.shared)));
      const replaced = i >= 0 ? rules.splice(i, 1)[0].local : null;
      rules.push({ shared: String(args.shared), local: String(args.local) });
      setTimeout(() => emitLocal("library-changed", null), 0);
      return replaced;
    }
    case "folders_remove": {
      const i = rules.findIndex((r) => norm(r.shared) === norm(String(args.shared)));
      if (i >= 0) rules.splice(i, 1);
      setTimeout(() => emitLocal("library-changed", null), 0);
      return i >= 0;
    }
    case "locate_folder": {
      const name = String(args.id).split("#")[1];
      const written = books[name].bookmark.cwd ?? "";
      const missingBefore = Object.entries(books).filter(([n, { file, bookmark }]) => n !== name && folderOf(file, bookmark)?.state === "missing");
      const parent = (p: string) => p.replace(/[\\/][^\\/]*$/, "");
      rules.push({ shared: parent(written).replace(/\\/g, "/"), local: parent(String(args.dir)) });
      setTimeout(() => emitLocal("library-changed", null), 0);
      return {
        shared: parent(written).replace(/\\/g, "/"),
        local: parent(String(args.dir)),
        replaced: null,
        fixed: missingBefore.filter(([, { file, bookmark }]) => folderOf(file, bookmark)?.state !== "missing").map(([n]) => n),
      };
    }
    case "sync_check_token":
      if (!String(args.token).includes(":")) throw "Telegram doesn't accept this bot token (Unauthorized)";
      return "clibooker_sync_bot";
    case "sync_wait_for_start":
      await new Promise((r) => setTimeout(r, 1500));
      return { chat_id: 42, user_id: 42, name: "You", has_data: false };
    case "sync_finish_setup":
      syncState = { device: String(args.device) || thisDevice, bot: String(args.bot), last_sync_ms: null, last_summary: null, auto: true };
      return null;
    case "sync_now":
      await new Promise((r) => setTimeout(r, 700));
      if (syncState) Object.assign(syncState, { last_sync_ms: Date.now(), last_summary: "↑ 12 sent · history +40" });
      return { line: "↑ 12 sent · history +40", skipped: [], warning: null };
    case "sync_forget":
      syncState = null;
      return null;
    case "delete_server":
      deletedServers.add(String(args.name));
      return null;
    case "hide_server": {
      const rest = (settings.hidden_servers ?? []).filter((h) => h !== args.name);
      settings = { ...settings, hidden_servers: args.hidden ? [...rest, String(args.name)] : rest };
      return null;
    }
    case "list_servers":
      return [
        { name: "gpu", source: "clibooker", destination: "ubuntu@10.0.0.5", description: "Lab A100", bookmarks: 1, scanned_ms: now - 3600_000,
          status: { state: "reachable", ms: 42 },
          server: { host: "10.0.0.5", user: "ubuntu", identity: gpuIdentity, forward: ["8888:127.0.0.1:8888"], description: "Lab A100" } },
        { name: "staging", source: "clibooker", destination: "deploy@staging.example.com:2222", description: null, bookmarks: 0, scanned_ms: null,
          status: { state: "unknown-host-key" }, server: { host: "staging.example.com", user: "deploy", port: 2222, jump: "gpu" } },
        { name: "orb", source: "ssh-config", destination: "orb", description: "from ~/.ssh/config", bookmarks: 0, scanned_ms: null, status: null, server: null },
        { name: "203.0.113.10", source: "ssh-config", destination: "203.0.113.10", description: "from ~/.ssh/config", bookmarks: 0, scanned_ms: null, status: null, server: null },
      ].filter((s) => !deletedServers.has(s.name) && !settings.hidden_servers?.includes(s.name));
    case "check_server":
      if (args.name === "staging") return { state: "unknown-host-key" };
      // `?auth` previews a server that wants a password or a key.
      return new URLSearchParams(window.location.search).has("auth") ? { state: "auth-required" } : { state: "reachable", ms: 38 };
    case "server_inventory":
      return {
        probed_ms: now - 3600_000, os: "Linux x86_64", shell: "/bin/bash", home: "/home/ubuntu",
        tools: ["bash", "tmux", "conda", "git", "nvidia-smi", "python3"],
        conda_base: "/home/ubuntu/miniconda3",
        conda_envs: [{ name: "base", prefix: "/home/ubuntu/miniconda3" }, { name: "sam2", prefix: "/home/ubuntu/miniconda3/envs/sam2" }, { name: "vllm", prefix: "/home/ubuntu/miniconda3/envs/vllm" }],
        runtimes: { node: [{ version: "20.11.1", home: "/home/ubuntu/.nvm/versions/node/v20.11.1", source: "nvm" }] },
      };
    case "host_fingerprints":
      return ["256 SHA256:Xq3k8rBv2ZL0Hd9nP7wqY1sTgUe4cFjM6oKaNiVb5Rs staging.example.com (ED25519)"];
    case "trust_host":
      return { state: "auth-required" };
    case "scan_remote_folder":
      return { files: ["train.py", ".nvmrc", "environment.yml"], runtime_hints: { node: "20" }, scripts: [] };
    case "connect_server":
    case "shell_here":
      return startFakeRun(`${GLOBAL}#train`, {});
    case "keys_list":
      return {
        keys: sshKeys.map((k) => ({ ...k, used_by: gpuIdentity === `key:${k.name}` ? ["gpu"] : [], clash: false, file: `~/.local/share/bk/keys/${k.name}` })),
        no_agent: agentOff
          ? mockPlatform === "windows"
            ? { why: "the OpenSSH Authentication Agent service isn't running", fix: "Get-Service ssh-agent | Set-Service -StartupType Automatic; Start-Service ssh-agent" }
            : { why: "ssh-agent isn't running here (SSH_AUTH_SOCK isn't set)", fix: null }
          : null,
        synced: !!syncState,
        folder: mockPlatform === "windows" ? "~\\AppData\\Local\\bk\\keys" : "~/.local/share/bk/keys",
      };
    case "keys_suggest_name": {
      const file = String(args.path).split(/[\\/]/).pop() ?? "key";
      const base = file.startsWith("id_") ? `${thisDevice}-${file.slice(3)}` : file.replace(/\.(pem|key)$/, "");
      return base.toLowerCase().replace(/[^a-z0-9_]+/g, "-").replace(/^-+|-+$/g, "") || "key";
    }
    case "keys_import": {
      const name = String(args.name);
      if (sshKeys.some((k) => k.name === name)) throw new Error(`a key named \`${name}\` is already saved`);
      const k: MockKey = { name, kind: "ED25519", fingerprint: fakeFingerprint(), comment: "you@desktop-pc", encrypted: !args.text, unlocked: false,
        public: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGltcG9ydGVkLWZha2Uta2V5 you@desktop-pc" };
      sshKeys.push(k);
      return k;
    }
    case "keys_rename": {
      const k = sshKeys.find((x) => x.name === args.from);
      if (k) k.name = String(args.to);
      if (gpuIdentity === `key:${args.from}`) {
        gpuIdentity = `key:${args.to}`;
        return ["gpu"];
      }
      return [];
    }
    case "keys_remove": {
      const i = sshKeys.findIndex((k) => k.name === args.name);
      if (i >= 0) sshKeys.splice(i, 1);
      return null;
    }
    case "keys_lock": {
      const k = sshKeys.find((x) => x.name === args.name);
      if (k) k.unlocked = false;
      return null;
    }
    case "keys_generate": {
      const name = String(args.name);
      const lines = args.passphrase
        ? ["Enter passphrase for \"" + name + "\" (empty for no passphrase): ", "Enter same passphrase again: "]
        : [];
      return startFakeTask(`New key ${name}`, "ssh-keygen", lines, () => {
        const encrypted = Boolean(args.passphrase);
        sshKeys.push({ name, kind: args.kind === "rsa" ? "RSA 4096" : "ED25519", fingerprint: fakeFingerprint(), comment: String(args.comment ?? "you@MacBook-Pro"),
          encrypted, unlocked: false, public: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG5ldy1mYWtlLWtleQ you@MacBook-Pro" });
        return `\x1b[32m✓ saved key ${name}\x1b[0m \x1b[2m(ED25519)\x1b[0m`;
      });
    }
    case "keys_unlock": {
      const name = String(args.name);
      return startFakeTask(`Unlock ${name}`, "ssh-add", [`Enter passphrase for ${name}: `, `Identity added: ${name}`], () => {
        const k = sshKeys.find((x) => x.name === name);
        if (k) k.unlocked = true;
        return "\x1b[32m✓ unlocked: you won't be asked for its passphrase again\x1b[0m";
      });
    }
    case "keys_passphrase": {
      const name = String(args.name);
      return startFakeTask(`Passphrase for ${name}`, "ssh-keygen", ["Enter new passphrase (empty for no passphrase): ", "Enter same passphrase again: ", "Your identification has been saved with the new passphrase."], () => {
        const k = sshKeys.find((x) => x.name === name);
        if (k) k.encrypted = true;
        return null;
      });
    }
    case "keys_install": {
      const [name, server] = [String(args.name), String(args.server)];
      return startFakeTask(`Install ${name} on ${server}`, `ssh ${server}`, [`ubuntu@10.0.0.5's password: `, "bk: added the key to ~/.ssh/authorized_keys on lab-a100"], () => {
        if (server === "gpu") gpuIdentity = `key:${name}`;
        return `\x1b[32m✓ ${server} now uses key ${name}\x1b[0m\r\n\x1b[32m✓ logs in with the key\x1b[0m`;
      });
    }
    case "update_status":
      return updateStatus();
    case "update_check":
      lastUpdateCheck = Date.now();
      return updateStatus();
    case "update_install": {
      emitLocal("update-status", updateStatus(true));
      const total = 9_400_000;
      for (let i = 1; i <= 10; i++) setTimeout(() => emitLocal("update-progress", [(total * i) / 10, total]), i * 250);
      await new Promise((r) => setTimeout(r, 3200));
      emitLocal("update-status", updateStatus());
      return null;
    }
    case "cli_status":
      return { on_path: null, bundled: "/Applications/Clibooker.app/Contents/MacOS/bk", link: "~/.local/bin/bk", linked: false, link_dir_on_path: true };
    case "install_cli":
      return "~/.local/bin/bk";
    case "get_inbox":
      return [
        { ts: now / 1000 - 120, status: 0, cwd: "~/work/sam2", env: { type: "conda", name: "sam2" }, command: "python eval.py --ckpt runs/best.pt --split val" },
        { ts: now / 1000 - 900, status: 1, cwd: "~/work/fastcap", env: { type: "venv", path: ".venv" }, command: "pytest -x tests/test_api.py" },
        { ts: now / 1000 - 4000, status: 0, cwd: "~/work/yelp_scraper", env: null, command: "docker compose up -d redis" },
      ];
    case "start_run":
      return startFakeRun(String(args.id), (args.params as Record<string, string>) ?? {});
    case "list_runs":
      return [...runs.values()].map((r) => r.info);
    case "stop_run":
    case "remove_run": {
      const r = runs.get(String(args.runId));
      if (cmd === "remove_run") runs.delete(String(args.runId));
      else if (r) {
        r.info.status = "stopped";
        r.info.exit_code = 130;
        emitLocal("run-updated", { ...r.info });
      }
      return null;
    }
    default:
      return null;
  }
}
