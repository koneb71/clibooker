import { ArrowUpCircle, Bookmark, Clock, FolderGit2, Globe, Hash, History, Inbox, Layers, Plus, Server, Settings2, SquareTerminal, Stethoscope, Trash2 } from "lucide-react";
import { ContextMenu } from "radix-ui";
import type { ReactNode } from "react";
import { allTags, useLibrary, useServers, useServerStatus, useUpdates } from "../lib/queries";
import type { ServerInfo } from "../lib/types";
import { ReachDot } from "../views/ServerView";
import { isMac } from "../lib/platform";
import { cn } from "../lib/utils";
import { useRuns } from "../stores/runs";
import { type Filter, showUpdates, type View, useUi } from "../stores/ui";

function sameFilter(a: Filter, b: Filter) {
  if (a.kind !== b.kind) return false;
  if (a.kind === "tag" && b.kind === "tag") return a.tag === b.tag;
  if (a.kind === "project" && b.kind === "project") return a.file === b.file;
  return true;
}

function Item({
  icon,
  label,
  count,
  active,
  onClick,
}: {
  icon: ReactNode;
  label: ReactNode;
  count?: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] transition-colors duration-75",
        active ? "bg-active text-fg" : "text-fg/85 hover:bg-hover",
      )}
    >
      <span className={cn("flex size-4 items-center justify-center", active ? "text-accent" : "text-muted")}>{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {count !== undefined && count > 0 && <span className="text-xs tabular-nums text-faint">{count}</span>}
    </button>
  );
}

function Heading({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="mt-4 mb-1 flex h-5 items-center justify-between px-2">
      <span className="text-[11px] font-semibold text-faint">{children}</span>
      {action}
    </div>
  );
}

const menuItem =
  "flex h-7 cursor-default items-center gap-2 rounded-md px-2 outline-none select-none data-[highlighted]:bg-accent data-[highlighted]:text-accent-fg";

function ServerItem({ s, active, onClick }: { s: ServerInfo; active: boolean; onClick: () => void }) {
  // ~/.ssh/config hosts are only checked once you open them.
  const status = useServerStatus(s.name, s.source === "clibooker");
  const connect = useRuns((r) => r.connect);
  const askRemoveServer = useUi((u) => u.askRemoveServer);
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger asChild>
        <div>
          <Item
            icon={
              <span className="relative">
                <Server size={14} />
                <ReachDot reach={status.data ?? s.status} className="absolute -right-0.5 -bottom-0.5 size-[6px] ring-1 ring-sidebar" />
              </span>
            }
            label={<span title={s.destination}>{s.name}</span>}
            count={s.bookmarks}
            active={active}
            onClick={onClick}
          />
        </div>
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content className="z-50 min-w-[190px] rounded-lg border border-line-strong bg-raised p-1 text-[13px] text-fg shadow-xl">
          <ContextMenu.Item className={menuItem} onSelect={() => void connect(s.name)}>
            <SquareTerminal size={13} /> Connect
          </ContextMenu.Item>
          <ContextMenu.Item className={menuItem} onSelect={onClick}>
            <Server size={13} /> {s.source === "clibooker" ? "Edit…" : "Details…"}
          </ContextMenu.Item>
          <ContextMenu.Separator className="my-1 h-px bg-line" />
          <ContextMenu.Item className={cn(menuItem, "text-err data-[highlighted]:bg-err")} onSelect={() => askRemoveServer(s.name)}>
            <Trash2 size={13} /> {s.source === "clibooker" ? "Delete server…" : "Remove from list…"}
          </ContextMenu.Item>
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
}

