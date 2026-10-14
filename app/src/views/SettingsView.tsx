import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Copy, ExternalLink, FolderGit2, Loader2, RefreshCw, Send, TerminalSquare, X } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { toast } from "sonner";
import { api, copyText, inTauri } from "../lib/api";
import { keys, useInventory, useLibrary } from "../lib/queries";
import { isLinux, isMac, isWindows, kbd, shellSetup, trayName } from "../lib/platform";
import type { Settings, SyncContact } from "../lib/types";
import { relativeTime } from "../lib/utils";
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
  const mods = [e.metaKey && (isMac ? "Cmd" : "Super"), e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift"].filter(
    Boolean,
  ) as string[];
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

const prettyHotkey = kbd;

/** Step shells offered per platform ("" = the default). */
function shellOptions(userShell: string): [string, string][] {
  if (isWindows) {
    return [
      ["", "PowerShell (pwsh if installed)"],
      ["pwsh", "PowerShell 7 (pwsh)"],
      ["powershell", "Windows PowerShell 5.1"],
      ["cmd", "Command Prompt (cmd)"],
      ["bash", "Git Bash"],
    ];
  }
  const fallback = userShell === "zsh" || userShell === "bash" ? userShell : "sh";
  return [
    ["", `Your login shell (${fallback})`],
    ["zsh", "zsh"],
    ["bash", "bash"],
    ["sh", "sh"],
  ];
}

