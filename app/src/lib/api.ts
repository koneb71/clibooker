import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Bookmark, CliStatus, DoctorReport, FolderRule, FolderScan, Inventory, Library, LocateResult, Preview, Reach, RecentCommand, RemoteFolder, RemoteInventory, RunInfo, RunRecord, ScriptScan, Server, ServerInfo, Settings, SyncContact, SyncResult, SyncStatus } from "./types";

export const inTauri = isTauri();

type Handler<T> = (payload: T) => void;

// Local event bus used by the browser mock.
const localHandlers = new Map<string, Set<Handler<unknown>>>();
export function emitLocal<T>(event: string, payload: T) {
  localHandlers.get(event)?.forEach((h) => h(payload));
}

/** Subscribe to a backend event. Returns an unsubscribe function. */
export function on<T>(event: string, handler: Handler<T>): () => void {
  if (inTauri) {
    const p = listen<T>(event, (e) => handler(e.payload));
    return () => void p.then((un) => un());
  }
  const set = localHandlers.get(event) ?? new Set();
  set.add(handler as Handler<unknown>);
  localHandlers.set(event, set);
  return () => set.delete(handler as Handler<unknown>);
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri) {
    const { mockCall } = await import("./mock");
    return mockCall(cmd, args) as Promise<T>;
  }
  return invoke<T>(cmd, args);
}

/** Output sink for a run: receives raw terminal bytes. */
export type OutputSink = (bytes: Uint8Array) => void;

function toBytes(data: unknown): Uint8Array {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (data instanceof Uint8Array) return data;
  if (Array.isArray(data)) return Uint8Array.from(data as number[]);
  return new TextEncoder().encode(String(data));
}

