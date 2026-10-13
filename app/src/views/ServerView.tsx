import { useQueryClient } from "@tanstack/react-query";
import { FolderOpen, RefreshCw, ShieldCheck, SquareTerminal, Trash2 } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { KeyValueEditor, newKey, TagInput, type VarDraft } from "../components/editors";
import { Badge, Button, Dialog, Field, IconButton, Input, Select } from "../components/ui";
import { api, inTauri } from "../lib/api";
import { contractHome, joinPath } from "../lib/platform";
import { keys, useLibrary, useServerInventory, useServers, useServerStatus } from "../lib/queries";
import { ALLOWED_SSH_OPTIONS, type Reach, type Server } from "../lib/types";
import { cn, nameError, relativeTime } from "../lib/utils";
import { useRuns } from "../stores/runs";
import { useUi } from "../stores/ui";
import { EnvBadge } from "../components/bits";

export function reachColor(r?: Reach | null): string {
  switch (r?.state) {
    case "reachable":
      return "bg-ok";
    case "auth-required":
    case "unknown-host-key":
      return "bg-warn";
    case "changed-host-key":
    case "unreachable":
      return "bg-err";
    default:
      return "bg-line-strong";
  }
}

export function reachText(r?: Reach | null): string {
  switch (r?.state) {
    case "reachable":
      return `Reachable · ${r.ms} ms`;
    case "auth-required":
      return "Needs a password or a key that isn't loaded";
    case "unknown-host-key":
      return "Host key not trusted yet";
    case "changed-host-key":
      return "Host key CHANGED — refusing to connect";
    case "unreachable":
      return `Unreachable · ${r.reason}`;
    default:
      return "Not checked yet";
  }
}

export function ReachDot({ reach, className }: { reach?: Reach | null; className?: string }) {
  return <span className={cn("inline-block size-[7px] shrink-0 rounded-full", reachColor(reach), className)} />;
}

interface Draft {
  name: string;
  host: string;
  user: string;
  port: string;
  identity: string;
  jump: string;
  forward: string[];
  options: VarDraft[];
  description: string;
  shell: string;
}

function toDraft(name: string, s?: Server | null): Draft {
  return {
    name,
    host: s?.host ?? "",
    user: s?.user ?? "",
    port: s?.port ? String(s.port) : "",
    identity: s?.identity ?? "",
    jump: s?.jump ?? "",
    forward: s?.forward ?? [],
    options: Object.entries(s?.options ?? {}).map(([k, v]) => ({ key: newKey(), k, v })),
    description: s?.description ?? "",
    shell: s?.shell ?? "",
  };
}

function toServer(d: Draft): Server {
  const s: Server = { host: d.host.trim() };
  if (d.user.trim()) s.user = d.user.trim();
  if (d.port.trim()) s.port = Number(d.port);
  if (d.identity.trim()) s.identity = d.identity.trim();
  if (d.jump.trim()) s.jump = d.jump.trim();
  if (d.forward.length) s.forward = d.forward;
  const opts = Object.fromEntries(d.options.filter((o) => o.k).map((o) => [o.k, o.v]));
  if (Object.keys(opts).length) s.options = opts;
  if (d.description.trim()) s.description = d.description.trim();
  if (d.shell) s.shell = d.shell;
  return s;
}

function Panel({ title, children, actions }: { title: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <section className="rounded-lg border border-line">
      <header className="flex items-center justify-between border-b border-line px-4 py-2">
        <h3 className="text-xs font-semibold text-muted">{title}</h3>
        {actions}
      </header>
      <div className="px-4 py-3">{children}</div>
    </section>
  );
}

