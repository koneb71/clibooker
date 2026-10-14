import { Globe, MapPinOff, PackageOpen, Pin, Play, Plus, Search } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import { matchesSearch, sortEntries, useLibrary } from "../lib/queries";
import { computerNoun, isMac, isMod, kbd } from "../lib/platform";
import { requestRun } from "../lib/run";
import type { Entry } from "../lib/types";
import { cn, relativeTime } from "../lib/utils";
import { useRuns } from "../stores/runs";
import { type Filter, useUi } from "../stores/ui";
import { pickFolder } from "../lib/api";
import { EnvBadge, StatusDot, statusOfExit } from "./bits";
import { RuntimeBadges } from "./RuntimesEditor";
import { IconButton, Kbd } from "./ui";

function applyFilter(entries: Entry[], f: Filter): Entry[] {
  switch (f.kind) {
    case "all":
      return entries;
    case "pinned":
      return entries.filter((e) => e.bookmark.pinned);
    case "recent":
      return entries
        .filter((e) => e.last_run)
        .sort((a, b) => (b.last_run?.started_ms ?? 0) - (a.last_run?.started_ms ?? 0));
    case "global":
      return entries.filter((e) => e.scope === "global");
    case "tag":
      return entries.filter((e) => e.bookmark.tags?.includes(f.tag));
    case "project":
      return entries.filter((e) => e.file === f.file);
  }
}

function filterTitle(f: Filter, projectName?: string) {
  switch (f.kind) {
    case "all":
      return "All bookmarks";
    case "pinned":
      return "Pinned";
    case "recent":
      return "Recent";
    case "global":
      return "Global";
    case "tag":
      return `#${f.tag}`;
    case "project":
      return projectName ?? "Project";
  }
}

export function useVisibleEntries() {
  const { data: lib } = useLibrary();
  const { filter, search } = useUi();
  return useMemo(() => {
    const base = (lib?.entries ?? []).filter((e) => !e.shadowed);
    const filtered = applyFilter(base, filter).filter((e) => matchesSearch(e, search));
    return filter.kind === "recent" ? filtered : sortEntries(filtered);
  }, [lib, filter, search]);
}