export function Sidebar({ onAddProject }: { onAddProject: () => void }) {
  const { data: lib } = useLibrary();
  const { data: servers } = useServers();
  const { filter, view, setFilter, setView, selectedServer, selectServer } = useUi();
  const { data: update } = useUpdates();
  const entries = (lib?.entries ?? []).filter((e) => !e.shadowed);
  const tags = allTags(lib);
  const inLibrary = view === "library";

  const nav = (f: Filter) => ({ active: inLibrary && sameFilter(filter, f), onClick: () => setFilter(f) });
  const go = (v: View) => ({ active: view === v, onClick: () => setView(v) });

  return (
    <nav className="flex h-full w-[212px] shrink-0 flex-col bg-sidebar">
      {/* Space for macOS's traffic lights; also a drag handle for the window. */}
      <div data-tauri-drag-region className={isMac ? "h-[52px] shrink-0" : "h-2.5 shrink-0"} />
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2.5 pb-3">
        <Item icon={<Layers size={15} />} label="All bookmarks" count={entries.length} {...nav({ kind: "all" })} />
        <Item
          icon={<Bookmark size={15} />}
          label="Pinned"
          count={entries.filter((e) => e.bookmark.pinned).length}
          {...nav({ kind: "pinned" })}
        />
        <Item icon={<Clock size={15} />} label="Recent" {...nav({ kind: "recent" })} />
        <Item
          icon={<Globe size={15} />}
          label="Global"
          count={entries.filter((e) => e.scope === "global").length}
          {...nav({ kind: "global" })}
        />

        <Heading
          action={
            <button
              onClick={onAddProject}
              aria-label="Add project folder"
              className="flex size-5 items-center justify-center rounded text-faint hover:bg-hover hover:text-fg"
            >
              <Plus size={13} />
            </button>
          }
        >
          Projects
        </Heading>
        {lib?.projects.length ? (
          lib.projects.map((p) => (
            <Item
              key={p.file}
              icon={<FolderGit2 size={15} />}
              label={<span title={p.dir}>{p.name}</span>}
              count={entries.filter((e) => e.file === p.file).length}
              {...nav({ kind: "project", file: p.file })}
            />
          ))
        ) : (
          <p className="px-2 text-xs leading-relaxed text-faint">Add a repo to keep its bookmarks in a shareable .bk.toml.</p>
        )}

        {tags.length > 0 && (
          <>
            <Heading>Tags</Heading>
            {tags.map((t) => (
              <Item
                key={t}
                icon={<Hash size={14} />}
                label={t}
                count={entries.filter((e) => e.bookmark.tags?.includes(t)).length}
                {...nav({ kind: "tag", tag: t })}
              />
            ))}
          </>
        )}

        <Heading
          action={
            <button
              onClick={() => selectServer("new")}
              aria-label="Add server"
              className="flex size-5 items-center justify-center rounded text-faint hover:bg-hover hover:text-fg"
            >
              <Plus size={13} />
            </button>
          }
        >
          Servers
        </Heading>
        {servers?.length ? (
          servers.map((s) => (
            <ServerItem key={s.name} s={s} active={view === "server" && selectedServer === s.name} onClick={() => selectServer(s.name)} />
          ))
        ) : (
          <p className="px-2 text-xs leading-relaxed text-faint">Add an SSH server to connect to it and run bookmarks there.</p>
        )}

        <Heading>Activity</Heading>
        <Item icon={<History size={15} />} label="History" {...go("history")} />
        <Item icon={<Inbox size={15} />} label="Inbox" {...go("inbox")} />
        <Item icon={<Stethoscope size={15} />} label="Doctor" {...go("doctor")} />
      </div>
      <div className="shrink-0 px-2.5 pb-3">
        {update?.available && (
          <button
            onClick={showUpdates}
            className="mb-1 flex h-7 w-full items-center gap-2 rounded-md bg-accent-soft px-2 text-left text-[12.5px] font-medium text-accent transition-[filter] hover:brightness-95"
          >
            <ArrowUpCircle size={15} className="shrink-0" />
            <span className="truncate">Update to {update.available.version}</span>
          </button>
        )}
        <Item icon={<Settings2 size={15} />} label="Settings" {...go("settings")} />
      </div>
    </nav>
  );
}