const TERMINALS: [string, string][] = isMac
  ? [
      ["terminal", "Terminal"],
      ["iterm", "iTerm"],
      ["ghostty", "Ghostty"],
      ["warp", "Warp"],
    ]
  : isWindows
    ? [
        ["windows-terminal", "Windows Terminal"],
        ["powershell", "PowerShell window"],
      ]
    : [
        ["auto", "Automatic"],
        ["gnome-terminal", "GNOME Terminal"],
        ["konsole", "Konsole"],
        ["xfce4-terminal", "Xfce Terminal"],
        ["kitty", "kitty"],
        ["alacritty", "Alacritty"],
        ["wezterm", "WezTerm"],
        ["ghostty", "Ghostty"],
        ["xterm", "xterm"],
      ];

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

  const hotkey = s.hotkey ?? lib.default_hotkey;
  const setup = shellSetup(lib.shell);

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

          <Group
            title="Quick run"
            description={
              isMac
                ? "A Spotlight-style launcher for your bookmarks, from anywhere."
                : "A quick launcher for your bookmarks, from any app."
            }
          >
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
            {isLinux && (
              <p className="text-xs leading-relaxed text-muted">
                On Wayland, global shortcuts are up to your desktop: add a custom shortcut in its keyboard settings that runs{" "}
                <code className="selectable font-mono text-fg">Clibooker --palette</code>.
              </p>
            )}
          </Group>

          <Group title="Running" description="How bookmarks run inside Clibooker and the bk CLI.">
            <Field
              label="Default shell"
              hint={
                isWindows
                  ? "Steps run without your PowerShell profile (its functions and aliases aren't available)."
                  : `Steps run non-interactively, without your ${setup.file} (aliases aren't available).`
              }
            >
              <Select className="w-[240px]" value={s.shell ?? ""} onChange={(e) => void save({ shell: e.target.value || undefined })}>
                {shellOptions(lib.shell).map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
              </Select>
            </Field>
            <Field
              label="ssh program"
              hint={
                isMac
                  ? "Default /usr/bin/ssh (understands macOS Keychain). Change it if you use Homebrew's OpenSSH."
                  : isWindows
                    ? "Default: Windows' built-in OpenSSH."
                    : "Default /usr/bin/ssh."
              }
            >
              <Input
                mono
                className="w-[320px]"
                defaultValue={s.ssh ?? ""}
                placeholder={isWindows ? "C:\\Windows\\System32\\OpenSSH\\ssh.exe" : "/usr/bin/ssh"}
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
            <Field
              label="Pop-out terminal"
              hint={isLinux ? "Used by “Open in terminal”. Automatic uses $TERMINAL, then the first one installed." : "Used by “Open in terminal”."}
            >
              <Select
                className="w-[240px]"
                value={s.terminal ?? lib.default_terminal}
                onChange={(e) => void save({ terminal: e.target.value })}
              >
                {TERMINALS.map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
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
                    toast.success(`Reloaded ${isWindows ? "the" : "shell"} environment (${n} variables)`);
                    qc.invalidateQueries({ queryKey: keys.inventory });
                  } catch (e) {
                    toast.error(isWindows ? "Could not read your environment" : "Could not read your login shell", {
                      description: String(e),
                    });
                  } finally {
                    setReloading(false);
                  }
                }}
              >
                <RefreshCw size={13} className={reloading ? "animate-spin" : ""} /> Reload {isWindows ? "" : "shell "}environment
              </Button>
              <span className="text-xs text-faint">
                {isWindows ? "After installing tools or changing environment variables" : `After editing ${setup.file} (PATH, conda init…)`}
              </span>
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
              hint={`Starts in the ${trayName} so the quick-run shortcut always works`}
            />
            <Switch
              checked={s.keep_running ?? true}
              onChange={(keep_running) => void save({ keep_running })}
              label="Keep running when the window is closed"
              hint={`Runs continue and the quick-run shortcut keeps working; quit from the ${trayName}.`}
            />
            {isMac && (
              <Switch
                checked={s.show_in_dock ?? true}
                onChange={async (v) => {
                  await api.setDockVisible(v).catch(() => {});
                  void save({ show_in_dock: v });
                }}
                label="Show in Dock"
                hint="When off, Clibooker lives only in the menu bar"
              />
            )}
          </Group>

          <SyncGroup />

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
                    <p className="mt-1 text-warn">
                      {isWindows ? "Open a new terminal to use it." : "Add ~/.local/bin to your PATH to use it."}
                    </p>
                  )}
                </div>
                {cli.bundled && !cli.linked && (
                  <Button
                    size="sm"
                    onClick={async () => {
                      try {
                        const where = await api.installCli();
                        toast.success(isWindows ? `Added ${where} to your PATH` : `Installed bk at ${where}`);
                        await api.reloadShellEnv().catch(() => {});
                        void refetchCli();
                      } catch (e) {
                        toast.error("Could not install", { description: String(e) });
                      }
                    }}
                  >
                    {isWindows ? "Add bk to PATH" : "Install bk command"}
                  </Button>
                )}
              </div>
            )}
            {[
              [`Add to ${setup.file}`, setup.line],
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

function openLink(url: string) {
  if (inTauri) void import("@tauri-apps/plugin-opener").then((m) => m.openUrl(url));
  else window.open(url, "_blank");
}

type SetupStep = { at: "token" } | { at: "start"; bot: string } | { at: "passphrase"; bot: string; contact: SyncContact };

/** Settings → Sync: connect this device to the user's Telegram bot, then sync on demand. */
function SyncGroup() {
  const qc = useQueryClient();
  const { data: st, refetch } = useQuery({ queryKey: ["sync-status"], queryFn: api.syncStatus });
  const [step, setStep] = useState<SetupStep | null>(null);
  const [token, setToken] = useState("");
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");
  const [device, setDevice] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshAll = async () => {
    await refetch();
    await qc.invalidateQueries({ queryKey: keys.library });
    await qc.invalidateQueries({ queryKey: ["history"] });
    await qc.invalidateQueries({ queryKey: ["servers"] });
  };

  const syncNow = async () => {
    setBusy(true);
    try {
      const r = await api.syncNow();
      toast.success(`Synced: ${r.line}`, { description: [...r.skipped.map((x) => `Skipped ${x}`), r.warning].filter(Boolean).join("\n") || undefined });
      await refreshAll();
    } catch (e) {
      toast.error("Sync failed", { description: String(e) });
    } finally {
      setBusy(false);
    }
  };

  // Step 2 waits for the user to press Start in Telegram.
  useEffect(() => {
    if (step?.at !== "start") return;
    let cancelled = false;
    setError(null);
    api
      .syncWaitForStart(token)
      .then((contact) => !cancelled && setStep({ at: "passphrase", bot: step.bot, contact }))
      .catch((e) => !cancelled && setError(String(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step?.at]);

  const description = (
    <>
      Keep bookmarks, servers, settings and run history the same on all your devices, through a Telegram bot only you use.
      Everything is encrypted on this device before it's sent.
    </>
  );

  if (!st) return null;

  if (st.configured && !step) {
    return (
      <Group title="Sync" description={description}>
        <div className="flex items-center gap-3 rounded-lg border border-line px-3 py-2.5 text-xs">
          <Send size={15} className="shrink-0 text-muted" />
          <div className="min-w-0 flex-1">
            <div className="text-fg">
              <b>{st.device}</b> syncs with <code className="font-mono">@{st.bot}</code>
            </div>
            <div className="text-muted">
              {st.last_sync_ms ? `Last synced ${relativeTime(st.last_sync_ms)} · ${st.last_summary ?? ""}` : "Not synced yet"}
            </div>
          </div>
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void syncNow()}>
            {busy ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />} Sync now
          </Button>
        </div>
        <div>
          <Button
            variant="ghost"
            size="sm"
            onClick={async () => {
              if (!window.confirm("Stop syncing on this device? Your bookmarks stay; the chat in Telegram is left as it is.")) return;
              await api.syncForget();
              await refetch();
            }}
          >
            Stop syncing on this device
          </Button>
        </div>
      </Group>
    );
  }

  if (!step) {
    return (
      <Group title="Sync" description={description}>
        <div>
          <Button onClick={() => setStep({ at: "token" })}>
            <Send size={13} /> Set up sync
          </Button>
        </div>
      </Group>
    );
  }

  const cancel = (
    <Button
      variant="ghost"
      onClick={() => {
        setStep(null);
        setError(null);
      }}
    >
      Cancel
    </Button>
  );

  return (
    <Group title="Sync" description={description}>
      {step.at === "token" && (
        <>
          <p className="text-xs leading-relaxed text-muted">
            <b className="text-fg">1.</b> In Telegram, message{" "}
            <button className="font-medium text-accent hover:underline" onClick={() => openLink("https://t.me/BotFather")}>
              @BotFather
            </button>
            , send <code className="font-mono">/newbot</code>, pick a name, and paste the token it gives you. Keep the token private.
          </p>
          <Field label="Bot token" error={error}>
            <Input mono type="password" value={token} onChange={(e) => setToken(e.target.value.trim())} placeholder="123456789:AA…" autoFocus />
          </Field>
          <div className="flex gap-2">
            <Button
              variant="primary"
              disabled={!token || busy}
              onClick={async () => {
                setBusy(true);
                setError(null);
                try {
                  setStep({ at: "start", bot: await api.syncCheckToken(token) });
                } catch (e) {
                  setError(String(e));
                } finally {
                  setBusy(false);
                }
              }}
            >
              {busy ? "Checking…" : "Continue"}
            </Button>
            {cancel}
          </div>
        </>
      )}

      {step.at === "start" && (
        <>
          <p className="text-xs leading-relaxed text-muted">
            <b className="text-fg">2.</b> Open your bot in Telegram and press <b>Start</b>, so it knows which chat to keep your sync data in.
          </p>
          <div className="flex items-center gap-3">
            <Button onClick={() => openLink(`https://t.me/${step.bot}`)}>
              <ExternalLink size={13} /> Open @{step.bot}
            </Button>
            {!error && (
              <span className="flex items-center gap-1.5 text-xs text-muted">
                <Loader2 size={12} className="animate-spin" /> Waiting for Start…
              </span>
            )}
          </div>
          {error && (
            <div className="flex items-center gap-2">
              <p className="flex-1 text-xs text-err">{error}</p>
              <Button size="sm" onClick={() => setStep({ at: "start", bot: step.bot })}>
                Try again
              </Button>
            </div>
          )}
          <div>{cancel}</div>
        </>
      )}

      {step.at === "passphrase" && (
        <>
          <p className="text-xs leading-relaxed text-muted">
            <b className="text-fg">3.</b>{" "}
            {step.contact.has_data
              ? "This bot already holds synced data: enter the passphrase your other devices use."
              : "Choose a passphrase. You'll enter it once on each device; it can't be recovered."}
          </p>
          <div className="grid grid-cols-2 gap-3">
            <Field label="Passphrase" error={error}>
              <Input type="password" value={pass} onChange={(e) => setPass(e.target.value)} autoFocus />
            </Field>
            {!step.contact.has_data && (
              <Field label="Repeat it" error={pass2 && pass2 !== pass ? "Doesn't match" : null}>
                <Input type="password" value={pass2} onChange={(e) => setPass2(e.target.value)} />
              </Field>
            )}
          </div>
          <Field label="Name for this device">
            <Input className="w-[260px]" value={device} placeholder={st.default_device} onChange={(e) => setDevice(e.target.value)} />
          </Field>
          <div className="flex gap-2">
            <Button
              variant="primary"
              disabled={busy || pass.length < 8 || (!step.contact.has_data && pass !== pass2)}
              onClick={async () => {
                setBusy(true);
                setError(null);
                try {
                  await api.syncFinishSetup(token, step.bot, step.contact, pass, device || st.default_device);
                  setStep(null);
                  setPass("");
                  setPass2("");
                  setToken("");
                  await refetch();
                  setBusy(false);
                  await syncNow();
                } catch (e) {
                  setError(String(e));
                } finally {
                  setBusy(false);
                }
              }}
            >
              {busy ? "Connecting…" : "Finish and sync"}
            </Button>
            {cancel}
          </div>
          {pass.length > 0 && pass.length < 8 && <p className="text-xs text-faint">At least 8 characters.</p>}
        </>
      )}
    </Group>
  );
}