export const api = {
  getLibrary: () => call<Library>("get_library"),
  saveBookmark: (file: string | null, originalName: string | null, name: string, bookmark: Bookmark) =>
    call<string>("save_bookmark", { file, originalName, name, bookmark }),
  deleteBookmark: (id: string) => call<void>("delete_bookmark", { id }),
  setPinned: (id: string, pinned: boolean) => call<void>("set_pinned", { id, pinned }),
  getInventory: () => call<Inventory>("get_inventory"),
  scanFolder: (path: string) => call<FolderScan>("scan_folder", { path }),
  scanScripts: (dir: string, server?: string | null) => call<ScriptScan>("scan_scripts", { dir, server: server ?? null }),
  preview: (file: string | null, name: string, bookmark: Bookmark, params: Record<string, string>) =>
    call<Preview>("preview_bookmark", { file, name, bookmark, params }),
  oneLiner: (id: string, params: Record<string, string>) => call<string>("one_liner", { id, params }),
  saveSettings: (settings: Settings) => call<void>("save_settings", { settings }),
  reloadShellEnv: () => call<number>("reload_shell_env"),
  getHistory: (limit: number, name?: string) => call<RunRecord[]>("get_history", { limit, name: name ?? null }),
  addProject: (dir: string) => call<string>("add_project", { dir }),
  removeProject: (file: string) => call<void>("remove_project", { file }),
  trustProject: (file: string) => call<void>("trust_project", { file }),

  startRun: (
    id: string,
    params: Record<string, string>,
    opts: { cols: number; rows: number; background?: boolean; extraArgs?: string[] },
  ) =>
    call<RunInfo>("start_run", {
      id,
      params,
      extraArgs: opts.extraArgs ?? [],
      cols: opts.cols,
      rows: opts.rows,
      background: opts.background ?? false,
    }),
  listRuns: () => call<RunInfo[]>("list_runs"),
  attachRun: async (runId: string, key: string, sink: OutputSink) => {
    if (!inTauri) {
      const { mockAttach } = await import("./mock");
      return mockAttach(runId, key, sink);
    }
    const ch = new Channel<unknown>();
    ch.onmessage = (data) => sink(toBytes(data));
    await invoke("attach_run", { runId, key, onOutput: ch });
  },
  detachRun: (runId: string, key: string) => call<void>("detach_run", { runId, key }),
  writeRun: (runId: string, data: string) => call<void>("write_run", { runId, data }),
  resizeRun: (runId: string, cols: number, rows: number) => call<void>("resize_run", { runId, cols, rows }),
  stopRun: (runId: string) => call<void>("stop_run", { runId }),
  removeRun: (runId: string) => call<void>("remove_run", { runId }),
  detachTmux: (runId: string) => call<void>("detach_tmux", { runId }),

  openInTerminal: (id: string, params: Record<string, string>) => call<void>("open_in_terminal", { id, params }),
  listServers: () => call<ServerInfo[]>("list_servers"),
  saveServer: (originalName: string | null, name: string, server: Server) =>
    call<void>("save_server", { originalName, name, server }),
  deleteServer: (name: string) => call<void>("delete_server", { name }),
  hideServer: (name: string, hidden: boolean) => call<void>("hide_server", { name, hidden }),
  checkServer: (name: string) => call<Reach>("check_server", { name }),
  serverInventory: (name: string, refresh = false) => call<RemoteInventory>("server_inventory", { name, refresh }),
  hostFingerprints: (name: string) => call<string[]>("host_fingerprints", { name }),
  trustHost: (name: string) => call<Reach>("trust_host", { name }),
  connectServer: (name: string, cols: number, rows: number) => call<RunInfo>("connect_server", { name, cols, rows }),
  shellHere: (id: string, cols: number, rows: number) => call<RunInfo>("shell_here", { id, cols, rows }),
  scanRemoteFolder: (name: string, dir: string) => call<RemoteFolder>("scan_remote_folder", { name, dir }),
  paletteConnect: (name: string) => call<void>("palette_connect", { name }),
  cliStatus: () => call<CliStatus>("cli_status"),
  syncStatus: () => call<SyncStatus>("sync_status"),
  syncCheckToken: (token: string) => call<string>("sync_check_token", { token }),
  syncWaitForStart: (token: string) => call<SyncContact>("sync_wait_for_start", { token }),
  syncFinishSetup: (token: string, bot: string, contact: SyncContact, passphrase: string, device: string) =>
    call<void>("sync_finish_setup", { token, bot, chatId: contact.chat_id, userId: contact.user_id, passphrase, device }),
  syncNow: () => call<SyncResult>("sync_now"),
  syncForget: () => call<void>("sync_forget"),
  syncSetAuto: (on: boolean) => call<void>("sync_set_auto", { on }),
  foldersList: () => call<FolderRule[]>("folders_list"),
  foldersSet: (shared: string, local: string) => call<string | null>("folders_set", { shared, local }),
  foldersRemove: (shared: string) => call<boolean>("folders_remove", { shared }),
  locateFolder: (id: string, dir: string) => call<LocateResult>("locate_folder", { id, dir }),
  installCli: () => call<string>("install_cli"),
  runDoctor: () => call<DoctorReport>("run_doctor"),
  getInbox: (limit: number) => call<RecentCommand[]>("get_inbox", { limit }),
  clearInbox: () => call<void>("clear_inbox"),

  setHotkey: (hotkey: string) => call<void>("set_hotkey", { hotkey }),
  hotkeyError: () => call<string | null>("hotkey_error"),
  hidePalette: () => call<void>("hide_palette"),
  showMainWindow: () => call<void>("show_main_window"),
  paletteRun: (id: string, params: Record<string, string>) => call<void>("palette_run", { id, params }),
  setDockVisible: (visible: boolean) => call<void>("set_dock_visible", { visible }),
};

/** `~/x` → an absolute path the native dialogs understand. */
export async function expandHome(p: string | undefined): Promise<string | undefined> {
  if (!p || !inTauri || !(p === "~" || p.startsWith("~/") || p.startsWith("~\\"))) return p;
  const { homeDir } = await import("@tauri-apps/api/path");
  const home = (await homeDir()).replace(/[\\/]+$/, "");
  const full = home + p.slice(1);
  return home.includes("\\") ? full.replace(/\//g, "\\") : full;
}

export async function pickFolder(defaultPath?: string): Promise<string | null> {
  if (!inTauri) return window.prompt("Folder path", defaultPath ?? "~/") || null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const res = await open({ directory: true, multiple: false, defaultPath: await expandHome(defaultPath) });
  return typeof res === "string" ? res : null;
}

export async function readClipboard(): Promise<string> {
  if (inTauri) {
    const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
    return readText();
  }
  return navigator.clipboard.readText();
}

export async function copyText(text: string) {
  if (inTauri) {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
  } else {
    await navigator.clipboard.writeText(text);
  }
}
