import { create } from "zustand";
import type { EnvSpec } from "../lib/types";

export type Filter =
  | { kind: "all" }
  | { kind: "pinned" }
  | { kind: "recent" }
  | { kind: "global" }
  | { kind: "tag"; tag: string }
  | { kind: "project"; file: string };

export type View = "library" | "history" | "inbox" | "doctor" | "settings" | "server";

/** A pending run that needs params and/or confirmation first. */
export interface RunPrompt {
  entryId: string;
  params: Record<string, string>;
}

interface UiState {
  view: View;
  filter: Filter;
  search: string;
  /** Selected entry id, or "new" for the create form. */
  selected: string | null;
  /** Prefill for the "new bookmark" form (e.g. from the Inbox). */
  draftSeed: { command?: string; cwd?: string; name?: string; env?: EnvSpec } | null;
  runPrompt: RunPrompt | null;
  /** Folder whose scripts the import dialog shows (on `importServer` when set). */
  importDir: string | null;
  importServer: string | null;
  /** Server shown in the server view, or "new". */
  selectedServer: string | null;
  /** Server whose "remove" confirmation is open. */
  removingServer: string | null;
  dirty: boolean;

  setView: (v: View) => void;
  setFilter: (f: Filter) => void;
  setSearch: (s: string) => void;
  select: (id: string | null) => void;
  newBookmark: (seed?: UiState["draftSeed"]) => void;
  setRunPrompt: (p: RunPrompt | null) => void;
  setImportDir: (dir: string | null, server?: string | null) => void;
  selectServer: (name: string) => void;
  askRemoveServer: (name: string | null) => void;
  setDirty: (d: boolean) => void;
}

/** Ask before throwing away unsaved edits. */
function canLeave(dirty: boolean) {
  return !dirty || window.confirm("Discard unsaved changes?");
}

export const useUi = create<UiState>((set, get) => ({
  view: "library",
  filter: { kind: "all" },
  search: "",
  selected: null,
  draftSeed: null,
  runPrompt: null,
  importDir: null,
  importServer: null,
  selectedServer: null,
  removingServer: null,
  dirty: false,

  setView: (view) => {
    if (view !== get().view && !canLeave(get().dirty)) return;
    set({ view, dirty: view === "library" ? get().dirty : false });
  },
  setFilter: (filter) => set({ filter, view: "library" }),
  setSearch: (search) => set({ search }),
  select: (selected) => {
    if (selected === get().selected && get().view === "library") return;
    if (!canLeave(get().dirty)) return;
    set({ selected, view: "library", draftSeed: null, dirty: false });
  },
  newBookmark: (seed) => {
    if (!canLeave(get().dirty)) return;
    set({ selected: "new", view: "library", draftSeed: seed ?? null, dirty: false });
  },
  setRunPrompt: (runPrompt) => set({ runPrompt }),
  setImportDir: (importDir, server) => set({ importDir, importServer: server ?? null }),
  selectServer: (name) => {
    if (!canLeave(get().dirty)) return;
    set({ view: "server", selectedServer: name, dirty: false });
  },
  askRemoveServer: (removingServer) => set({ removingServer }),
  setDirty: (dirty) => set({ dirty }),
}));
