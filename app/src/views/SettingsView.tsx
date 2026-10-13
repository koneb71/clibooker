import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Copy, FolderGit2, RefreshCw, TerminalSquare, X } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { toast } from "sonner";
import { api, copyText, inTauri } from "../lib/api";
import { keys, useInventory, useLibrary } from "../lib/queries";
import type { Settings } from "../lib/types";
import { useTheme } from "../stores/theme";
import { Button, Field, IconButton, Input, Select, Switch } from "../components/ui";

function Group({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <section className="grid grid-cols-[200px_1fr] gap-6 border-b border-line py-6 last:border-b-0">
      <div>
        <h2 className="text-[13px] font-semibold">{title}</h2>
        {description && <p className="mt-1 text-xs leading-relaxed text-muted">{description}</p>}
      </div>
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

/** Turn a keydown into a Tauri accelerator string like "Alt+Space". */
function accelerator(e: React.KeyboardEvent): string | null {
  const mods = [e.metaKey && "Cmd", e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift"].filter(Boolean) as string[];
  const code = e.code;
  let key: string | null = null;
  if (code === "Space") key = "Space";
  else if (code.startsWith("Key")) key = code.slice(3);
  else if (code.startsWith("Digit")) key = code.slice(5);
  else if (/^F\d+$/.test(code)) key = code;
  else if (["Semicolon", "Quote", "Comma", "Period", "Slash", "Backquote", "BracketLeft", "BracketRight", "Backslash", "Minus", "Equal"].includes(code)) key = code;
  if (!key || mods.length === 0) return null;
  return [...mods, key].join("+");
}

function prettyHotkey(h: string) {
  return h
    .replace(/Cmd|CmdOrCtrl|Super/g, "⌘")
    .replace(/Ctrl/g, "⌃")
    .replace(/Alt|Option/g, "⌥")
    .replace(/Shift/g, "⇧")
    .replace(/\+/g, "")
    .replace("Space", " Space");
}

export function SettingsView() {
  const { data: lib } = useLibrary();
  const { data: inventory } = useInventory();
  const qc = useQueryClient();
  const setThemePref = useTheme((s) => s.setPref);
  const [recording, setRecording] = useState(false);
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [reloading, setReloading] = useState(false);
  const { data: cli, refetch: refetchCli } = useQuery({ queryKey: ["cli-status"], queryFn: api.cliStatus });

  useEffect(() => {
    if (!inTauri) return;
    void import("@tauri-apps/plugin-autostart").then((m) => m.isEnabled().then(setLaunchAtLogin));
  }, []);

  if (!lib) return null;
  const s = lib.settings;

  const save = async (patch: Partial<Settings>) => {
    try {
      await api.saveSettings({ ...s, ...patch });
      await qc.invalidateQueries({ queryKey: keys.library });
    } catch (e) {
      toast.error("Could not save settings", { description: String(e) });
    }
  };

  const hotkey = s.hotkey ?? "Alt+Space";

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
      <header data-tauri-drag-region className="flex h-[52px] shrink-0 items-center border-b border-line px-5">
        <h1 data-tauri-drag-region className="text-[15px] font-semibold">Settings</h1>
      </header>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-[760px] px-6">
          <Group title="Appearance">
            <Field label="Theme">
              <Select
                className="w-[200px]"
                value={s.theme ?? "system"}
                onChange={(e) => {
                  const theme = e.target.value as Settings["theme"];
                  setThemePref(theme ?? "system");
                  void save({ theme });
                }}
              >
                <option value="system">Match system</option>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </Select>
            </Field>
            <Field label="Terminal font size">
              <Select
                className="w-[200px]"
                value={String(s.terminal_font_size ?? 12.5)}
                onChange={(e) => void save({ terminal_font_size: Number(e.target.value) })}
              >
                {[11, 12, 12.5, 13, 14, 15, 16].map((n) => (
                  <option key={n} value={n}>
                    {n}px
                  </option>
                ))}
              </Select>
            </Field>
          </Group>

          <Group title="Quick run" description="A Spotlight-style launcher for your bookmarks, from anywhere.">
            <Field label="Global shortcut" error={hotkeyError}>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => setRecording(true)}
                  onBlur={() => setRecording(false)}
                  onKeyDown={async (e) => {
                    if (!recording) return;
                    e.preventDefault();
                    if (e.key === "Escape") return setRecording(false);
                    const acc = accelerator(e);
                    if (!acc) return;
                    setRecording(false);
                    try {
                      await api.setHotkey(acc);
                      setHotkeyError(null);
                      await qc.invalidateQueries({ queryKey: keys.library });
                      toast.success(`Quick run is now ${prettyHotkey(acc)}`);
                    } catch (err) {
                      setHotkeyError(String(err));
                    }
                  }}
                  className="h-7 min-w-[140px] rounded-md border border-line-strong bg-raised px-3 text-left font-mono text-[12px] outline-none focus:border-accent focus:ring-3 focus:ring-accent-soft"
                >
                  {recording ? <span className="text-accent">Press keys…</span> : prettyHotkey(hotkey)}
                </button>
                <span className="text-xs text-faint">Click, then press the new combination</span>
              </div>
            </Field>
          </Group>

          <Group title="Running" description="How bookmarks run inside Clibooker and the bk CLI.">
            <Field label="Default shell" hint="Steps run non-interactively, without your .zshrc (aliases aren't available).">
              <Select className="w-[200px]" value={s.shell ?? ""} onChange={(e) => void save({ shell: e.target.value || undefined })}>
                <option value="">From $SHELL (zsh)</option>
                <option value="zsh">zsh</option>
                <option value="bash">bash</option>
                <option value="sh">sh</option>
              </Select>
            </Field>
            <Field label="ssh program" hint="Default /usr/bin/ssh (understands macOS Keychain). Change it if you use Homebrew's OpenSSH.">
              <Input
                mono
                className="w-[320px]"
                defaultValue={s.ssh ?? ""}
                placeholder="/usr/bin/ssh"
                onBlur={(e) => {
                  const v = e.target.value.trim();
                  if (v !== (s.ssh ?? "")) void save({ ssh: v || undefined });
                }}
              />
            </Field>
            {s.hidden_servers?.length ? (
              <Field label="Hidden servers" hint="Hosts from ~/.ssh/config you removed from the server list. They still work by name.">
                <div className="flex flex-wrap gap-1.5">
                  {s.hidden_servers.map((h) => (
                    <span key={h} className="flex items-center gap-1 rounded-md border border-line bg-panel py-0.5 pr-0.5 pl-2 font-mono text-[12px]">
                      {h}
                      <button
                        className="rounded px-1.5 py-0.5 font-sans text-[11px] font-medium text-accent hover:bg-accent-soft"
                        onClick={async () => {
                          try {
                            await api.hideServer(h, false);
                            await qc.invalidateQueries({ queryKey: keys.library });
                            await qc.invalidateQueries({ queryKey: ["servers"] });
                          } catch (e) {
                            toast.error(`Could not show ${h}`, { description: String(e) });
                          }
                        }}
                      >
                        Show
                      </button>
                    </span>
                  ))}
                </div>
              </Field>
            ) : null}
            <Field label="Pop-out terminal" hint="Used by “Open in terminal”.">
              <Select className="w-[200px]" value={s.terminal ?? "terminal"} onChange={(e) => void save({ terminal: e.target.value })}>
                <option value="terminal">Terminal</option>
                <option value="iterm">iTerm</option>
                <option value="ghostty">Ghostty</option>
                <option value="warp">Warp</option>
              </Select>
            </Field>
            <Switch
              checked={s.history ?? true}
              onChange={(history) => void save({ history })}
              label="Keep run history"
              hint="Commands, params, exit codes and durations. Environment values are never stored."
            />
            <div className="flex items-center gap-3">
              <Button
                disabled={reloading}
                onClick={async () => {
                  setReloading(true);
                  try {
                    const n = await api.reloadShellEnv();
                    toast.success(`Reloaded shell environment (${n} variables)`);
                    qc.invalidateQueries({ queryKey: keys.inventory });
                  } catch (e) {
                    toast.error("Could not read your login shell", { description: String(e) });
                  } finally {
                    setReloading(false);
                  }
                }}
              >
                <RefreshCw size={13} className={reloading ? "animate-spin" : ""} /> Reload shell environment
              </Button>
              <span className="text-xs text-faint">After editing ~/.zshrc (PATH, conda init…)</span>
            </div>
            {inventory && (
              <p className="text-xs leading-relaxed text-muted">
                Found {inventory.conda_envs.length} conda envs{inventory.conda_base ? ` in ${inventory.conda_base}` : ""}
                {inventory.tools.uv && " · uv"}
                {inventory.tools.poetry && " · poetry"}
                {inventory.tools.pyenv && ` · pyenv (${inventory.pyenv_versions.length} versions)`}
              </p>
            )}
          </Group>

          <Group title="App">
            <Switch
              checked={launchAtLogin}
              onChange={async (v) => {
                if (!inTauri) return setLaunchAtLogin(v);
                const m = await import("@tauri-apps/plugin-autostart");
                await (v ? m.enable() : m.disable());
                setLaunchAtLogin(await m.isEnabled());
                void save({ launch_at_login: v });
              }}
              label="Launch at login"
              hint="Starts in the menu bar so the quick-run shortcut always works"
            />
            <Switch
              checked={s.show_in_dock ?? true}
              onChange={async (v) => {
                await api.setDockVisible(v).catch(() => {});
                void save({ show_in_dock: v });
              }}
              label="Show in Dock"
              hint="When off, Clibooker lives only in the menu bar"
            />
          </Group>

          <Group title="Projects" description="Repos with a .bk.toml. Their bookmarks live in the repo and can be committed.">
            {lib.projects.length === 0 && <p className="text-xs text-faint">None yet. Use + next to Projects in the sidebar.</p>}
            {lib.projects.map((p) => (
              <div key={p.file} className="flex items-center gap-2 rounded-md border border-line px-3 py-2">
                <FolderGit2 size={14} className="text-muted" />
                <span className="text-[13px] font-medium">{p.name}</span>
                <span className="selectable flex-1 truncate font-mono text-[11px] text-faint">{p.dir}</span>
                <IconButton
                  label="Remove from list (keeps the file)"
                  onClick={async () => {
                    await api.removeProject(p.file);
                    qc.invalidateQueries({ queryKey: keys.library });
                  }}
                >
                  <X size={13} />
                </IconButton>
              </div>
            ))}
          </Group>

          <Group
            title="Command line"
            description={
              <>
                The <code className="font-mono">bk</code> CLI shares these bookmarks. The shell hook adds{" "}
                <code className="font-mono">bk save</code> and <code className="font-mono">bk go</code>.
              </>
            }
          >
            {cli && (
              <div className="flex items-center gap-3 rounded-lg border border-line px-3 py-2.5">
                <TerminalSquare size={16} className="shrink-0 text-muted" />
                <div className="min-w-0 flex-1 text-xs">
                  {cli.on_path ? (
                    <span className="flex items-center gap-1.5 text-fg">
                      <CheckCircle2 size={13} className="text-ok" /> <code className="font-mono">bk</code> is installed at{" "}
                      <code className="selectable font-mono">{cli.on_path}</code>
                    </span>
                  ) : (
                    <span className="text-muted">
                      <code className="font-mono">bk</code> isn't on your PATH yet.
                    </span>
                  )}
                  {cli.linked && !cli.link_dir_on_path && (
                    <p className="mt-1 text-warn">Add ~/.local/bin to your PATH to use it.</p>
                  )}
                </div>
                {cli.bundled && !cli.linked && (
                  <Button
                    size="sm"
                    onClick={async () => {
                      try {
                        const where = await api.installCli();
                        toast.success(`Linked bk to ${where}`);
                        await api.reloadShellEnv().catch(() => {});
                        void refetchCli();
                      } catch (e) {
                        toast.error("Could not install", { description: String(e) });
                      }
                    }}
                  >
                    Install bk command
                  </Button>
                )}
              </div>
            )}
            {[
              ["Add to ~/.zshrc", 'eval "$(bk init zsh)"'],
              ["Bookmarks file", lib.global_file],
            ].map(([label, value]) => (
              <Field key={label} label={label}>
                <div className="flex gap-2">
                  <Input mono readOnly value={value} className="flex-1" />
                  <IconButton label="Copy" onClick={() => void copyText(value).then(() => toast.success("Copied"))}>
                    <Copy size={13} />
                  </IconButton>
                </div>
              </Field>
            ))}
          </Group>
        </div>
      </div>
    </div>
  );
}
