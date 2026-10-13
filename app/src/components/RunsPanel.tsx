import {
  ArrowUpRight,
  ChevronDown,
  ChevronUp,
  ClipboardCopy,
  Globe,
  Pause,
  RotateCcw,
  Square,
  SquareTerminal,
  TriangleAlert,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { useLibrary } from "../lib/queries";
import type { RunInfo } from "../lib/types";
import { cn, duration } from "../lib/utils";
import { useRuns } from "../stores/runs";
import { api, copyText, inTauri } from "../lib/api";
import { StatusDot } from "./bits";
import { Terminal, terminalText } from "./Terminal";
import { IconButton } from "./ui";

function Elapsed({ run }: { run: RunInfo }) {
  const [, tick] = useState(0);
  useEffect(() => {
    if (run.status !== "running") return;
    const t = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [run.status]);
  const ms = (run.ended_ms ?? Date.now()) - run.started_ms;
  return <span className="tabular-nums">{duration(Math.max(0, ms))}</span>;
}

export function RunsPanel() {
  const { tabs, runs, active, panelOpen, panelHeight, activate, close, setPanelOpen, setPanelHeight, start, connect } = useRuns();
  const { data: lib } = useLibrary();
  const dragging = useRef(false);
  const fontSize = lib?.settings.terminal_font_size ?? 12.5;

  if (!tabs.length) return null;
  const current = active ? runs[active] : undefined;

  const onDrag = (e: React.PointerEvent) => {
    dragging.current = true;
    const startY = e.clientY;
    const startH = panelHeight;
    const move = (ev: PointerEvent) => setPanelHeight(startH + (startY - ev.clientY));
    const up = () => {
      dragging.current = false;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    if (!panelOpen) setPanelOpen(true);
  };

  const rerun = (r: RunInfo) => void (r.kind === "session" && r.server ? connect(r.server) : start(r.entry_id, r.params));
  const openLink = (port: number) => {
    const url = `http://localhost:${port}`;
    if (inTauri) void import("@tauri-apps/plugin-opener").then((m) => m.openUrl(url));
    else window.open(url, "_blank");
  };

  return (
    <div
      className="relative flex shrink-0 flex-col border-t border-line-strong bg-panel"
      style={{ height: panelOpen ? panelHeight : 36 }}
    >
      <div onPointerDown={onDrag} className="absolute -top-1 right-0 left-0 z-10 h-2 cursor-row-resize" />
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line px-2">
        <SquareTerminal size={14} className="mx-1 shrink-0 text-faint" />
        <div className="scroll-thin flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto">
          {tabs.map((id) => {
            const r = runs[id];
            if (!r) return null;
            const isActive = id === active;
            return (
              <div
                key={id}
                onClick={() => activate(id)}
                onAuxClick={(e) => e.button === 1 && void close(id)}
                className={cn(
                  "group flex h-7 max-w-[220px] shrink-0 cursor-default items-center gap-1.5 rounded-md pr-1 pl-2.5 text-[12.5px] transition-colors",
                  isActive && panelOpen ? "bg-raised text-fg shadow-sm ring-1 ring-line" : "text-muted hover:bg-hover hover:text-fg",
                )}
              >
                <StatusDot status={r.status} />
                {r.kind === "session" && <SquareTerminal size={11} className="shrink-0 text-faint" />}
                <span className="truncate font-medium">{r.name}</span>
                {r.server && r.kind === "bookmark" && (
                  <span className="flex shrink-0 items-center gap-0.5 text-[11px] text-faint">
                    <Globe size={10} />
                    {r.server}
                  </span>
                )}
                {r.status === "running" && r.total_steps > 1 && (
                  <span className="text-[11px] text-faint tabular-nums">
                    {r.step}/{r.total_steps}
                  </span>
                )}
                <button
                  aria-label={r.status === "running" ? `Stop ${r.name}` : `Close ${r.name}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    void close(id);
                  }}
                  className="flex size-5 items-center justify-center rounded text-faint opacity-0 group-hover:opacity-100 hover:bg-hover hover:text-fg"
                >
                  {r.status === "running" ? <Square size={10} fill="currentColor" /> : <X size={12} />}
                </button>
              </div>
            );
          })}
        </div>

        {current && (
          <div className="flex shrink-0 items-center gap-0.5 pl-2 text-xs text-muted">
            {current.forwards.map((f) =>
              f.error ? (
                <span key={f.spec} title={f.error} className="mr-1 flex items-center gap-1 text-warn">
                  <TriangleAlert size={12} /> :{f.local_port}
                </span>
              ) : (
                <button
                  key={f.spec}
                  onClick={() => openLink(f.local_port)}
                  disabled={current.status !== "running"}
                  title={`localhost:${f.local_port} → ${f.remote} on ${current.server}`}
                  className="mr-1 flex items-center gap-0.5 rounded px-1.5 py-0.5 font-mono text-[11px] text-accent hover:bg-accent-soft disabled:text-faint"
                >
                  localhost:{f.local_port}
                  <ArrowUpRight size={11} />
                </button>
              ),
            )}
            <span className="mr-2 hidden max-w-[260px] truncate md:inline" title={current.cwd}>
              <span className="font-mono text-[11px]">{current.env_label}</span>
              <span className="mx-1.5 text-faint">·</span>
              <Elapsed run={current} />
              {current.status === "detached" ? (
                <span className="ml-1.5 text-[#2f8fbf]">still running on {current.server}</span>
              ) : current.status === "disconnected" ? (
                <span className="ml-1.5 text-warn">connection lost</span>
              ) : (
                current.status !== "running" &&
                current.kind === "bookmark" &&
                current.exit_code !== null &&
                current.exit_code !== 0 && <span className="ml-1.5 text-err">exit {current.exit_code}</span>
              )}
            </span>
            {current.tmux && current.status === "running" && (
              <IconButton label="Detach: keep it running on the server" onClick={() => void api.detachTmux(current.id)}>
                <Pause size={14} />
              </IconButton>
            )}
            {current.status === "running" ? (
              <IconButton
                label={current.kind === "session" ? "Disconnect" : "Stop"}
                shortcut={current.kind === "session" ? undefined : "⌃C in terminal"}
                onClick={() => void api.stopRun(current.id)}
              >
                <Square size={13} fill="currentColor" />
              </IconButton>
            ) : (
              <IconButton
                label={current.kind === "session" ? "Reconnect" : current.status === "detached" ? "Attach again" : "Run again"}
                onClick={() => rerun(current)}
              >
                <RotateCcw size={14} />
              </IconButton>
            )}
            <IconButton
              label="Copy output"
              onClick={async () => {
                await copyText(terminalText(current.id));
                toast.success("Output copied");
              }}
            >
              <ClipboardCopy size={14} />
            </IconButton>
          </div>
        )}
        <IconButton label={panelOpen ? "Hide panel" : "Show panel"} shortcut="⌘J" onClick={() => setPanelOpen(!panelOpen)}>
          {panelOpen ? <ChevronDown size={15} /> : <ChevronUp size={15} />}
        </IconButton>
      </div>

      <div className={cn("relative min-h-0 flex-1", !panelOpen && "hidden")}>
        {tabs.map((id) => (
          <div key={id} className={cn("absolute inset-0", id !== active && "pointer-events-none invisible")}>
            <Terminal runId={id} visible={panelOpen && id === active} fontSize={fontSize} />
          </div>
        ))}
      </div>
    </div>
  );
}
