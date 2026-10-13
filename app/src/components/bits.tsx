import type { EnvSpec, RunStatus } from "../lib/types";
import { cn, envShort } from "../lib/utils";

const envColors: Record<string, string> = {
  conda: "text-[#2f8f5b] dark:text-[#5fd193] bg-[#2f8f5b]/10",
  venv: "text-[#3d6fd1] dark:text-[#82a8ff] bg-[#3d6fd1]/10",
  uv: "text-[#a2449f] dark:text-[#e18ade] bg-[#a2449f]/10",
  poetry: "text-[#3a7ea8] dark:text-[#7cc4ef] bg-[#3a7ea8]/10",
  pyenv: "text-[#b0632b] dark:text-[#f0a46b] bg-[#b0632b]/10",
  auto: "text-muted bg-hover",
  clean: "text-muted bg-hover",
};

export function EnvBadge({ env, className }: { env?: EnvSpec | null; className?: string }) {
  const s = envShort(env);
  if (!s) return null;
  return (
    <span
      className={cn(
        "inline-flex h-[18px] max-w-[160px] shrink-0 items-center gap-1 rounded px-1.5 text-[11px] font-medium",
        envColors[s.kind] ?? envColors.auto,
        className,
      )}
    >
      <span className="opacity-75">{s.kind}</span>
      {s.detail && <span className="truncate font-mono text-[10.5px]">{s.detail}</span>}
    </span>
  );
}

export function StatusDot({ status, className }: { status: RunStatus | "none"; className?: string }) {
  const color = {
    running: "bg-accent animate-pulse-dot",
    succeeded: "bg-ok",
    failed: "bg-err",
    stopped: "bg-warn",
    disconnected: "bg-warn ring-2 ring-warn/30",
    detached: "bg-[#2f8fbf] animate-pulse-dot",
    none: "bg-transparent",
  }[status];
  return <span aria-label={status} className={cn("inline-block size-[7px] shrink-0 rounded-full", color, className)} />;
}

export function statusOfExit(code: number | null | undefined): RunStatus | "none" {
  if (code === undefined) return "none";
  if (code === null) return "stopped";
  if (code === 0) return "succeeded";
  if (code === 130) return "stopped";
  return "failed";
}

/** Highlight `{{param}}` placeholders inside a command. */
export function CommandText({ text, className }: { text: string; className?: string }) {
  const parts = text.split(/(\{\{\s*[A-Za-z_]\w*\s*(?::[^}]*?)?\s*\}\})/g);
  return (
    <span className={cn("font-mono", className)}>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <span key={i} className="rounded-sm bg-accent-soft px-0.5 text-accent">
            {p}
          </span>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </span>
  );
}
