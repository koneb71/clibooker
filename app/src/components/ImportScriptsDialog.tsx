import { useQueryClient } from "@tanstack/react-query";
import { FolderOpen } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { api, pickFolder } from "../lib/api";
import { keys, useLibrary } from "../lib/queries";
import type { Bookmark, ScriptScan } from "../lib/types";
import { cn, nameError } from "../lib/utils";
import { useUi } from "../stores/ui";
import { EnvBadge } from "./bits";
import { RuntimeBadges } from "./RuntimesEditor";
import { Badge, Button, Dialog, Field, Input, Select } from "./ui";

const NEW_PROJECT = "__new_project__";

/** Turns a project's package.json / Makefile / justfile / ... scripts into bookmarks. */
export function ImportScriptsDialog() {
  const { importDir, importServer, setImportDir, setFilter } = useUi();
  const { data: lib } = useLibrary();
  const qc = useQueryClient();
  const [scan, setScan] = useState<ScriptScan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const [target, setTarget] = useState("");
  const [prefix, setPrefix] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!importDir) return;
    setScan(null);
    setError(null);
    api
      .scanScripts(importDir, importServer)
      .then((r) => {
        setScan(r);
        setPicked(new Set(r.scripts.map((_, i) => i)));
        const registered = lib?.projects.find((p) => p.file === r.project_file || p.dir === r.dir);
        setTarget(registered?.file ?? r.project_file ?? "");
        setPrefix(r.name.replace(/[^A-Za-z0-9_-]+/g, "-").toLowerCase());
      })
      .catch((e) => setError(String(e)));
    // lib only seeds the default target; don't rescan when it changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [importDir, importServer]);

  const isGlobal = target === "";
  const nameFor = (suggested: string) => (isGlobal && prefix ? `${prefix}-${suggested}` : suggested);
  const existing = useMemo(() => {
    const file = isGlobal ? lib?.global_file : target;
    return new Set((lib?.entries ?? []).filter((e) => e.file === file).map((e) => e.name));
  }, [lib, target, isGlobal]);

  if (!importDir) return null;
  const close = () => setImportDir(null);

  const run = async () => {
    if (!scan) return;
    setBusy(true);
    try {
      let file: string | null = isGlobal ? null : target;
      if (target === NEW_PROJECT) file = await api.addProject(scan.dir);
      let done = 0;
      const failed: string[] = [];
      for (const i of picked) {
        const s = scan.scripts[i];
        const name = nameFor(s.suggested_name);
        const b: Bookmark = {
          description: s.detail ? `${s.source} · ${s.detail}` : s.source,
          cwd: file ? "." : scan.dir,
          command: s.command,
        };
        if (scan.server) b.server = scan.server;
        if (scan.env.type !== "inherit") b.env = scan.env;
        if (Object.keys(scan.runtimes).length) b.runtimes = scan.runtimes;
        if (isGlobal && prefix) b.tags = [prefix];
        try {
          await api.saveBookmark(file, existing.has(name) ? name : null, name, b);
          done++;
        } catch (e) {
          failed.push(`${name}: ${e}`);
        }
      }
      await qc.invalidateQueries({ queryKey: keys.library });
      if (failed.length) toast.error(`${failed.length} not imported`, { description: failed.join("\n") });
      if (done) {
        toast.success(`Bookmarked ${done} script${done === 1 ? "" : "s"}`);
        setFilter(file ? { kind: "project", file } : prefix ? { kind: "tag", tag: prefix } : { kind: "all" });
      }
      close();
    } finally {
      setBusy(false);
    }
  };

  const toggle = (i: number) => {
    const next = new Set(picked);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    setPicked(next);
  };
  const badNames = scan?.scripts.filter((s, i) => picked.has(i) && nameError(nameFor(s.suggested_name))) ?? [];

  return (
    <Dialog
      open
      onOpenChange={(o) => !o && close()}
      title="Bookmark project scripts"
      description={
        <span className="flex items-center gap-1.5">
          <span className="selectable font-mono text-[12px]">
            {importServer ? `${importServer}:` : ""}
            {scan?.dir ?? importDir}
          </span>
          {!importServer && <button
            onClick={async () => {
              const d = await pickFolder(scan?.dir);
              if (d) setImportDir(d);
            }}
            className="inline-flex items-center gap-1 rounded px-1 text-xs font-medium text-accent hover:bg-accent-soft"
          >
            <FolderOpen size={12} /> Change
          </button>}
        </span>
      }
      className="w-[min(680px,calc(100vw-32px))]"
    >
      {error && <p className="selectable text-[13px] text-err">{error}</p>}
      {!scan && !error && <p className="text-[13px] text-muted">Looking for scripts…</p>}
      {scan && scan.scripts.length === 0 && (
        <p className="text-[13px] leading-relaxed text-muted">
          No scripts here. Clibooker reads package.json (npm, pnpm, yarn, bun), deno.json, composer.json, pyproject.toml,
          Makefile, justfile, Taskfile.yml and Procfile.
        </p>
      )}
      {scan && scan.scripts.length > 0 && (
        <div className="flex flex-col gap-4">
          <div className="grid grid-cols-2 gap-3">
            <Field label="Save to">
              <Select value={target} disabled={Boolean(scan.server)} onChange={(e) => setTarget(e.target.value)}>
                <option value="">Global bookmarks</option>
                {lib?.projects.map((p) => (
                  <option key={p.file} value={p.file}>
                    Project: {p.name}
                  </option>
                ))}
                {!scan.server && !scan.project_file && !lib?.projects.some((p) => p.dir === scan.dir) && (
                  <option value={NEW_PROJECT}>New .bk.toml in {scan.name}</option>
                )}
              </Select>
            </Field>
            {isGlobal && (
              <Field label="Name prefix">
                <Input mono value={prefix} onChange={(e) => setPrefix(e.target.value.replace(/[^A-Za-z0-9_-]/g, ""))} />
              </Field>
            )}
          </div>

          <div className="flex items-center gap-2 text-xs text-muted">
            <span>Runs with</span>
            {scan.env.type === "inherit" && !Object.keys(scan.runtimes).length && <span>your default environment</span>}
            <EnvBadge env={scan.env} />
            <RuntimeBadges runtimes={scan.runtimes} max={4} />
            <button
              className="ml-auto font-medium text-accent hover:underline"
              onClick={() => setPicked(picked.size === scan.scripts.length ? new Set() : new Set(scan.scripts.map((_, i) => i)))}
            >
              {picked.size === scan.scripts.length ? "Select none" : "Select all"}
            </button>
          </div>

          <div className="scroll-thin max-h-[300px] overflow-y-auto rounded-lg border border-line">
            {scan.scripts.map((s, i) => {
              const name = nameFor(s.suggested_name);
              return (
                <label
                  key={`${s.source}-${s.name}`}
                  className={cn("flex cursor-default items-start gap-3 border-b border-line px-3 py-2 last:border-b-0 hover:bg-hover", !picked.has(i) && "opacity-60")}
                >
                  <input type="checkbox" checked={picked.has(i)} onChange={() => toggle(i)} className="mt-1 accent-[var(--c-accent)]" />
                  <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <div className="flex items-center gap-2">
                      <span className="font-mono text-[12.5px] font-semibold">{name}</span>
                      {existing.has(name) && <Badge tone="accent">replaces existing</Badge>}
                      <span className="ml-auto shrink-0 text-[11px] text-faint">{s.source}</span>
                    </div>
                    <span className="truncate font-mono text-[11.5px] text-accent">{s.command}</span>
                    {s.detail && <span className="truncate font-mono text-[11px] text-faint">{s.detail}</span>}
                  </div>
                </label>
              );
            })}
          </div>
          {badNames.length > 0 && <p className="text-xs text-err">Some names are invalid; adjust the prefix.</p>}
        </div>
      )}
      <div className="mt-4 flex justify-end gap-2">
        <Button variant="ghost" onClick={close}>
          Cancel
        </Button>
        <Button variant="primary" disabled={!scan || picked.size === 0 || busy || badNames.length > 0} onClick={() => void run()}>
          {busy ? "Importing…" : `Bookmark ${picked.size || ""} script${picked.size === 1 ? "" : "s"}`}
        </Button>
      </div>
    </Dialog>
  );
}
