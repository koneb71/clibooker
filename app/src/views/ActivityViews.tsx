import { useQuery, useQueryClient } from "@tanstack/react-query";
import { BookmarkPlus, CheckCircle2, Copy, RefreshCw, RotateCcw, Trash2, TriangleAlert } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { api, copyText, on } from "../lib/api";
import { useHistory, useLibrary } from "../lib/queries";
import { requestRun } from "../lib/run";
import { cn, duration, relativeTime } from "../lib/utils";
import { useUi } from "../stores/ui";
import { EnvBadge, StatusDot, statusOfExit } from "../components/bits";
import { Button, IconButton, Kbd } from "../components/ui";

function ViewShell({ title, subtitle, actions, children }: { title: string; subtitle?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
      <header data-tauri-drag-region className="flex h-[52px] shrink-0 items-center gap-3 border-b border-line px-5">
        <div data-tauri-drag-region className="flex min-w-0 flex-1 items-baseline gap-3">
          <h1 className="text-[15px] font-semibold">{title}</h1>
          {subtitle && <span className="truncate text-xs text-muted">{subtitle}</span>}
        </div>
        {actions}
      </header>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">{children}</div>
    </div>
  );
}

function dayLabel(ms: number) {
  const d = new Date(ms);
  const today = new Date();
  const yesterday = new Date(Date.now() - 86400_000);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric" });
}

