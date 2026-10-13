/** The OS this window runs on, known synchronously (before the first backend call). */
export type Platform = "macos" | "windows" | "linux";

function detect(): Platform {
  if (typeof navigator === "undefined") return "macos";
  // Browser preview only: `?platform=windows|linux` shows another OS's UI against the mock.
  if (typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window)) {
    const p = new URLSearchParams(window.location.search).get("platform");
    if (p === "macos" || p === "windows" || p === "linux") return p;
  }
  const s = `${navigator.userAgent} ${navigator.platform}`;
  if (/Mac/.test(s)) return "macos";
  if (/Win/.test(s)) return "windows";
  return "linux";
}

export const platform: Platform = detect();
export const isMac = platform === "macos";
export const isWindows = platform === "windows";
export const isLinux = platform === "linux";

/** The primary shortcut modifier: ⌘ on macOS, Ctrl elsewhere. */
export function isMod(e: { metaKey: boolean; ctrlKey: boolean }) {
  return isMac ? e.metaKey : e.ctrlKey;
}

const MAC_KEYS: Record<string, string> = {
  Mod: "⌘",
  Cmd: "⌘",
  CmdOrCtrl: "⌘",
  Super: "⌘",
  Ctrl: "⌃",
  Alt: "⌥",
  Option: "⌥",
  Shift: "⇧",
  Enter: "↩",
  Space: " Space",
};
const OTHER_KEYS: Record<string, string> = { Mod: "Ctrl", CmdOrCtrl: "Ctrl", Cmd: "Super", Option: "Alt", Enter: "↩" };

/** A shortcut label: `kbd("Mod+N")` is "⌘N" on macOS and "Ctrl+N" elsewhere. Also takes Tauri accelerators. */
export function kbd(spec: string): string {
  const parts = spec.split("+").filter(Boolean);
  if (isMac) return parts.map((p) => MAC_KEYS[p] ?? p).join("");
  return parts.map((p) => OTHER_KEYS[p] ?? p).join("+");
}

/** Where the window menu lives: the macOS menu bar, the system tray elsewhere. */
export const trayName = isMac ? "menu bar" : "system tray";

export type ShellKind = "zsh" | "bash" | "powershell";

/** The rc file and the line that loads bk's shell integration. */
export function shellSetup(shell: string, record = false): { file: string; line: string } {
  const flag = record ? " --record" : "";
  if (shell === "powershell") return { file: "$PROFILE", line: `bk init powershell${flag} | Out-String | Invoke-Expression` };
  if (shell === "bash") return { file: "~/.bashrc", line: `eval "$(bk init bash${flag})"` };
  return { file: "~/.zshrc", line: `eval "$(bk init zsh${flag})"` };
}

/** `C:\x`, `\\server\x`, `/x` or `~/x`. */
export function isAbsolutePath(p: string) {
  return /^([A-Za-z]:[\\/]|\\\\|\/|~([\\/]|$))/.test(p);
}

/** Join a folder and a relative path with the folder's own separator. */
export function joinPath(dir: string, rest: string) {
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + rest;
}

/** `home/x` → `~/x` (any separator, case-insensitive on Windows). */
export function contractHome(p: string, home: string | undefined) {
  if (!home) return p;
  const norm = (s: string) => (isWindows ? s.toLowerCase().replace(/\\/g, "/") : s);
  const h = norm(home).replace(/\/+$/, "");
  const n = norm(p);
  if (n === h) return "~";
  if (n.startsWith(`${h}/`)) return `~/${p.slice(home.replace(/[\\/]+$/, "").length + 1).replace(/\\/g, "/")}`;
  return p;
}
