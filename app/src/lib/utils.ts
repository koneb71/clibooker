import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import type { Bookmark, EnvSpec, Step, StepTable } from "./types";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function envLabel(spec?: EnvSpec | null): string {
  if (!spec) return "inherit";
  switch (spec.type) {
    case "conda":
      return `conda:${spec.name ?? spec.prefix ?? "?"}`;
    case "venv":
      return `venv:${spec.path}`;
    case "uv":
      return spec.sync ? "uv (sync)" : "uv";
    case "pyenv":
      return `pyenv:${spec.version}`;
    default:
      return spec.type;
  }
}

/** Env plus runtimes, e.g. `conda:sam2 · node 22`. */
export function envSummary(b: Bookmark): string {
  const parts: string[] = [];
  const rts = Object.entries(b.runtimes ?? {});
  if ((b.env && b.env.type !== "inherit") || !rts.length) parts.push(envLabel(b.env));
  for (const [l, v] of rts) parts.push(`${l} ${v}`);
  return parts.join(" · ");
}

/** Short badge text, e.g. "sam2" for a conda env, ".venv" for a venv. */
export function envShort(spec?: EnvSpec | null): { kind: string; detail?: string } | null {
  if (!spec || spec.type === "inherit") return null;
  switch (spec.type) {
    case "conda":
      return { kind: "conda", detail: spec.name ?? spec.prefix?.split(/[\\/]/).pop() };
    case "venv":
      return { kind: "venv", detail: spec.path };
    case "pyenv":
      return { kind: "pyenv", detail: spec.version };
    case "none":
      return { kind: "clean" };
    default:
      return { kind: spec.type };
  }
}

export function stepTable(s: Step): StepTable {
  return typeof s === "string" ? { run: s } : s;
}

/** Collapse a step table back to the compact string form when possible. */
export function compactStep(s: StepTable): Step {
  const t: StepTable = { run: s.run };
  if (s.name?.trim()) t.name = s.name.trim();
  if (s.cwd?.trim()) t.cwd = s.cwd.trim();
  if (s.continue_on_error) t.continue_on_error = true;
  return Object.keys(t).length === 1 ? t.run : t;
}

export function stepsOf(b: Bookmark): StepTable[] {
  if (b.command !== undefined && !(b.steps?.length)) return [{ run: b.command }];
  return (b.steps ?? []).map(stepTable);
}

export function relativeTime(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  if (s < 86400 * 7) return `${Math.round(s / 86400)}d ago`;
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function duration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${Math.round(s % 60)}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;
export const RESERVED = new Set([
  "add", "app", "completions", "doctor", "edit", "export", "folders", "go", "help", "history", "import", "init",
  "keys", "list", "locate", "ls", "mv", "pick", "remove", "rename", "rm", "run", "save", "scan", "server", "servers",
  "settings", "show", "ssh", "sync", "trust",
]);

/** Saved SSH key names: the same file name on every OS (lowercase, no dots). */
export function keyNameError(name: string): string | null {
  if (!name) return "Name is required";
  if (!/^[a-z0-9_][a-z0-9_-]{0,63}$/.test(name)) return "Lowercase letters, digits, - and _ only";
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/.test(name)) return "Windows can't use that name for a file";
  return null;
}

/** A key name from anything (a server name, a device name). */
export function toKeyName(s: string): string {
  const name = s.toLowerCase().replace(/['’]/g, "").replace(/[^a-z0-9_]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64);
  return keyNameError(name) ? "key" : name;
}

export function nameError(name: string): string | null {
  if (!name) return "Name is required";
  if (!NAME_RE.test(name)) return "Letters, digits, - and _ only";
  if (RESERVED.has(name)) return `"${name}" is a bk subcommand`;
  return null;
}

export { isMac } from "./platform";
