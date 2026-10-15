// Saved SSH keys: making and importing them, picking one for a server, putting one on a server.
// Passphrases and passwords are only ever typed into OpenSSH's own prompts, in a terminal tab.

import { useQueryClient } from "@tanstack/react-query";
import { FolderOpen, KeyRound, LockKeyhole, LockKeyholeOpen, TriangleAlert, Upload } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api, pickFile } from "../lib/api";
import { contractHome, joinPath } from "../lib/platform";
import { useKeys, useLibrary } from "../lib/queries";
import { type KeyType, savedKeyName, type SshKey } from "../lib/types";
import { cn, keyNameError, toKeyName } from "../lib/utils";
import { useRuns } from "../stores/runs";
import { Button, Dialog, Field, Input, Select, Switch, Textarea } from "./ui";

/** `ED25519 · passphrase · unlocked`, as chips. */
export function KeyFacts({ k, className }: { k: SshKey; className?: string }) {
  return (
    <span className={cn("flex flex-wrap items-center gap-1.5 text-[11px]", className)}>
      <span className="text-muted">{k.kind}</span>
      {k.encrypted ? (
        <span className="flex items-center gap-0.5 text-ok">
          <LockKeyhole size={10} /> passphrase
        </span>
      ) : (
        <span className="flex items-center gap-0.5 text-warn">
          <LockKeyholeOpen size={10} /> no passphrase
        </span>
      )}
      {k.unlocked && <span className="rounded bg-ok-soft px-1 text-ok">unlocked</span>}
    </span>
  );
}

/** Start a key task (ssh-keygen, ssh-add, an install) in a terminal tab. */
export function useKeyTask() {
  const task = useRuns((s) => s.task);
  return {
    unlock: (name: string) => task(`unlock ${name}`, (c, r) => api.keysUnlock(name, c, r)),
    passphrase: (name: string) => task(`change ${name}'s passphrase`, (c, r) => api.keysPassphrase(name, c, r)),
    install: (name: string, server: string) => task(`put ${name} on ${server}`, (c, r) => api.keysInstall(name, server, c, r)),
  };
}

