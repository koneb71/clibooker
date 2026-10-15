import { useQueryClient } from "@tanstack/react-query";
import { BookmarkPlus, Command, MousePointerClick, Terminal } from "lucide-react";
import { useEffect } from "react";
import { Toaster, toast } from "sonner";
import { BookmarkEditor } from "./components/BookmarkEditor";
import { BookmarkList } from "./components/BookmarkList";
import { ImportScriptsDialog } from "./components/ImportScriptsDialog";
import { RunDialog } from "./components/RunDialog";
import { RunsPanel } from "./components/RunsPanel";
import { Sidebar } from "./components/Sidebar";
import { Button, Kbd, TooltipProvider } from "./components/ui";
import { api, on, pickFolder } from "./lib/api";
import { isMac, isMod, kbd } from "./lib/platform";
import { keys, useBackendInvalidation, useEntry, useLibrary } from "./lib/queries";
import { requestRun } from "./lib/run";
import type { RunInfo, SyncEvent } from "./lib/types";
import { useRuns } from "./stores/runs";
import { useTheme } from "./stores/theme";
import { useUi } from "./stores/ui";
import { DoctorView, HistoryView, InboxView } from "./views/ActivityViews";
import { RemoveServerDialog, ServerView } from "./views/ServerView";
import { SettingsView } from "./views/SettingsView";

function EmptyDetail() {
  const newBookmark = useUi((s) => s.newBookmark);
  const { data: lib } = useLibrary();
  const hotkey = lib?.settings.hotkey ?? lib?.default_hotkey ?? (isMac ? "Alt+Space" : "Ctrl+Alt+Space");
  const tips = [
    { icon: <BookmarkPlus size={15} />, text: "Create a bookmark", keys: kbd("Mod+N") },
    { icon: <MousePointerClick size={15} />, text: "Double-click a bookmark to run it", keys: "↩" },
    { icon: <Command size={15} />, text: "Quick run from any app", keys: kbd(hotkey) },
    { icon: <Terminal size={15} />, text: "In a terminal: bk save NAME saves the last command", keys: "" },
  ];
  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
      {isMac && <div data-tauri-drag-region className="h-[52px] shrink-0" />}
      <div className="flex flex-1 flex-col items-center justify-center gap-6 px-8 pb-16">
        <div className="flex flex-col gap-2.5">
          {tips.map((t) => (
            <div key={t.text} className="flex items-center gap-3 text-[13px] text-muted">
              <span className="text-faint">{t.icon}</span>
              <span className="w-[300px]">{t.text}</span>
              {t.keys && <Kbd>{t.keys}</Kbd>}
            </div>
          ))}
        </div>
        <Button variant="primary" onClick={() => newBookmark()}>
          New bookmark
        </Button>
      </div>
    </div>
  );
}

