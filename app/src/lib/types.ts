// Mirrors of the Rust types (crates/bk-core + app/src-tauri/src/commands.rs).

export type EnvSpec =
  | { type: "inherit" }
  | { type: "none" }
  | { type: "conda"; name?: string; prefix?: string }
  | { type: "venv"; path: string }
  | { type: "uv"; sync?: boolean }
  | { type: "poetry" }
  | { type: "pyenv"; version: string }
  | { type: "auto" }
  | { type: "mise" };

export type EnvType = EnvSpec["type"];

export interface StepTable {
  run: string;
  name?: string;
  cwd?: string;
  continue_on_error?: boolean;
}

/** A plain string when only `run` is set. */
export type Step = string | StepTable;

export type Os = "macos" | "linux" | "windows";
export const OSES: Os[] = ["macos", "linux", "windows"];

/** What runs differently on one OS: the command (or steps) and the shell they're written for. */
export interface OsVariant {
  command?: string;
  steps?: Step[];
  shell?: string;
}

export type RuntimeLang = "node" | "java" | "ruby" | "go" | "rust" | "php";
export const RUNTIME_LANGS: RuntimeLang[] = ["node", "java", "ruby", "go", "rust", "php"];

export interface Bookmark {
  description?: string;
  cwd?: string;
  env?: EnvSpec;
  /** Language runtimes: version prefix, exact version, "auto" (follow .nvmrc etc.) or "system". */
  runtimes?: Partial<Record<RuntimeLang, string>>;
  /** Extra PATH entries, relative to cwd. */
  path?: string[];
  /** Run on this SSH server (server name, ~/.ssh/config alias, or user@host[:port]). */
  server?: string;
  /** Port forwards while it runs (remote only), e.g. "8888:127.0.0.1:8888". */
  forward?: string[];
  /** Keep running in tmux on the server (remote only). */
  detach?: boolean;
  command?: string;
  steps?: Step[];
  /** A different command, steps or shell on one OS (`windows = { command = "…" }`). */
  macos?: OsVariant;
  linux?: OsVariant;
  windows?: OsVariant;
  tags?: string[];
  dotenv?: string;
  vars?: Record<string, string>;
  shell?: string;
  confirm?: boolean;
  notify?: boolean;
  pinned?: boolean;
}

export interface Param {
  name: string;
  default: string | null;
}

export interface RunRecord {
  id: string;
  started_ms: number;
  duration_ms: number;
  name: string;
  entry_id: string;
  source: string;
  cwd: string;
  env: string;
  params: Record<string, string>;
  commands: string[];
  exit_code: number | null;
  server?: string;
  /** The device it ran on (absent in older records). */
  device?: string;
}

export interface Entry {
  id: string;
  name: string;
  scope: "global" | "project";
  file: string;
  project_dir: string | null;
  cwd: string | null;
  /** Where the folder is on this device; null for server bookmarks and ones without a folder. */
  folder: FolderInfo | null;
  bookmark: Bookmark;
  params: Param[];
  summary: string;
  shadowed: boolean;
  trusted: boolean;
  last_run: RunRecord | null;
}

export interface FolderInfo {
  /** As written (`here`), through a mapping on this device (`mapped`), or not here (`missing`). */
  state: "here" | "mapped" | "missing";
  /** On this device, with `~` (as written when missing). */
  path: string;
  via: { shared: string; local: string } | null;
  /** The OS a missing folder's path was clearly written on. */
  foreign: string | null;
  /** A folder on this device that may be the missing one. */
  suggestion: string | null;
}

export interface FolderRule {
  shared: string;
  local: string;
  local_exists: boolean;
  used_by: number;
}

export interface LocateResult {
  shared: string;
  local: string;
  replaced: string | null;
  /** Other bookmarks this found too. */
  fixed: string[];
}

export interface Settings {
  shell?: string;
  history?: boolean;
  hotkey?: string;
  terminal?: string;
  theme?: "system" | "light" | "dark";
  launch_at_login?: boolean;
  show_in_dock?: boolean;
  terminal_font_size?: number;
  ssh?: string;
  /** Keep running in the menu bar / tray when the window closes (default true). */
  keep_running?: boolean;
  /** The app looks for updates by itself (default true; per device). */
  check_updates?: boolean;
  /** ~/.ssh/config hosts left out of the server lists (they still work by name). */
  hidden_servers?: string[];
}

