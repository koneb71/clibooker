import { create } from "zustand";
import { toast } from "sonner";
import { api } from "../lib/api";
import type { RunInfo } from "../lib/types";

const HEIGHT_KEY = "clibooker.panelHeight";

function loadHeight(): number {
  try {
    const v = Number(localStorage.getItem(HEIGHT_KEY));
    return v >= 120 ? v : 300;
  } catch {
    return 300;
  }
}

interface RunsState {
  runs: Record<string, RunInfo>;
  /** Open terminal tabs, in order. */
  tabs: string[];
  active: string | null;
  panelOpen: boolean;
  panelHeight: number;
  /** Last measured terminal size, used for new PTYs before their tab mounts. */
  termSize: { cols: number; rows: number };

  upsert: (info: RunInfo, opts?: { focus?: boolean }) => void;
  activate: (id: string) => void;
  close: (id: string) => Promise<void>;
  setPanelOpen: (open: boolean) => void;
  setPanelHeight: (h: number) => void;
  setTermSize: (cols: number, rows: number) => void;
  start: (entryId: string, params: Record<string, string>, opts?: { background?: boolean }) => Promise<RunInfo | null>;
  /** Interactive SSH session to a server. */
  connect: (server: string) => Promise<RunInfo | null>;
  /** Shell on the server in a remote bookmark's folder and env. */
  shellHere: (entryId: string) => Promise<RunInfo | null>;
  /** One of OpenSSH's tools for a saved key, in a tab of its own. */
  task: (what: string, start: (cols: number, rows: number) => Promise<RunInfo>) => Promise<RunInfo | null>;
}

export const useRuns = create<RunsState>((set, get) => ({
  runs: {},
  tabs: [],
  active: null,
  panelOpen: false,
  panelHeight: loadHeight(),
  termSize: { cols: 120, rows: 24 },

  upsert: (info, opts) =>
    set((s) => {
      // Events can overtake the start_run reply; never let a stale "running" snapshot win.
      const prev = s.runs[info.id];
      if (prev && info.status === "running" && (prev.status !== "running" || prev.step > info.step)) info = prev;
      const tabs = s.tabs.includes(info.id) ? s.tabs : [...s.tabs, info.id];
      const focus = opts?.focus ?? false;
      return {
        runs: { ...s.runs, [info.id]: info },
        tabs,
        active: focus || !s.active ? info.id : s.active,
        panelOpen: focus ? true : s.panelOpen,
      };
    }),

  activate: (id) => set({ active: id, panelOpen: true }),

  close: async (id) => {
    const run = get().runs[id];
    if (run?.status === "running") {
      await api.stopRun(id);
      return;
    }
    await api.removeRun(id).catch(() => {});
    set((s) => {
      const tabs = s.tabs.filter((t) => t !== id);
      const idx = s.tabs.indexOf(id);
      const active = s.active === id ? (tabs[Math.min(idx, tabs.length - 1)] ?? null) : s.active;
      const runs = { ...s.runs };
      delete runs[id];
      return { tabs, active, runs, panelOpen: tabs.length ? s.panelOpen : false };
    });
  },

  setPanelOpen: (open) => set({ panelOpen: open }),
  setPanelHeight: (h) => {
    const height = Math.max(120, Math.min(h, window.innerHeight - 160));
    try {
      localStorage.setItem(HEIGHT_KEY, String(height));
    } catch {
      /* storage unavailable */
    }
    set({ panelHeight: height });
  },
  setTermSize: (cols, rows) => set({ termSize: { cols, rows } }),

  connect: async (server) => {
    const { cols, rows } = get().termSize;
    try {
      const info = await api.connectServer(server, cols, rows);
      get().upsert(info, { focus: true });
      return info;
    } catch (e) {
      toast.error(`Could not connect to ${server}`, { description: String(e) });
      return null;
    }
  },

  shellHere: async (entryId) => {
    const { cols, rows } = get().termSize;
    try {
      const info = await api.shellHere(entryId, cols, rows);
      get().upsert(info, { focus: true });
      return info;
    } catch (e) {
      toast.error("Could not open a shell", { description: String(e) });
      return null;
    }
  },

  task: async (what, start) => {
    const { cols, rows } = get().termSize;
    try {
      const info = await start(cols, rows);
      get().upsert(info, { focus: true });
      return info;
    } catch (e) {
      toast.error(`Could not ${what}`, { description: String(e) });
      return null;
    }
  },

  start: async (entryId, params, opts) => {
    const { cols, rows } = get().termSize;
    try {
      const info = await api.startRun(entryId, params, { cols, rows, background: opts?.background });
      get().upsert(info, { focus: !opts?.background });
      return info;
    } catch (e) {
      toast.error("Could not start", { description: String(e) });
      return null;
    }
  },
}));