export function MainApp() {
  useBackendInvalidation();
  const qc = useQueryClient();
  const { data: lib } = useLibrary();
  const { view, selected, newBookmark, setView, setFilter } = useUi();
  const entry = useEntry(selected && selected !== "new" ? selected : null);
  const resolvedTheme = useTheme((s) => s.resolved);

  useEffect(() => {
    if (lib) useTheme.getState().setPref(lib.settings.theme ?? "system");
  }, [lib?.settings.theme, lib]);

  // Tell the user once if the quick-run shortcut is taken by another app.
  useEffect(() => {
    void api.hotkeyError().then((err) => {
      if (err)
        toast.warning("Quick-run shortcut unavailable", {
          description: `${err}. Pick another one in Settings.`,
          action: { label: "Settings", onClick: () => useUi.getState().setView("settings") },
          duration: 10_000,
        });
    });
  }, []);

  // Run lifecycle events from the backend.
  useEffect(() => {
    void api.listRuns().then((list) => list.forEach((r) => useRuns.getState().upsert(r)));
    const offs = [
      on<RunInfo>("run-updated", (info) => {
        const { runs, upsert } = useRuns.getState();
        const isNew = !runs[info.id];
        upsert(info, { focus: false });
        if (info.status !== "running" && !isNew) {
          qc.invalidateQueries({ queryKey: keys.library });
          qc.invalidateQueries({ queryKey: ["history"] });
          // A key was made, unlocked or installed (and a server may use it now).
          if (info.kind === "task") {
            qc.invalidateQueries({ queryKey: ["keys"] });
            qc.invalidateQueries({ queryKey: ["servers"] });
            qc.invalidateQueries({ queryKey: ["server-status"] });
          }
        }
      }),
      on<string>("focus-run", (id) => useRuns.getState().activate(id)),
      on<{ id: string; params: Record<string, string> | null }>("request-run", async ({ id, params }) => {
        const fresh = await qc.fetchQuery({ queryKey: keys.library, queryFn: api.getLibrary });
        const e = fresh.entries.find((x) => x.id === id);
        if (!e) return toast.error("That bookmark no longer exists");
        requestRun(e, params ? { params } : undefined);
      }),
      on<string>("request-connect", (name) => void useRuns.getState().connect(name)),
      on<string | null>("shell-env-ready", (err) => {
        if (err) toast.warning("Couldn't read your login shell environment", { description: err });
      }),
      on<SyncEvent>("sync-finished", (ev) => {
        // Automatic syncs only speak up when something arrived; Settings shows why one failed.
        const details = [...ev.skipped.map((x) => `Skipped ${x}`), ev.warning].filter(Boolean).join("\n") || undefined;
        if (ev.error) {
          if (!ev.auto) toast.error("Sync failed", { description: ev.error });
        } else if (!ev.auto || ev.received > 0) {
          toast.success(`Synced: ${ev.summary}`, { description: details });
        }
        qc.invalidateQueries({ queryKey: ["sync-status"] });
        qc.invalidateQueries({ queryKey: ["servers"] });
        qc.invalidateQueries({ queryKey: ["keys"] });
      }),
    ];
    return () => offs.forEach((off) => off());
  }, [qc]);

  // Global shortcuts.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!isMod(e)) return;
      // Off macOS the shortcuts are Ctrl+…, which a focused terminal needs for itself.
      if (!isMac && (e.target as Element | null)?.closest?.(".xterm")) return;
      if (e.key === "n") {
        e.preventDefault();
        newBookmark();
      } else if (e.key === "j") {
        e.preventDefault();
        const s = useRuns.getState();
        if (s.tabs.length) s.setPanelOpen(!s.panelOpen);
      } else if (e.key === ",") {
        e.preventDefault();
        setView("settings");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [newBookmark, setView]);

  const addProject = async () => {
    const dir = await pickFolder();
    if (!dir) return;
    try {
      const file = await api.addProject(dir);
      await qc.invalidateQueries({ queryKey: keys.library });
      setFilter({ kind: "project", file });
      toast.success("Project added", { description: `Bookmarks saved to it go in ${file}` });
    } catch (e) {
      toast.error("Could not add project", { description: String(e) });
    }
  };

  let content;
  if (view === "history") content = <HistoryView />;
  else if (view === "inbox") content = <InboxView />;
  else if (view === "doctor") content = <DoctorView />;
  else if (view === "settings") content = <SettingsView />;
  else if (view === "server") content = <ServerView key={useUi.getState().selectedServer ?? ""} />;
  else
    content = (
      <>
        <BookmarkList />
        {selected === "new" || entry ? <BookmarkEditor key={selected} entry={entry ?? null} /> : <EmptyDetail />}
      </>
    );

  return (
    <TooltipProvider>
      <div className="flex h-full">
        <Sidebar onAddProject={addProject} />
        <div className="flex min-w-0 flex-1 flex-col border-l border-line bg-bg">
          <div className="flex min-h-0 flex-1">{content}</div>
          <RunsPanel />
        </div>
      </div>
      <RunDialog />
      <ImportScriptsDialog />
      <RemoveServerDialog />
      {lib?.errors.length ? <LoadErrors errors={lib.errors} /> : null}
      <Toaster position="bottom-right" theme={resolvedTheme} richColors closeButton offset={16} />
    </TooltipProvider>
  );
}

function LoadErrors({ errors }: { errors: { file: string; message: string }[] }) {
  return (
    <div className="fixed top-3 left-1/2 z-30 max-w-[560px] -translate-x-1/2 rounded-lg border border-err/30 bg-raised px-4 py-2.5 text-xs shadow-lg">
      <p className="mb-1 font-semibold text-err">A bookmarks file has errors (its bookmarks are hidden until fixed)</p>
      {errors.map((e) => (
        <p key={e.file} className="selectable font-mono text-[11px] whitespace-pre-wrap text-muted">
          {e.message}
        </p>
      ))}
    </div>
  );
}