export function BookmarkList() {
  const { data: lib, isLoading } = useLibrary();
  const { filter, search, setSearch, selected, select, newBookmark, setImportDir } = useUi();
  const runs = useRuns((s) => s.runs);
  const entries = useVisibleEntries();
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const runningIds = useMemo(
    () => new Set(Object.values(runs).filter((r) => r.status === "running").map((r) => r.entry_id)),
    [runs],
  );
  const projectName = filter.kind === "project" ? lib?.projects.find((p) => p.file === filter.file)?.name : undefined;

  // ⌘F / ⌘K (Ctrl+F / Ctrl+K) focus search
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isMod(e) && (e.key === "f" || e.key === "k") && (isMac || !(e.target as Element | null)?.closest?.(".xterm"))) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Arrow keys move the selection; Enter runs.
  const onListKey = (e: React.KeyboardEvent) => {
    if (!entries.length) return;
    const idx = entries.findIndex((x) => x.id === selected);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next = e.key === "ArrowDown" ? Math.min(idx + 1, entries.length - 1) : Math.max(idx - 1, 0);
      select(entries[next].id);
      listRef.current?.querySelector<HTMLElement>(`[data-id="${CSS.escape(entries[next].id)}"]`)?.scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter" && idx >= 0) {
      e.preventDefault();
      requestRun(entries[idx]);
    }
  };

  return (
    <section className="flex h-full w-[340px] shrink-0 flex-col border-r border-line bg-panel">
      <header data-tauri-drag-region className="flex h-[52px] shrink-0 items-center gap-2 px-3">
        <h1 data-tauri-drag-region className="min-w-0 flex-1 truncate text-[15px] font-semibold">
          {filterTitle(filter, projectName)}
        </h1>
        <IconButton
          label="Bookmark a project's scripts (npm, make, just…)"
          onClick={async () => {
            const projectDir = filter.kind === "project" ? lib?.projects.find((p) => p.file === filter.file)?.dir : undefined;
            const dir = projectDir ?? (await pickFolder());
            if (dir) setImportDir(dir);
          }}
        >
          <PackageOpen size={16} />
        </IconButton>
        <IconButton label="New bookmark" shortcut={kbd("Mod+N")} onClick={() => newBookmark()}>
          <Plus size={16} />
        </IconButton>
      </header>
      <div className="shrink-0 px-3 pb-2">
        <div className="relative">
          <Search size={13} className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-faint" />
          <input
            ref={searchRef}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setSearch("");
              if (e.key === "ArrowDown" || e.key === "Enter") onListKey(e);
            }}
            placeholder="Search name, command, env, folder…"
            spellCheck={false}
            className="h-7 w-full rounded-md border border-line bg-raised pr-12 pl-7 text-[13px] outline-none placeholder:text-faint focus:border-accent focus:ring-3 focus:ring-accent-soft"
          />
          <Kbd className="absolute top-1/2 right-1.5 -translate-y-1/2">{kbd("Mod+K")}</Kbd>
        </div>
      </div>

      <div
        ref={listRef}
        tabIndex={0}
        onKeyDown={onListKey}
        className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2 pb-2 outline-none"
      >
        {isLoading && <p className="px-3 py-6 text-[13px] text-muted">Loading…</p>}
        {!isLoading && entries.length === 0 && (
          <div className="flex flex-col items-center gap-2 px-6 py-12 text-center">
            <p className="text-[13px] text-muted">{search ? `Nothing matches “${search}”.` : "No bookmarks here yet."}</p>
            {!search && (
              <button onClick={() => newBookmark()} className="text-[13px] font-medium text-accent hover:underline">
                Create one
              </button>
            )}
          </div>
        )}
        {entries.map((e) => {
          const active = e.id === selected;
          const running = runningIds.has(e.id);
          const status = running ? "running" : statusOfExit(e.last_run ? e.last_run.exit_code : undefined);
          return (
            <div
              key={e.id}
              data-id={e.id}
              onClick={() => select(e.id)}
              onDoubleClick={() => requestRun(e)}
              className={cn(
                "group relative mb-px flex cursor-default flex-col gap-1 rounded-lg px-3 py-2 transition-colors duration-75",
                active ? "bg-accent-soft" : "hover:bg-hover",
              )}
            >
              <div className="flex items-center gap-2">
                <StatusDot status={status} />
                <span className="min-w-0 truncate text-[13px] font-semibold">{e.name}</span>
                {e.bookmark.pinned && <Pin size={11} className="shrink-0 text-faint" />}
                <span className="ml-auto flex shrink-0 items-center gap-1">
                  <EnvBadge env={e.bookmark.env} />
                  <RuntimeBadges runtimes={e.bookmark.runtimes} max={e.bookmark.env && e.bookmark.env.type !== "inherit" ? 1 : 2} />
                </span>
              </div>
              <div className="flex items-center gap-2 pl-[15px] text-xs text-muted">
                <span className="min-w-0 flex-1 truncate font-mono text-[11.5px]">{e.summary}</span>
              </div>
              <div className="flex items-center gap-2 pl-[15px] text-[11px] text-faint">
                {e.bookmark.server ? (
                  <span className="flex min-w-0 items-center gap-1 truncate">
                    <Globe size={10} className="shrink-0" />
                    {e.bookmark.server}:{e.bookmark.cwd ?? "~"}
                  </span>
                ) : e.folder?.state === "missing" ? (
                  <span className="flex min-w-0 items-center gap-1 truncate text-warn" title={`${e.folder.path} isn't on this ${computerNoun}`}>
                    <MapPinOff size={10} className="shrink-0" />
                    Not on this {computerNoun}
                  </span>
                ) : (
                  <span
                    className="min-w-0 truncate"
                    title={e.folder?.via ? `${e.bookmark.cwd} — ${e.folder.via.shared} is ${e.folder.via.local} on this ${computerNoun}` : undefined}
                  >
                    {e.cwd ?? "any folder"}
                  </span>
                )}
                {e.last_run && <span className="ml-auto shrink-0">{relativeTime(e.last_run.started_ms)}</span>}
              </div>
              <button
                aria-label={`Run ${e.name}`}
                onClick={(ev) => {
                  ev.stopPropagation();
                  requestRun(e);
                }}
                className={cn(
                  "absolute top-1/2 right-2 flex size-7 -translate-y-1/2 items-center justify-center rounded-full bg-ok text-white shadow-md transition-[opacity,transform] duration-100",
                  "opacity-0 group-hover:opacity-100 focus-visible:opacity-100 active:scale-95",
                )}
              >
                <Play size={13} fill="currentColor" />
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}