/** Confirms deleting a Clibooker server, or hiding an ~/.ssh/config host (that file is never edited). */
export function RemoveServerDialog() {
  const { removingServer: name, askRemoveServer } = useUi();
  const { data: servers } = useServers();
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const info = servers?.find((s) => s.name === name);
  if (!name) return null;
  const fromConfig = info?.source === "ssh-config";
  const used = info?.bookmarks ?? 0;
  const close = () => askRemoveServer(null);

  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: ["servers"] });
    await qc.invalidateQueries({ queryKey: keys.library });
  };

  const remove = async () => {
    setBusy(true);
    try {
      if (fromConfig) await api.hideServer(name, true);
      else await api.deleteServer(name);
      qc.removeQueries({ queryKey: ["server-status", name] });
      qc.removeQueries({ queryKey: ["server-inventory", name] });
      const ui = useUi.getState();
      if (ui.view === "server" && ui.selectedServer === name) useUi.setState({ view: "library", dirty: false });
      await refresh();
      close();
      const definition = info?.server;
      toast.success(fromConfig ? `Removed ${name} from the list` : `Deleted ${name}`, {
        description: fromConfig ? "It's still in ~/.ssh/config. Settings → Hidden servers shows it again." : undefined,
        action:
          fromConfig || definition
            ? {
                label: "Undo",
                onClick: () =>
                  void (fromConfig ? api.hideServer(name, false) : api.saveServer(null, name, definition!)).then(refresh),
              }
            : undefined,
      });
    } catch (e) {
      toast.error(`Could not remove ${name}`, { description: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const bookmarksNote = used ? ` ${used} bookmark${used === 1 ? " runs" : "s run"} on it.` : "";
  return (
    <Dialog
      open
      onOpenChange={(o) => !o && close()}
      title={fromConfig ? `Remove ${name} from the list?` : `Delete ${name}?`}
      description={
        fromConfig
          ? `It stays in ~/.ssh/config, so ssh and bookmarks that use it keep working.${bookmarksNote}`
          : `Removes the server from Clibooker.${used ? `${bookmarksNote} Pick another server for ${used === 1 ? "it" : "them"}.` : ""}`
      }
    >
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={close}>
          Cancel
        </Button>
        <Button variant="primary" className="bg-err text-white" disabled={busy} onClick={() => void remove()}>
          {fromConfig ? "Remove from list" : "Delete"}
        </Button>
      </div>
    </Dialog>
  );
}

export function ServerView() {
  const { selectedServer, select, selectServer, askRemoveServer, setDirty } = useUi();
  const { data: servers } = useServers();
  const { data: lib } = useLibrary();
  const qc = useQueryClient();
  const connect = useRuns((s) => s.connect);
  const isNew = selectedServer === "new";
  const info = servers?.find((s) => s.name === selectedServer);
  const editable = isNew || info?.source === "clibooker";
  const initial = useMemo(() => toDraft(isNew ? "" : (selectedServer ?? ""), info?.server), [selectedServer, info?.server, isNew]);
  const [draft, setDraft] = useState(initial);
  const [saving, setSaving] = useState(false);
  const [fingerprints, setFingerprints] = useState<string[] | null>(null);
  useEffect(() => setDraft(initial), [initial]);

  const dirty = JSON.stringify(toServer(draft)) + draft.name !== JSON.stringify(toServer(initial)) + initial.name;
  useEffect(() => setDirty(editable && dirty), [dirty, editable, setDirty]);

  const name = isNew ? null : selectedServer;
  const status = useServerStatus(name);
  const inventory = useServerInventory(name && status.data?.state === "reachable" ? name : null);
  const bookmarks = (lib?.entries ?? []).filter((e) => e.bookmark.server === name);

  if (!selectedServer) return null;
  const set = <K extends keyof Draft>(k: K, v: Draft[K]) => setDraft((d) => ({ ...d, [k]: v }));
  const nameErr = draft.name ? nameError(draft.name) : "Name is required";
  const canSave = editable && dirty && !nameErr && draft.host.trim() && !saving;

  const save = async () => {
    setSaving(true);
    try {
      await api.saveServer(isNew ? null : selectedServer, draft.name, toServer(draft));
      setDirty(false);
      await qc.invalidateQueries({ queryKey: ["servers"] });
      await qc.invalidateQueries({ queryKey: keys.library });
      selectServer(draft.name);
      toast.success(isNew ? `Added ${draft.name}` : "Saved");
    } catch (e) {
      toast.error("Could not save", { description: String(e) });
    } finally {
      setSaving(false);
    }
  };

  const recheck = async () => {
    await qc.invalidateQueries({ queryKey: ["server-status", name] });
    await qc.invalidateQueries({ queryKey: ["servers"] });
  };

  const rescan = async () => {
    if (!name) return;
    try {
      const inv = await api.serverInventory(name, true);
      qc.setQueryData(["server-inventory", name], inv);
      qc.invalidateQueries({ queryKey: ["servers"] });
      toast.success(`Scanned ${name}`);
    } catch (e) {
      toast.error(`Could not scan ${name}`, { description: String(e) });
    }
  };

  const reach = status.data ?? info?.status ?? null;
  const inv = inventory.data;

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
      <header data-tauri-drag-region className="flex h-[52px] shrink-0 items-center gap-2 border-b border-line px-5">
        <div data-tauri-drag-region className="flex min-w-0 flex-1 items-center gap-2">
          <h2 className="truncate text-[15px] font-semibold">{isNew ? "New server" : selectedServer}</h2>
          {info?.source === "ssh-config" && <Badge>~/.ssh/config</Badge>}
          {!isNew && (
            <span className="flex items-center gap-1.5 text-xs text-muted">
              <ReachDot reach={reach} /> {status.isFetching ? "Checking…" : reachText(reach)}
            </span>
          )}
        </div>
        {!isNew && (
          <>
            <IconButton label="Check connection" onClick={() => void recheck()}>
              <RefreshCw size={15} className={cn(status.isFetching && "animate-spin")} />
            </IconButton>
            {info && (
              <IconButton
                label={info.source === "clibooker" ? "Delete server" : "Remove from list"}
                className="hover:text-err"
                onClick={() => askRemoveServer(info.name)}
              >
                <Trash2 size={15} />
              </IconButton>
            )}
            <Button variant="run" onClick={() => void connect(selectedServer)} className="ml-1">
              <SquareTerminal size={13} /> Connect
            </Button>
          </>
        )}
      </header>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-[720px] flex-col gap-5 px-6 pt-5 pb-8">
          {reach?.state === "unknown-host-key" && (
            <div className="flex items-center gap-3 rounded-lg border border-warn/30 bg-warn/10 px-4 py-3 text-[13px]">
              <ShieldCheck size={16} className="shrink-0 text-warn" />
              <span className="flex-1">This server's host key isn't trusted yet. Check its fingerprint before connecting.</span>
              <Button
                size="sm"
                onClick={async () => {
                  try {
                    setFingerprints(await api.hostFingerprints(selectedServer));
                  } catch (e) {
                    toast.error("Couldn't fetch the host key", { description: String(e) });
                  }
                }}
              >
                Trust host…
              </Button>
            </div>
          )}
          {reach?.state === "changed-host-key" && (
            <div className="rounded-lg border border-err/30 bg-err-soft px-4 py-3 text-[13px] text-err">
              The server's host key has <b>changed</b> since you last connected. This can mean the server was reinstalled, or that
              someone is intercepting the connection. Clibooker won't connect until you remove the old key from ~/.ssh/known_hosts.
            </div>
          )}
          {reach?.state === "auth-required" && (
            <div className="rounded-lg border border-line bg-panel px-4 py-3 text-[13px] text-muted">
              The server wants a password or a key that isn't loaded. You can still <b>Connect</b> and type it in the terminal;
              background checks and env discovery need key-based login (<code className="font-mono">ssh-add</code>).
            </div>
          )}

          {editable ? (
            <>
              <div className="grid grid-cols-[1fr_1fr] gap-3">
                <Field label="Name" error={draft.name ? nameErr : null} hint="Used as `bk ssh <name>` and in bookmarks">
                  <Input value={draft.name} onChange={(e) => set("name", e.target.value)} placeholder="gpu" autoFocus={isNew} />
                </Field>
                <Field label="Description">
                  <Input value={draft.description} onChange={(e) => set("description", e.target.value)} placeholder="What's this server for?" />
                </Field>
              </div>
              <div className="grid grid-cols-[1fr_2fr_100px] gap-3">
                <Field label="User">
                  <Input mono value={draft.user} onChange={(e) => set("user", e.target.value)} placeholder="ubuntu" />
                </Field>
                <Field label="Host" hint="Hostname, IP, or an ~/.ssh/config alias">
                  <Input mono value={draft.host} onChange={(e) => set("host", e.target.value)} placeholder="10.0.0.5" />
                </Field>
                <Field label="Port">
                  <Input mono value={draft.port} onChange={(e) => set("port", e.target.value.replace(/\D/g, ""))} placeholder="22" />
                </Field>
              </div>
              <Field label="Private key" hint="Leave empty to use your ssh-agent / default keys">
                <div className="flex gap-2">
                  <Input mono value={draft.identity} onChange={(e) => set("identity", e.target.value)} placeholder="~/.ssh/id_ed25519" />
                  <Button
                    onClick={async () => {
                      if (!inTauri) return;
                      const { open } = await import("@tauri-apps/plugin-dialog");
                      const f = await open({ multiple: false, defaultPath: lib?.home ? joinPath(lib.home, ".ssh") : undefined });
                      if (typeof f === "string") set("identity", contractHome(f, lib?.home));
                    }}
                  >
                    <FolderOpen size={13} /> Choose…
                  </Button>
                </div>
              </Field>
              <Field label="Jump host" hint="Connect through a bastion: another server's name or user@host[:port]">
                <Input mono list="jump-servers" value={draft.jump} onChange={(e) => set("jump", e.target.value)} placeholder="bastion" />
                <datalist id="jump-servers">
                  {servers?.filter((s) => s.name !== draft.name).map((s) => <option key={s.name} value={s.name} />)}
                </datalist>
              </Field>
              <Field label="Port forwarding for sessions" hint="LOCAL_PORT:HOST:PORT, opened while you're connected (e.g. 8888:127.0.0.1:8888 for Jupyter)">
                <TagInput tags={draft.forward} onChange={(f) => set("forward", f)} suggestions={["8888:127.0.0.1:8888", "6006:127.0.0.1:6006"]} prefix="" placeholder="8888:127.0.0.1:8888" mono />
              </Field>
              <div className="grid grid-cols-[1fr_200px] gap-3">
                <Field label="SSH options" hint="Others (ProxyCommand, …) belong in ~/.ssh/config">
                  <KeyValueEditor
                    rows={draft.options}
                    onChange={(o) => set("options", o)}
                    addLabel="Add option"
                    keyPlaceholder="ServerAliveInterval"
                    valuePlaceholder="30"
                  />
                  <span className="text-[11px] text-faint">Allowed: {ALLOWED_SSH_OPTIONS.join(", ")}</span>
                </Field>
                <Field label="Shell for runs">
                  <Select value={draft.shell} onChange={(e) => set("shell", e.target.value)}>
                    <option value="">Auto (bash, else sh)</option>
                    <option value="bash">bash</option>
                    <option value="sh">sh</option>
                    <option value="zsh">zsh</option>
                  </Select>
                </Field>
              </div>
              {(dirty || isNew) && (
                <div className="flex justify-end gap-2">
                  {!isNew && (
                    <Button variant="ghost" onClick={() => setDraft(initial)}>
                      Revert
                    </Button>
                  )}
                  <Button variant="primary" disabled={!canSave} onClick={() => void save()}>
                    {saving ? "Saving…" : isNew ? "Add server" : "Save"}
                  </Button>
                </div>
              )}
            </>
          ) : (
            info && (
              <div className="flex items-center gap-3 rounded-lg border border-line bg-panel px-4 py-3 text-[13px] text-muted">
                <span className="flex-1">
                  Defined in <code className="font-mono">~/.ssh/config</code>; Clibooker connects with your settings there.
                </span>
                <Button
                  size="sm"
                  onClick={async () => {
                    const copy = info.name.replace(/[^A-Za-z0-9_-]/g, "-");
                    try {
                      await api.saveServer(null, copy, { host: info.name });
                      // The copy takes the host's place in the list (same name already shadows it).
                      if (copy !== info.name) await api.hideServer(info.name, true);
                      await qc.invalidateQueries({ queryKey: ["servers"] });
                      await qc.invalidateQueries({ queryKey: keys.library });
                      selectServer(copy);
                      toast.success("Copied; add options here without touching ~/.ssh/config");
                    } catch (e) {
                      toast.error("Could not copy", { description: String(e) });
                    }
                  }}
                >
                  Copy to Clibooker
                </Button>
              </div>
            )
          )}

          {!isNew && (
            <Panel
              title="ON THE SERVER"
              actions={
                <button onClick={() => void rescan()} className="flex items-center gap-1 text-xs font-medium text-accent hover:underline">
                  <RefreshCw size={11} /> Scan again
                </button>
              }
            >
              {inv ? (
                <div className="flex flex-col gap-2 text-xs">
                  <div className="text-muted">
                    {inv.os} · {inv.shell} · scanned {relativeTime(inv.probed_ms)}
                  </div>
                  <div className="flex flex-wrap gap-1">
                    {inv.tools.map((t) => (
                      <Badge key={t} tone={t === "tmux" ? "accent" : "neutral"}>
                        {t}
                      </Badge>
                    ))}
                  </div>
                  {inv.conda_envs.length > 0 && (
                    <div className="flex flex-wrap items-center gap-1">
                      <span className="w-14 text-faint">conda</span>
                      {inv.conda_envs.map((e) => (
                        <EnvBadge key={e.prefix} env={{ type: "conda", name: e.name }} />
                      ))}
                    </div>
                  )}
                  {Object.entries(inv.runtimes).map(([lang, list]) => (
                    <div key={lang} className="flex items-baseline gap-1">
                      <span className="w-14 text-faint">{lang}</span>
                      <span className="font-mono text-[11px]">{list?.map((i) => i.version).join(", ")}</span>
                    </div>
                  ))}
                </div>
              ) : (
                <p className="text-xs text-faint">
                  {inventory.isFetching
                    ? "Scanning…"
                    : reach?.state === "reachable"
                      ? "Not scanned yet."
                      : "Envs and runtimes are listed once the server is reachable with key-based login."}
                </p>
              )}
            </Panel>
          )}

          {!isNew && (
            <Panel title={`BOOKMARKS ON ${selectedServer.toUpperCase()}`}>
              {bookmarks.length === 0 ? (
                <p className="text-xs text-faint">None yet. In a bookmark, set “Runs on” to {selectedServer}.</p>
              ) : (
                <div className="flex flex-col">
                  {bookmarks.map((b) => (
                    <button key={b.id} onClick={() => select(b.id)} className="flex items-center gap-2 rounded px-1 py-1 text-left text-[13px] hover:bg-hover">
                      <span className="font-medium">{b.name}</span>
                      <span className="truncate font-mono text-[11px] text-muted">{b.summary}</span>
                    </button>
                  ))}
                </div>
              )}
            </Panel>
          )}
        </div>
      </div>

      <Dialog open={fingerprints !== null} onOpenChange={(o) => !o && setFingerprints(null)} title={`Trust ${selectedServer}?`} description="Compare with the fingerprint shown by the server's admin or console (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub).">
        <div className="mb-4 flex flex-col gap-1 rounded-lg border border-line bg-panel p-3">
          {fingerprints?.map((f) => (
            <code key={f} className="selectable font-mono text-[11px] break-all">
              {f}
            </code>
          ))}
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setFingerprints(null)}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={async () => {
              const r = await api.trustHost(selectedServer).catch((e) => {
                toast.error(String(e));
                return null;
              });
              setFingerprints(null);
              if (r) qc.setQueryData(["server-status", selectedServer], r);
              qc.invalidateQueries({ queryKey: ["servers"] });
            }}
          >
            Trust host key
          </Button>
        </div>
      </Dialog>
    </div>
  );
}
