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
}

export interface Entry {
  id: string;
  name: string;
  scope: "global" | "project";
  file: string;
  project_dir: string | null;
  cwd: string | null;
  bookmark: Bookmark;
  params: Param[];
  summary: string;
  shadowed: boolean;
  trusted: boolean;
  last_run: RunRecord | null;
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
}

export interface Preview {
  one_liner: string | null;
  plan: PlanSummary | null;
  error: string | null;
  params: Param[];
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
  kind: "bookmark" | "session";
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