export interface Project {
  file: string;
  dir: string;
  name: string;
}

export interface Library {
  settings: Settings;
  entries: Entry[];
  errors: { file: string; message: string }[];
  global_file: string;
  home: string;
  projects: Project[];
  platform: "macos" | "linux" | "windows";
  default_hotkey: string;
  default_terminal: string;
  /** The user's shell for setup instructions: zsh, bash, powershell, ... */
  shell: string;
  /** The shell steps run in when neither the bookmark nor settings pick one. */
  default_step_shell: string;
  /** This device's name (the one it syncs as). */
  device: string;
}

export interface CondaEnv {
  name: string;
  prefix: string;
}

export interface RuntimeInstall {
  version: string;
  home: string;
  source: string;
}

export interface Inventory {
  conda_base: string | null;
  conda_envs: CondaEnv[];
  pyenv_versions: string[];
  tools: { conda: boolean; uv: boolean; poetry: boolean; pyenv: boolean; mise: boolean };
  runtimes: Partial<Record<RuntimeLang, RuntimeInstall[]>>;
}

export interface FolderScan {
  exists: boolean;
  detected: EnvSpec | null;
  venvs: string[];
  has_uv_lock: boolean;
  has_poetry_lock: boolean;
  dotenv_files: string[];
  conda_env_file: string | null;
  python_version: string | null;
  /** lang → [version, file it came from] */
  runtime_hints: Partial<Record<RuntimeLang, [string, string]>>;
  package_manager: string | null;
  script_count: number;
}

export interface PlannedStep {
  label: string;
  command: string;
  cwd: string;
  continue_on_error: boolean;
  synthetic: boolean;
}

export interface PlanSummary {
  name: string;
  cwd: string;
  shell: string;
  env_label: string;
  env_description: string;
  env_changes: string[];
  steps: PlannedStep[];
  notes: string[];
}

export interface Preview {
  one_liner: string | null;
  plan: PlanSummary | null;
  error: string | null;
  params: Param[];
  /** Notes on what runs here, and steps written for another shell than the one that runs them. */
  warnings: string[];
}

export type RunStatus = "running" | "succeeded" | "failed" | "stopped" | "disconnected" | "detached";

export interface ForwardInfo {
  spec: string;
  local_port: number;
  remote: string;
  error: string | null;
}

export interface RunInfo {
  id: string;
  /** A task is one of OpenSSH's tools for a saved key (making, unlocking, installing one). */
  kind: "bookmark" | "session" | "task";
  server: string | null;
  forwards: ForwardInfo[];
  /** tmux session for runs that keep running on the server. */
  tmux: string | null;
  reattached: boolean;
  name: string;
  entry_id: string;
  status: RunStatus;
  exit_code: number | null;
  started_ms: number;
  ended_ms: number | null;
  step: number;
  total_steps: number;
  step_label: string;
  cwd: string;
  env_label: string;
  params: Record<string, string>;
  background: boolean;
}

export interface DoctorCheck {
  entry_id: string;
  name: string;
  file: string;
  env: string;
  problems: string[];
  warnings: string[];
  /** The folder isn't on this device. */
  missing_folder?: { written: string; foreign: string | null; suggestion: string | null };
}

export interface DoctorReport {
  checks: DoctorCheck[];
  file_errors: string[];
}

export interface RecentCommand {
  ts: number;
  status: number;
  cwd: string;
  env: EnvSpec | null;
  command: string;
}

export interface SyncStatus {
  configured: boolean;
  device: string | null;
  bot: string | null;
  last_sync_ms: number | null;
  last_summary: string | null;
  default_device: string;
  /** Syncs by itself. */
  auto: boolean;
  syncing: boolean;
  /** Changes here not sent yet. */
  pending: number;
  /** Why automatic syncing is failing, if it is. */
  last_error: string | null;
  devices: SyncDevice[];
}

export interface SyncDevice {
  name: string;
  /** macOS, Linux, Windows */
  os: string;
  last_change_ms: number;
  version: string;
  this: boolean;
}