export function HistoryView() {
  const { data: records = [], isLoading } = useHistory(undefined, 500);
  const { data: lib } = useLibrary();
  const [filter, setFilter] = useState<"all" | "failed">("all");
  const rows = records.filter((r) => filter === "all" || (r.exit_code !== 0 && r.exit_code !== null));

  const groups = useMemo(() => {
    const out: { day: string; items: typeof rows }[] = [];
    for (const r of rows) {
      const day = dayLabel(r.started_ms);
      const last = out[out.length - 1];
      if (last?.day === day) last.items.push(r);
      else out.push({ day, items: [r] });
    }
    return out;
  }, [rows]);

  return (
    <ViewShell
      title="History"
      subtitle={`${records.length} runs from the app and the bk CLI`}
      actions={
        <div className="flex rounded-md bg-hover p-0.5 text-xs">
          {(["all", "failed"] as const).map((f) => (
            <button
              key={f}
              onClick={() => setFilter(f)}
              className={cn("rounded px-2 py-0.5 font-medium capitalize", filter === f ? "bg-raised text-fg shadow-sm" : "text-muted hover:text-fg")}
            >
              {f}
            </button>
          ))}
        </div>
      }
    >
      <div className="mx-auto max-w-[860px] px-5 py-4">
        {isLoading && <p className="text-muted">Loading…</p>}
        {!isLoading && rows.length === 0 && (
          <p className="py-16 text-center text-[13px] text-muted">
            {filter === "failed" ? "No failed runs. Nice." : "Runs show up here once you run a bookmark."}
          </p>
        )}
        {groups.map((g) => (
          <section key={g.day} className="mb-5">
            <h2 className="mb-1.5 px-2 text-[11px] font-semibold text-faint">{g.day}</h2>
            <div className="overflow-hidden rounded-lg border border-line">
              {g.items.map((r) => {
                const entry = lib?.entries.find((e) => e.id === r.entry_id);
                const params = Object.entries(r.params);
                return (
                  <div key={r.id + r.started_ms} className="group flex items-center gap-3 border-b border-line px-3 py-2 last:border-b-0 hover:bg-hover">
                    <StatusDot status={statusOfExit(r.exit_code)} />
                    <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                      <div className="flex items-center gap-2">
                        <span className="text-[13px] font-semibold">{r.name}</span>
                        <span className="font-mono text-[11px] text-faint">{r.env}</span>
                        {r.server && <span className="rounded bg-hover px-1 text-[10.5px] text-muted">@{r.server}</span>}
                        {r.source === "cli" && <span className="rounded bg-hover px-1 text-[10.5px] text-muted">cli</span>}
                      </div>
                      <span className="selectable truncate font-mono text-[11.5px] text-muted">{r.commands.join(" && ")}</span>
                      {params.length > 0 && (
                        <span className="truncate font-mono text-[11px] text-faint">{params.map(([k, v]) => `${k}=${v}`).join("  ")}</span>
                      )}
                    </div>
                    <div className="flex shrink-0 flex-col items-end gap-0.5 text-xs text-muted tabular-nums">
                      <span>{new Date(r.started_ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</span>
                      <span className={cn(r.exit_code ? "text-err" : "text-faint")}>
                        {r.exit_code ? `exit ${r.exit_code} · ` : ""}
                        {duration(r.duration_ms)}
                      </span>
                    </div>
                    <IconButton
                      label="Run again with these values"
                      disabled={!entry}
                      className="opacity-0 group-hover:opacity-100"
                      onClick={() => entry && requestRun(entry, { params: r.params })}
                    >
                      <RotateCcw size={14} />
                    </IconButton>
                  </div>
                );
              })}
            </div>
          </section>
        ))}
      </div>
    </ViewShell>
  );
}

export function InboxView() {
  const qc = useQueryClient();
  const { newBookmark } = useUi();
  const { data: items = [], isLoading } = useQuery({ queryKey: ["inbox"], queryFn: () => api.getInbox(200) });
  useEffect(() => on("inbox-changed", () => qc.invalidateQueries({ queryKey: ["inbox"] })), [qc]);

  return (
    <ViewShell
      title="Inbox"
      subtitle="Commands you ran recently in your terminal"
      actions={
        items.length > 0 && (
          <Button
            variant="ghost"
            onClick={async () => {
              await api.clearInbox();
              qc.invalidateQueries({ queryKey: ["inbox"] });
            }}
          >
            <Trash2 size={13} /> Clear
          </Button>
        )
      }
    >
      <div className="mx-auto max-w-[860px] px-5 py-4">
        {!isLoading && items.length === 0 && (
          <div className="mx-auto max-w-[520px] py-12 text-[13px] leading-relaxed text-muted">
            <p className="mb-3 font-medium text-fg">Turn terminal commands into bookmarks in one click</p>
            <p className="mb-3">
              Enable recording in your shell and every command you run shows up here with its folder and active conda env / venv.
              Add this line to <code className="font-mono text-fg">~/.zshrc</code>:
            </p>
            <div className="mb-3 flex items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2">
              <code className="selectable flex-1 font-mono text-[12px] text-fg">eval "$(bk init zsh --record)"</code>
              <IconButton label="Copy" onClick={() => void copyText('eval "$(bk init zsh --record)"').then(() => toast.success("Copied"))}>
                <Copy size={13} />
              </IconButton>
            </div>
            <p className="text-faint">
              Commands starting with a space, and ones that look like they contain secrets, are never recorded. Without{" "}
              <code className="font-mono">--record</code> you still get <code className="font-mono">bk save NAME</code>, which saves just the last command.
            </p>
          </div>
        )}
        <div className="flex flex-col gap-1.5">
          {items.map((it) => (
            <div key={`${it.ts}-${it.command}`} className="group flex items-start gap-3 rounded-lg border border-line px-3 py-2.5 hover:bg-hover">
              <StatusDot status={it.status === 0 ? "succeeded" : "failed"} className="mt-1.5" />
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <code className="selectable font-mono text-[12px] break-all whitespace-pre-wrap text-fg">{it.command}</code>
                <div className="flex items-center gap-2 text-[11px] text-faint">
                  <span className="truncate font-mono">{it.cwd}</span>
                  <EnvBadge env={it.env} />
                  <span className="ml-auto shrink-0">{relativeTime(it.ts * 1000)}</span>
                </div>
              </div>
              <Button
                size="sm"
                className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                onClick={() => newBookmark({ command: it.command, cwd: it.cwd, env: it.env ?? undefined })}
              >
                <BookmarkPlus size={12} /> Bookmark
              </Button>
            </div>
          ))}
        </div>
      </div>
    </ViewShell>
  );
}

export function DoctorView() {
  const { select } = useUi();
  const { data, isFetching, refetch } = useQuery({ queryKey: ["doctor"], queryFn: api.runDoctor });
  const failing = data?.checks.filter((c) => c.problems.length) ?? [];
  const warned = data?.checks.filter((c) => !c.problems.length && c.warnings.length) ?? [];
  const healthy = (data?.checks.length ?? 0) - failing.length - warned.length;

  return (
    <ViewShell
      title="Doctor"
      subtitle={data ? `${data.checks.length} bookmarks checked` : undefined}
      actions={
        <Button variant="ghost" onClick={() => void refetch()} disabled={isFetching}>
          <RefreshCw size={13} className={cn(isFetching && "animate-spin")} /> Check again
        </Button>
      }
    >
      <div className="mx-auto flex max-w-[760px] flex-col gap-4 px-5 py-5">
        {data && failing.length === 0 && data.file_errors.length === 0 && (
          <div className="flex items-center gap-3 rounded-lg border border-ok/25 bg-ok-soft px-4 py-3 text-[13px]">
            <CheckCircle2 size={18} className="text-ok" />
            Every bookmark's folder, environment, .env file and shell resolve.
          </div>
        )}
        {data?.file_errors.map((e) => (
          <div key={e} className="selectable rounded-lg border border-err/25 bg-err-soft px-4 py-3 font-mono text-[12px] whitespace-pre-wrap text-err">
            {e}
          </div>
        ))}
        {[...failing, ...warned].map((c) => (
          <button
            key={c.entry_id}
            onClick={() => select(c.entry_id)}
            className="flex flex-col gap-1.5 rounded-lg border border-line px-4 py-3 text-left hover:bg-hover"
          >
            <div className="flex items-center gap-2">
              <TriangleAlert size={14} className={c.problems.length ? "text-err" : "text-warn"} />
              <span className="text-[13px] font-semibold">{c.name}</span>
              <span className="font-mono text-[11px] text-faint">{c.env}</span>
              <span className="ml-auto text-[11px] text-faint">Open <Kbd>↩</Kbd></span>
            </div>
            {[...c.problems.map((p) => [p, "text-err"]), ...c.warnings.map((w) => [w, "text-warn"])].map(([msg, cls]) => (
              <p key={msg} className={cn("pl-6 text-xs leading-relaxed", cls)}>
                {msg}
              </p>
            ))}
          </button>
        ))}
        {data && healthy > 0 && <p className="text-center text-xs text-faint">{healthy} healthy bookmarks not shown</p>}
      </div>
    </ViewShell>
  );
}