/** Make a new key: ssh-keygen runs in a tab and asks for the passphrase there. */
export function NewKeyDialog({
  open,
  onOpenChange,
  defaultName,
  onStarted,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  defaultName?: string;
  onStarted?: (name: string) => void;
}) {
  const { data: info } = useKeys();
  const task = useRuns((s) => s.task);
  const [name, setName] = useState("");
  const [comment, setComment] = useState("");
  const [kind, setKind] = useState<KeyType>("ed25519");
  const [passphrase, setPassphrase] = useState(true);
  useEffect(() => {
    if (open) setName(defaultName ?? "");
  }, [open, defaultName]);
  const taken = info?.keys.some((k) => k.name === name);
  const err = name ? (taken ? `A key named ${name} is already saved` : keyNameError(name)) : null;

  const make = async () => {
    onOpenChange(false);
    const run = await task(`make ${name}`, (c, r) => api.keysGenerate(name, comment.trim() || null, kind, passphrase, c, r));
    if (run) onStarted?.(name);
  };

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title="New SSH key"
      description="ssh-keygen makes it in a terminal tab. Clibooker keeps it and syncs it to your devices."
    >
      <div className="flex flex-col gap-3">
        <div className="grid grid-cols-[1fr_150px] gap-3">
          <Field label="Name" error={err} hint="Servers use it as key:NAME">
            <Input mono autoFocus value={name} onChange={(e) => setName(e.target.value.toLowerCase())} placeholder="lab" />
          </Field>
          <Field label="Type">
            <Select value={kind} onChange={(e) => setKind(e.target.value as KeyType)}>
              <option value="ed25519">Ed25519</option>
              <option value="rsa">RSA 4096 (old servers)</option>
            </Select>
          </Field>
        </div>
        <Field label="Comment" hint="Shown next to the key in a server's authorized_keys">
          <Input mono value={comment} onChange={(e) => setComment(e.target.value)} placeholder="you@this-device" />
        </Field>
        <Switch
          checked={passphrase}
          onChange={setPassphrase}
          label="Protect it with a passphrase"
          hint={
            passphrase
              ? "You type it in the terminal; once unlocked, it's remembered"
              : "Anyone with the file can use it (synced, it's protected by your sync passphrase alone)"
          }
        />
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!name || !!err} onClick={() => void make()}>
            <KeyRound size={13} /> Make key
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/** Save an existing private key in Clibooker: from a file (its .pub is read too) or pasted. */
export function ImportKeyDialog({
  open,
  onOpenChange,
  defaultPath,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  defaultPath?: string;
  onSaved?: (k: SshKey) => void;
}) {
  const { data: lib } = useLibrary();
  const { data: info } = useKeys();
  const qc = useQueryClient();
  const [paste, setPaste] = useState(false);
  const [path, setPath] = useState("");
  const [text, setText] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!open) return;
    setPaste(false);
    setText("");
    setPath(defaultPath ?? "");
    setName("");
    if (defaultPath) void api.keysSuggestName(defaultPath).then(setName);
  }, [open, defaultPath]);
  const err = name ? keyNameError(name) : null;

  const choose = async () => {
    const f = await pickFile(lib?.home ? joinPath(lib.home, ".ssh") : undefined);
    if (!f) return;
    setPath(contractHome(f, lib?.home));
    setName(await api.keysSuggestName(f));
  };

  const save = async () => {
    setBusy(true);
    try {
      const k = await api.keysImport(name, paste ? { text } : { path });
      await qc.invalidateQueries({ queryKey: ["keys"] });
      onOpenChange(false);
      toast.success(`Saved key ${k.name}`, {
        description: k.encrypted
          ? undefined
          : info?.synced
            ? "It has no passphrase, so your sync passphrase alone protects it. Add one in Settings → SSH keys."
            : "It has no passphrase: anyone with the file can use it.",
      });
      onSaved?.({ ...k, unlocked: false, used_by: [], clash: false, file: "" });
    } catch (e) {
      toast.error("Couldn't save that key", { description: String(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title="Import an SSH key"
      description="A private key, like ~/.ssh/id_ed25519. Clibooker keeps a copy and syncs it to your devices; the original stays where it is."
    >
      <div className="flex flex-col gap-3">
        <div className="flex gap-1 text-xs">
          {[
            [false, "From a file"],
            [true, "Paste it"],
          ].map(([p, label]) => (
            <button
              key={String(p)}
              onClick={() => setPaste(p as boolean)}
              className={cn("rounded-md px-2.5 py-1", paste === p ? "bg-accent-soft font-medium text-accent" : "text-muted hover:bg-hover")}
            >
              {label}
            </button>
          ))}
        </div>
        {paste ? (
          <Field label="Private key">
            <Textarea
              mono
              rows={6}
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder={"-----BEGIN OPENSSH PRIVATE KEY-----\n…"}
              className="text-[11px]"
            />
          </Field>
        ) : (
          <Field label="Private key file" hint="Its .pub file is read too, when it's next to it">
            <div className="flex gap-2">
              <Input mono value={path} onChange={(e) => setPath(e.target.value)} onBlur={() => path && !name && void api.keysSuggestName(path).then(setName)} placeholder="~/.ssh/id_ed25519" />
              <Button onClick={() => void choose()}>
                <FolderOpen size={13} /> Choose…
              </Button>
            </div>
          </Field>
        )}
        <Field label="Name" error={err} hint="Servers use it as key:NAME">
          <Input mono value={name} onChange={(e) => setName(e.target.value.toLowerCase())} placeholder="lab" />
        </Field>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button variant="primary" disabled={busy || !name || !!err || !(paste ? text.trim() : path.trim())} onClick={() => void save()}>
            <Upload size={13} /> Save key
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/** Put a saved key on a server: ssh logs in the way it does now, and the server uses the key from then on. */
export function InstallKeyDialog({
  server,
  defaultKey,
  open,
  onOpenChange,
}: {
  server: string;
  defaultKey?: string | null;
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const { data: info } = useKeys();
  const { install } = useKeyTask();
  const [key, setKey] = useState("");
  const [making, setMaking] = useState(false);
  const keys = info?.keys ?? [];
  useEffect(() => {
    if (open) setKey(defaultKey && keys.some((k) => k.name === defaultKey) ? defaultKey : (keys[0]?.name ?? ""));
  }, [open, defaultKey, keys.length]);

  return (
    <>
      <Dialog
        open={open && !making}
        onOpenChange={onOpenChange}
        title={`Set up a key for ${server}`}
        description={
          <>
            Clibooker logs in to {server} the way you do now (type its password in the terminal) and adds the key to{" "}
            <code className="font-mono">~/.ssh/authorized_keys</code> there. From then on {server} logs in with the key.
          </>
        }
      >
        <div className="flex flex-col gap-3">
          {keys.length ? (
            <Field label="Key">
              <Select value={key} onChange={(e) => setKey(e.target.value)}>
                {keys.map((k) => (
                  <option key={k.name} value={k.name}>
                    {k.name} · {k.kind}
                    {k.encrypted ? "" : " · no passphrase"}
                  </option>
                ))}
              </Select>
            </Field>
          ) : (
            <p className="text-[13px] text-muted">No saved keys yet: make one first. Then come back here to put it on {server}.</p>
          )}
          <div className="flex justify-between gap-2">
            <Button variant="ghost" onClick={() => setMaking(true)}>
              <KeyRound size={13} /> New key…
            </Button>
            <div className="flex gap-2">
              <Button variant="ghost" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                disabled={!key}
                onClick={() => {
                  onOpenChange(false);
                  void install(key, server);
                }}
              >
                Put it on {server}
              </Button>
            </div>
          </div>
        </div>
      </Dialog>
      <NewKeyDialog
        open={open && making}
        onOpenChange={(o) => {
          if (!o) setMaking(false);
        }}
        defaultName={toKeyName(server)}
        onStarted={() => {
          setMaking(false);
          onOpenChange(false);
        }}
      />
    </>
  );
}

/**
 * A server's private key: ssh-agent & default keys, a saved key (synced), or a key file on
 * this device. `value` is the server's `identity`.
 */
export function KeyPicker({
  value,
  onChange,
  server,
  canInstall,
}: {
  value: string;
  onChange: (identity: string) => void;
  /** The server's name, for new keys and installing. */
  server: string;
  /** The server is saved as it is (installing changes the saved server). */
  canInstall: boolean;
}) {
  const { data: lib } = useLibrary();
  const { data: info } = useKeys();
  const { unlock } = useKeyTask();
  const [fileMode, setFileMode] = useState(false);
  const [newOpen, setNewOpen] = useState(false);
  const [installOpen, setInstallOpen] = useState(false);
  const saved = savedKeyName(value);
  const keys = info?.keys ?? [];
  const key = keys.find((k) => k.name === saved);
  const mode = saved ? value : value || fileMode ? "file" : "";

  const pick = (v: string) => {
    if (v === "new") return setNewOpen(true);
    setFileMode(v === "file");
    if (v === "file") onChange(saved ? "" : value);
    else onChange(v);
  };

  const keepFile = async () => {
    try {
      const name = await api.keysSuggestName(value);
      const k = await api.keysImport(name, { path: value });
      onChange(`key:${k.name}`);
      toast.success(`Saved key ${k.name}`, { description: "Save the server to use it; it syncs to your devices." });
    } catch (e) {
      // The same key may be saved already: use it.
      const same = /already saved as `([^`]+)`/.exec(String(e))?.[1];
      if (same) {
        onChange(`key:${same}`);
        toast.success(`Using saved key ${same}`);
      } else {
        toast.error("Couldn't save that key", { description: String(e) });
      }
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <Select value={mode} onChange={(e) => pick(e.target.value)} aria-label="Private key">
        <option value="">ssh-agent and default keys</option>
        {keys.length > 0 && (
          <optgroup label="Saved in Clibooker">
            {keys.map((k) => (
              <option key={k.name} value={`key:${k.name}`}>
                {k.name} · {k.kind}
              </option>
            ))}
          </optgroup>
        )}
        {saved && !key && <option value={value}>{saved} (not on this device)</option>}
        <option value="file">A key file on this device…</option>
        <option value="new">New key…</option>
      </Select>

      {mode === "file" && (
        <div className="flex gap-2">
          <Input mono value={value} onChange={(e) => onChange(e.target.value)} placeholder="~/.ssh/id_ed25519" />
          <Button
            onClick={async () => {
              const f = await pickFile(lib?.home ? joinPath(lib.home, ".ssh") : undefined);
              if (f) onChange(contractHome(f, lib?.home));
            }}
          >
            <FolderOpen size={13} /> Choose…
          </Button>
          {value && (
            <Button onClick={() => void keepFile()} title="Keep a copy in Clibooker, synced to your other devices">
              <KeyRound size={13} /> Save in Clibooker
            </Button>
          )}
        </div>
      )}

      {key && (
        <div className="flex items-center gap-2 rounded-md border border-line px-3 py-1.5">
          <KeyRound size={13} className="shrink-0 text-muted" />
          <div className="flex min-w-0 flex-1 flex-col">
            <KeyFacts k={key} />
            <span className="selectable truncate font-mono text-[10.5px] text-faint">{key.fingerprint}</span>
          </div>
          {key.encrypted && !key.unlocked && !info?.no_agent && (
            <Button size="sm" variant="ghost" onClick={() => void unlock(key.name)}>
              <LockKeyholeOpen size={12} /> Unlock
            </Button>
          )}
          <Button
            size="sm"
            disabled={!canInstall}
            title={canInstall ? undefined : "Save the server first"}
            onClick={() => setInstallOpen(true)}
          >
            Put it on {server || "the server"}…
          </Button>
        </div>
      )}
      {saved && !key && (
        <p className="flex items-center gap-1.5 text-xs text-warn">
          <TriangleAlert size={12} className="shrink-0" />
          Not on this device yet: it comes with sync, or import it as {saved}. Until then ssh tries your agent and default keys.
        </p>
      )}

      <NewKeyDialog open={newOpen} onOpenChange={setNewOpen} defaultName={toKeyName(server || "server")} onStarted={(n) => onChange(`key:${n}`)} />
      {installOpen && <InstallKeyDialog server={server} defaultKey={saved} open={installOpen} onOpenChange={setInstallOpen} />}
    </div>
  );
}