/** How a sync went (`sync-finished`). */
export interface SyncEvent {
  summary: string | null;
  error: string | null;
  /** Started by automatic syncing. */
  auto: boolean;
  /** Bookmarks, servers and settings changed here. */
  received: number;
  skipped: string[];
  warning: string | null;
}

export interface SyncContact {
  chat_id: number;
  user_id: number;
  name: string;
  /** The chat already holds synced data, so the passphrase must match it. */
  has_data: boolean;
}

export interface SyncResult {
  line: string;
  skipped: string[];
  warning: string | null;
}

export interface CliStatus {
  on_path: string | null;
  bundled: string | null;
  link: string;
  linked: boolean;
  link_dir_on_path: boolean;
}

export interface ProjectScript {
  source: string;
  name: string;
  command: string;
  detail: string | null;
  suggested_name: string;
  runtime: string | null;
}

export interface ScriptScan {
  dir: string;
  name: string;
  scripts: ProjectScript[];
  env: EnvSpec;
  runtimes: Partial<Record<RuntimeLang, string>>;
  project_file: string | null;
  server: string | null;
}

export interface Server {
  host: string;
  user?: string;
  port?: number;
  identity?: string;
  jump?: string;
  forward?: string[];
  options?: Record<string, string>;
  description?: string;
  shell?: string;
  login?: boolean;
}

export type Reach =
  | { state: "reachable"; ms: number }
  | { state: "auth-required" }
  | { state: "unknown-host-key" }
  | { state: "changed-host-key" }
  | { state: "unreachable"; reason: string };

export interface ServerInfo {
  name: string;
  source: "clibooker" | "ssh-config" | "literal";
  destination: string;
  description: string | null;
  server: Server | null;
  status: Reach | null;
  scanned_ms: number | null;
  bookmarks: number;
}

export interface RemoteInventory {
  probed_ms: number;
  os: string;
  shell: string;
  home: string;
  tools: string[];
  conda_base: string | null;
  conda_envs: CondaEnv[];
  runtimes: Partial<Record<string, RuntimeInstall[]>>;
}

export interface RemoteFolder {
  files: string[];
  runtime_hints: Partial<Record<RuntimeLang, string>>;
  scripts: ProjectScript[];
}

export const ALLOWED_SSH_OPTIONS = [
  "ServerAliveInterval",
  "ServerAliveCountMax",
  "ConnectTimeout",
  "Compression",
  "StrictHostKeyChecking",
  "AddKeysToAgent",
  "UseKeychain",
  "IdentitiesOnly",
  "ConnectionAttempts",
];

/** A saved SSH key (servers use it as `identity = "key:NAME"`). Nothing secret. */
export interface SshKey {
  name: string;
  /** `ED25519`, `RSA 4096`, … */
  kind: string;
  /** `SHA256:…` */
  fingerprint: string;
  comment: string;
  /** It has a passphrase. */
  encrypted: boolean;
  /** The public key line. */
  public: string;
  /** ssh-agent holds it: its passphrase was typed and is remembered. */
  unlocked: boolean;
  used_by: string[];
  /** Another device has a different key under this name. */
  clash: boolean;
  file: string;
}

export interface KeysInfo {
  keys: SshKey[];
  /** Why ssh-agent can't remember passphrases here, and (Windows) the command that starts it. */
  no_agent: { why: string; fix: string | null } | null;
  /** Sync is set up, so keys go to your other devices. */
  synced: boolean;
  folder: string;
}

export type KeyType = "ed25519" | "rsa";

/** `key:NAME` → `NAME`. */
export function savedKeyName(identity?: string | null): string | null {
  return identity?.startsWith("key:") ? identity.slice(4) : null;
}

/** The app's own updates. */
export interface UpdateStatus {
  current: string;
  available: { version: string; date: string | null } | null;
  /** This install can update itself (otherwise: download the new version). */
  can_install: boolean;
  /** How it was installed. */
  bundle: "app" | "nsis" | "msi" | "appimage" | "deb" | "rpm" | null;
  checking: boolean;
  installing: boolean;
  last_checked_ms: number | null;
  error: string | null;
  auto: boolean;
  /** The latest release's page. */
  releases_url: string;
}
