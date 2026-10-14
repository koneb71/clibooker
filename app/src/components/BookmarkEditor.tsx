import { useQueryClient } from "@tanstack/react-query";
import {
  ChevronRight,
  Copy,
  FolderOpen,
  PackageOpen,
  Pin,
  PinOff,
  Play,
  SquareArrowOutUpRight,
  SquareTerminal,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { api, copyText, pickFolder } from "../lib/api";
import {
  contractHome,
  isAbsolutePath,
  isMod,
  kbd,
  notificationName,
  osLabel,
  platform,
  shellOptions,
  thisComputer,
  trayName,
} from "../lib/platform";
import { allTags, keys, useFolderScan, useInventory, useLibrary, useRemoteFolder, useServerInventory, useServers } from "../lib/queries";
import { folderFromRemote, inventoryFromRemote } from "../lib/remote";
import { useRuns } from "../stores/runs";
import { prefillParams, requestRun } from "../lib/run";
import { OSES, type Bookmark, type EnvSpec, type Entry, type Library, type Os, type OsVariant, type Preview, type StepTable } from "../lib/types";
import { cn, compactStep, nameError, stepsOf } from "../lib/utils";
import { useUi } from "../stores/ui";
import { CommandText } from "./bits";
import { KeyValueEditor, newKey, StepsEditor, TagInput, type StepDraft, type VarDraft } from "./editors";
import { EnvPicker } from "./EnvPicker";
import { FolderNotHere, FolderVia } from "./FolderStatus";
import { RuntimesEditor } from "./RuntimesEditor";
import { Badge, Button, Dialog, Field, IconButton, Input, Kbd, Select, Switch, Textarea } from "./ui";

interface Draft {
  name: string;
  /** Target file; null = global. */
  file: string | null;
  description: string;
  cwd: string;
  env: EnvSpec;
  runtimes: NonNullable<Bookmark["runtimes"]>;
  path: string[];
  /** "" = this device. */
  server: string;
  forward: string[];
  detach: boolean;
  mode: "command" | "steps";
  command: string;
  steps: StepDraft[];
  tags: string[];
  dotenv: string;
  vars: VarDraft[];
  shell: string;
  confirm: boolean;
  notify: boolean;
  pinned: boolean;
  /** What runs instead on one OS. */
  variants: Partial<Record<Os, VariantDraft>>;
  /** Fields this version doesn't know (from a newer Clibooker), kept as they are. */
  unknown: Record<string, unknown>;
}

/** A per-OS command; an empty command means the same one as on every other system. */
interface VariantDraft {
  mode: "command" | "steps";
  command: string;
  steps: StepDraft[];
  shell: string;
}

const KNOWN_FIELDS = new Set([
  "description", "cwd", "env", "runtimes", "path", "server", "forward", "detach", "command", "steps",
  "macos", "linux", "windows", "tags", "dotenv", "vars", "shell", "confirm", "notify", "pinned",
]);

/** Shells that only exist on one OS: picking one here only changes this OS's shell. */
const OS_ONLY_SHELLS = ["pwsh", "powershell", "cmd"];

const stepDraft = (s: StepTable): StepDraft => ({
  key: newKey(),
  run: s.run,
  name: s.name ?? "",
  cwd: s.cwd ?? "",
  continue_on_error: !!s.continue_on_error,
});

const toSteps = (steps: StepDraft[]) =>
  steps.map((s) => compactStep({ run: s.run, name: s.name, cwd: s.cwd, continue_on_error: s.continue_on_error }));

function variantFrom(v: OsVariant | undefined): VariantDraft | undefined {
  if (!v) return undefined;
  const steps = v.steps?.length ? stepsOf({ steps: v.steps }) : [];
  return { mode: steps.length ? "steps" : "command", command: v.command ?? "", steps: steps.map(stepDraft), shell: v.shell ?? "" };
}

function variantTo(v: VariantDraft): OsVariant | undefined {
  const out: OsVariant = {};
  if (v.mode === "command" && v.command.trim()) out.command = v.command;
  if (v.mode === "steps" && v.steps.some((s) => s.run.trim())) out.steps = toSteps(v.steps);
  if (v.shell) out.shell = v.shell;
  return Object.keys(out).length ? out : undefined;
}

const hasOwnCommand = (v: VariantDraft | undefined) => !!v && (v.mode === "command" ? !!v.command.trim() : v.steps.length > 0);

function fromEntry(e: Entry, lib: Library): Draft {
  const b = e.bookmark;
  const steps = stepsOf(b);
  const multi = !b.command && (b.steps?.length ?? 0) > 0;
  return {
    name: e.name,
    file: e.file === lib.global_file ? null : e.file,
    description: b.description ?? "",
    cwd: b.cwd ?? "",
    env: b.env ?? { type: "inherit" },
    runtimes: b.runtimes ?? {},
    path: b.path ?? [],
    server: b.server ?? "",
    forward: b.forward ?? [],
    detach: !!b.detach,
    mode: multi ? "steps" : "command",
    command: b.command ?? (multi ? "" : (steps[0]?.run ?? "")),
    steps: steps.map(stepDraft),
    tags: b.tags ?? [],
    dotenv: b.dotenv ?? "",
    vars: Object.entries(b.vars ?? {}).map(([k, v]) => ({ key: newKey(), k, v })),
    shell: b.shell ?? "",
    confirm: !!b.confirm,
    notify: !!b.notify,
    pinned: !!b.pinned,
    variants: Object.fromEntries(OSES.flatMap((os) => (b[os] ? [[os, variantFrom(b[os])]] : []))),
    unknown: Object.fromEntries(Object.entries(b).filter(([k]) => !KNOWN_FIELDS.has(k))),
  };
}

function emptyDraft(file: string | null, seed?: { command?: string; cwd?: string; name?: string; env?: EnvSpec } | null): Draft {
  return {
    name: seed?.name ?? "",
    file,
    description: "",
    cwd: seed?.cwd ?? (file ? "" : "~"),
    env: seed?.env ?? { type: "inherit" },
    runtimes: {},
    path: [],
    server: "",
    forward: [],
    detach: false,
    mode: "command",
    command: seed?.command ?? "",
    steps: [],
    tags: [],
    dotenv: "",
    vars: [],
    shell: "",
    confirm: false,
    notify: false,
    pinned: false,
    variants: {},
    unknown: {},
  };
}

function toBookmark(d: Draft): Bookmark {
  const b: Bookmark = { ...d.unknown };
  if (d.description.trim()) b.description = d.description.trim();
  if (d.cwd.trim()) b.cwd = d.cwd.trim();
  if (d.env.type !== "inherit") b.env = d.env;
  const runtimes = Object.fromEntries(Object.entries(d.runtimes).filter(([, v]) => v?.trim()));
  if (Object.keys(runtimes).length) b.runtimes = runtimes;
  if (d.path.length) b.path = d.path;
  if (d.server.trim()) {
    b.server = d.server.trim();
    if (d.forward.length) b.forward = d.forward;
    if (d.detach) b.detach = true;
  }
  if (d.mode === "command") b.command = d.command;
  else b.steps = toSteps(d.steps);
  // Server bookmarks run in the server's shell, whatever the OS here.
  for (const os of d.server.trim() ? [] : OSES) {
    const v = d.variants[os] && variantTo(d.variants[os]);
    if (v) b[os] = v;
  }
  if (d.tags.length) b.tags = d.tags;
  if (d.dotenv.trim()) b.dotenv = d.dotenv.trim();
  const vars = Object.fromEntries(d.vars.filter((v) => v.k).map((v) => [v.k, v.v]));
  if (Object.keys(vars).length) b.vars = vars;
  if (d.shell) b.shell = d.shell;
  if (d.confirm) b.confirm = true;
  if (d.notify) b.notify = true;
  if (d.pinned) b.pinned = true;
  return b;
}

/** Folder to scan for envs: absolute/~ as-is, relative against the project dir. */
function scanPath(d: Draft, projectDir: string | null): string | undefined {
  const c = d.cwd.trim();
  if (!c) return projectDir ?? undefined;
  if (isAbsolutePath(c)) return c;
  return projectDir ? `${projectDir}/${c}` : undefined;
}

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

function Section({ title, children, defaultOpen = true }: { title: string; children: React.ReactNode; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="border-t border-line pt-4">
      <button onClick={() => setOpen(!open)} className="mb-3 flex items-center gap-1 text-[12px] font-semibold text-muted hover:text-fg">
        <ChevronRight size={13} className={cn("transition-transform duration-150", open && "rotate-90")} />
        {title}
      </button>
      {open && <div className="flex flex-col gap-4 pb-4">{children}</div>}
    </section>
  );
}

export function BookmarkEditor({ entry }: { entry: Entry | null }) {
  const { data: lib } = useLibrary();
  const { data: inventory } = useInventory();
  const qc = useQueryClient();
  const { filter, select, draftSeed, setDirty, setImportDir } = useUi();
  const isNew = !entry;

  const initial = useMemo<Draft | null>(() => {
    if (!lib) return null;
    if (entry) return fromEntry(entry, lib);
    const file = filter.kind === "project" ? filter.file : null;
    return emptyDraft(file, draftSeed);
    // Re-init only when switching bookmarks, or when the saved bookmark changes on disk.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entry?.id, JSON.stringify(entry?.bookmark), !!lib, draftSeed]);

  const [draft, setDraft] = useState<Draft | null>(initial);
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [preview, setPreview] = useState<Preview | null>(null);
  /** The OS the command field shows ("all" = every system without its own). */
  const [osTab, setOsTab] = useState<Os | "all">("all");
  const nameRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setDraft(initial);
    setOsTab("all");
  }, [initial]);

  const dirty = useMemo(
    () => !!draft && !!initial && JSON.stringify({ ...toBookmark(draft), n: draft.name, f: draft.file }) !== JSON.stringify({ ...toBookmark(initial), n: initial.name, f: initial.file }),
    [draft, initial],
  );
  useEffect(() => setDirty(dirty), [dirty, setDirty]);
  useEffect(() => {
    if (isNew) nameRef.current?.focus();
  }, [isNew, entry?.id]);

  const projectDir = useMemo(() => {
    if (!draft?.file) return null;
    return lib?.projects.find((p) => p.file === draft.file)?.dir ?? draft.file.replace(/\/\.bk\.toml$/, "");
  }, [draft?.file, lib]);
  const remote = Boolean(draft?.server);
  const { data: servers } = useServers();
  const { data: localScan } = useFolderScan(draft && !remote ? scanPath(draft, projectDir) : undefined);
  const remoteInventory = useServerInventory(remote ? draft?.server : null);
  const remoteCwd = useDebounced(draft?.cwd, 600);
  const remoteFolder = useRemoteFolder(remote ? draft?.server : null, remoteCwd);
  const scan = remote ? folderFromRemote(remoteFolder.data) : localScan;
  const pickerInventory = remote ? inventoryFromRemote(remoteInventory.data) : inventory;
  const shellHere = useRuns((s) => s.shellHere);

  // Live preview of what would run.
  const debounced = useDebounced(draft, 300);
  useEffect(() => {
    if (!debounced) return;
    let alive = true;
    api
      .preview(debounced.file, debounced.name, toBookmark(debounced), {})
      .then((p) => alive && setPreview(p))
      .catch((e) => alive && setPreview({ one_liner: null, plan: null, error: String(e), params: [], warnings: [] }));
    return () => {
      alive = false;
    };
  }, [debounced]);

  const set = useCallback(<K extends keyof Draft>(k: K, v: Draft[K]) => setDraft((d) => (d ? { ...d, [k]: v } : d)), []);

  const nameErr = draft ? nameError(draft.name) : null;
  const commandMissing =
    !!draft && (draft.mode === "command" ? !draft.command.trim() : !draft.steps.length || draft.steps.some((s) => !s.run.trim()));
  const canSave = !!draft && !nameErr && !commandMissing && (dirty || isNew) && !saving;

  const save = useCallback(async (): Promise<string | null> => {
    if (!draft || nameErr || commandMissing) return null;
    setSaving(true);
    try {
      const id = await api.saveBookmark(draft.file, entry?.name ?? null, draft.name, toBookmark(draft));
      setDirty(false);
      await qc.invalidateQueries({ queryKey: keys.library });
      if (id !== entry?.id) select(id);
      toast.success(isNew ? `Saved ${draft.name}` : "Saved");
      return id;
    } catch (e) {
      toast.error("Could not save", { description: String(e) });
      return null;
    } finally {
      setSaving(false);
    }
  }, [draft, nameErr, commandMissing, entry, isNew, qc, select, setDirty]);

  const run = useCallback(async () => {
    if (!draft) return;
    let id = entry?.id ?? null;
    if (dirty || isNew) id = await save();
    if (!id) return;
    const fresh = await qc.fetchQuery({ queryKey: keys.library, queryFn: api.getLibrary });
    const e = fresh.entries.find((x) => x.id === id);
    if (e) requestRun(e);
  }, [draft, entry, dirty, isNew, save, qc]);

  // ⌘S save, ⌘↩ run
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!isMod(e)) return;
      if (e.key === "s") {
        e.preventDefault();
        if (canSave) void save();
      } else if (e.key === "Enter") {
        e.preventDefault();
        void run();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [canSave, save, run]);

  if (!draft || !lib) return null;

  const togglePin = async () => {
    const pinned = !draft.pinned;
    set("pinned", pinned);
    if (entry) {
      await api.setPinned(entry.id, pinned).catch((e) => toast.error(String(e)));
      qc.invalidateQueries({ queryKey: keys.library });
    }
  };

  const copyOneLiner = async () => {
    if (preview?.one_liner) {
      await copyText(preview.one_liner);
      toast.success("Copied shell one-liner");
    }
  };

  const doDelete = async () => {
    if (!entry) return;
    try {
      await api.deleteBookmark(entry.id);
      setConfirmDelete(false);
      setDirty(false);
      select(null);
      qc.invalidateQueries({ queryKey: keys.library });
      toast.success(`Deleted ${entry.name}`);
    } catch (e) {
      toast.error("Could not delete", { description: String(e) });
    }
  };

  const locationLabel = draft.file ? (lib.projects.find((p) => p.file === draft.file)?.name ?? "project") : "global";

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-bg">
      <header data-tauri-drag-region className="flex h-[52px] shrink-0 items-center gap-2 border-b border-line px-5">
        <div data-tauri-drag-region className="flex min-w-0 flex-1 items-center gap-2">
          <h2 className="truncate text-[15px] font-semibold">{isNew ? "New bookmark" : entry.name}</h2>
          <Badge tone={draft.file ? "accent" : "neutral"}>{locationLabel}</Badge>
          {dirty && !isNew && <span className="text-xs text-faint">Edited</span>}
        </div>
        <IconButton label={draft.pinned ? "Unpin" : `Pin to top & ${trayName}`} onClick={togglePin}>
          {draft.pinned ? <PinOff size={15} /> : <Pin size={15} />}
        </IconButton>
        {!isNew && remote && (
          <IconButton label={`Open a shell on ${draft.server} in this folder & env`} disabled={dirty} onClick={() => void shellHere(entry.id)}>
            <SquareTerminal size={15} />
          </IconButton>
        )}
        <IconButton label="Copy as shell one-liner" onClick={copyOneLiner} disabled={!preview?.one_liner}>
          <Copy size={15} />
        </IconButton>
        {!isNew && (
          <IconButton
            label="Open in terminal app"
            disabled={dirty}
            onClick={() =>
              void api
                .openInTerminal(entry.id, prefillParams(entry))
                .catch((e) => toast.error("Could not open terminal", { description: String(e) }))
            }
          >
            <SquareArrowOutUpRight size={15} />
          </IconButton>
        )}
        {!isNew && (
          <IconButton label="Delete" onClick={() => setConfirmDelete(true)} className="hover:text-err">
            <Trash2 size={15} />
          </IconButton>
        )}
        <Button variant="run" onClick={run} disabled={commandMissing || !!nameErr} className="ml-1">
          <Play size={12} fill="currentColor" /> Run
          <span className="ml-0.5 text-[11px] opacity-75">{kbd("Mod+Enter")}</span>
        </Button>
      </header>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-[680px] flex-col gap-5 px-6 pt-5 pb-8">
          <div className="grid grid-cols-[1fr_auto] gap-3">
            <Field label="Name" error={draft.name ? nameErr : null} hint="Run it from a terminal with bk <name>">
              <Input
                ref={nameRef}
                value={draft.name}
                onChange={(e) => set("name", e.target.value)}
                placeholder="train-sam2"
                className="font-medium"
              />
            </Field>
            <Field label="Saved in">
              <Select
                value={draft.file ?? ""}
                disabled={!isNew}
                onChange={(e) => set("file", e.target.value || null)}
                className="w-[180px]"
              >
                <option value="">Global bookmarks</option>
                {lib.projects.map((p) => (
                  <option key={p.file} value={p.file}>
                    Project: {p.name}
                  </option>
                ))}
              </Select>
            </Field>
          </div>

          <Field label="Description">
            <Input value={draft.description} onChange={(e) => set("description", e.target.value)} placeholder="What is this for?" />
          </Field>

          <Field label="Runs on" hint={remote ? "Folder, environment, runtimes and .env refer to the server." : undefined}>
            <Select
              value={draft.server}
              onChange={(e) => {
                const server = e.target.value;
                // Local folders, envs and per-OS commands don't carry over to a server (and vice versa).
                setDraft({ ...draft, server, cwd: server === draft.server ? draft.cwd : server ? "~" : "", env: { type: "inherit" }, runtimes: {}, forward: [], detach: false, variants: server ? {} : draft.variants });
                if (server) setOsTab("all");
              }}
            >
              <option value="">{thisComputer}</option>
              {servers?.map((s) => (
                <option key={s.name} value={s.name}>
                  {s.name} — {s.destination}
                </option>
              ))}
              {draft.server && !servers?.some((s) => s.name === draft.server) && <option value={draft.server}>{draft.server}</option>}
            </Select>
          </Field>

          {remote ? (
            <Field label={`Folder on ${draft.server}`} hint="Relative paths start in your home folder on the server.">
              <Input mono value={draft.cwd} onChange={(e) => set("cwd", e.target.value)} placeholder="~/project" />
              {remoteFolder.data && remoteFolder.data.scripts.length > 0 && isNew && (
                <button
                  onClick={() => setImportDir(draft.cwd || "~", draft.server)}
                  className="flex w-fit items-center gap-1 rounded px-1 text-xs font-medium text-accent hover:bg-accent-soft"
                >
                  <PackageOpen size={12} /> {remoteFolder.data.scripts.length} script{remoteFolder.data.scripts.length === 1 ? "" : "s"} in
                  this folder on {draft.server}: bookmark them all at once
                </button>
              )}
              {remoteFolder.isError && (
                <span className="flex items-center gap-1 text-xs text-warn">
                  <TriangleAlert size={12} /> Couldn't look inside it: {String(remoteFolder.error)}
                </span>
              )}
            </Field>
          ) : (
          <Field
            label="Folder"
            hint={draft.file ? "Relative paths are inside the project folder. Leave empty for the project root." : "Leave empty to run wherever bk is called."}
          >
            <div className="flex gap-2">
              <Input mono value={draft.cwd} onChange={(e) => set("cwd", e.target.value)} placeholder={draft.file ? "." : "~/work/project"} />
              <Button
                onClick={async () => {
                  const p = await pickFolder(scanPath(draft, projectDir));
                  if (!p) return;
                  set("cwd", contractHome(p, lib.home));
                }}
              >
                <FolderOpen size={13} /> Choose…
              </Button>
            </div>
            {entry?.folder && draft.cwd === (entry.bookmark.cwd ?? "") ? (
              <>
                <FolderNotHere entry={entry} />
                <FolderVia entry={entry} />
              </>
            ) : (
              scan &&
              !scan.exists &&
              draft.cwd && (
                <span className="flex items-center gap-1 text-xs text-warn">
                  <TriangleAlert size={12} /> Folder not found
                </span>
              )
            )}
            {scan && scan.script_count > 0 && isNew && (
              <button
                onClick={() => {
                  const p = scanPath(draft, projectDir);
                  if (p) setImportDir(p);
                }}
                className="flex w-fit items-center gap-1 rounded px-1 text-xs font-medium text-accent hover:bg-accent-soft"
              >
                <PackageOpen size={12} /> {scan.script_count} script{scan.script_count === 1 ? "" : "s"} in this folder
                {scan.package_manager ? ` (${scan.package_manager}, …)` : ""}: bookmark them all at once
              </button>
            )}
          </Field>
          )}

          <Field label="Environment">
            <EnvPicker value={draft.env} onChange={(v) => set("env", v)} inventory={pickerInventory} scan={scan} />
            {remote && remoteInventory.isError && (
              <span className="text-xs text-faint">
                Couldn't scan {draft.server} for envs ({String(remoteInventory.error)}); you can still type names.
              </span>
            )}
          </Field>

          <Field label="Runtimes" hint={Object.keys(draft.runtimes).length ? undefined : "Pin Node, Java, Ruby, Go, Rust or PHP versions on top of the environment."}>
            <RuntimesEditor value={draft.runtimes} onChange={(v) => set("runtimes", v)} inventory={pickerInventory} scan={scan} />
          </Field>

          {remote && (
            <>
              <Field label="Port forwarding while it runs" hint="LOCAL_PORT:HOST:PORT, e.g. 6006:127.0.0.1:6006 for TensorBoard. Links appear on the run's tab.">
                <TagInput
                  tags={draft.forward}
                  onChange={(f) => set("forward", f)}
                  suggestions={["8888:127.0.0.1:8888", "6006:127.0.0.1:6006", "8000:127.0.0.1:8000"]}
                  prefix=""
                  placeholder="6006:127.0.0.1:6006"
                  mono
                />
              </Field>
              <Switch
                checked={draft.detach}
                onChange={(v) => set("detach", v)}
                label="Keep running on the server"
                hint={
                  remoteInventory.data && !remoteInventory.data.tools.includes("tmux")
                    ? `Needs tmux, which isn't installed on ${draft.server}.`
                    : "Runs inside tmux, so it survives closing Clibooker or losing Wi-Fi. Run it again to reattach."
                }
              />
            </>
          )}

          <CommandField draft={draft} setDraft={setDraft} tab={osTab} setTab={setOsTab} userShell={lib.shell} />

          {preview && <PreviewCard preview={preview} />}

          <Section title="Options" defaultOpen={!!(draft.tags.length || draft.dotenv || draft.vars.length || draft.path.length || stepShell(draft) || draft.confirm || draft.notify)}>
            <Field label="Tags">
              <TagInput tags={draft.tags} onChange={(t) => set("tags", t)} suggestions={allTags(lib)} />
            </Field>
            <div className="grid grid-cols-2 gap-3">
              <Field label=".env file" hint="Loaded before the run; relative to the folder">
                <Input mono list="dotenv-suggestions" value={draft.dotenv} onChange={(e) => set("dotenv", e.target.value)} placeholder=".env" />
                <datalist id="dotenv-suggestions">
                  {scan?.dotenv_files.map((f) => <option key={f} value={f} />)}
                </datalist>
              </Field>
              <Field label="Shell" hint={draft.variants[platform]?.shell ? `Only on ${osLabel[platform]}` : undefined}>
                <Select value={stepShell(draft)} onChange={(e) => setDraft(withShellHere(draft, e.target.value))}>
                  {shellOptions(platform, lib.shell, { fish: platform !== "windows" }).map(([value, label]) => (
                    <option key={value} value={value}>
                      {value ? label : `Default (${lib.default_step_shell})`}
                    </option>
                  ))}
                  {stepShell(draft) && !shellOptions(platform, lib.shell, { fish: true }).some(([v]) => v === stepShell(draft)) && (
                    <option value={stepShell(draft)}>{stepShell(draft)}</option>
                  )}
                </Select>
              </Field>
            </div>
            <Field label="Extra PATH entries" hint="Searched first, relative to the folder (e.g. vendor/bin, bin, tools)">
              <TagInput tags={draft.path} onChange={(p) => set("path", p)} suggestions={["vendor/bin", "node_modules/.bin", "bin"]} prefix="" placeholder="vendor/bin" mono />
            </Field>
            <Field label="Environment variables">
              <KeyValueEditor rows={draft.vars} onChange={(v) => set("vars", v)} />
            </Field>
            <div className="flex flex-col gap-1">
              <Switch checked={draft.confirm} onChange={(v) => set("confirm", v)} label="Ask before running" hint="Shows what will run and waits for confirmation" />
              <Switch checked={draft.notify} onChange={(v) => set("notify", v)} label="Notify when finished" hint={`${notificationName}, even when the window is focused`} />
            </div>
          </Section>
        </div>
      </div>

      {(dirty || isNew) && (
        <footer className="flex h-12 shrink-0 items-center justify-end gap-2 border-t border-line bg-panel px-5">
          {commandMissing && <span className="mr-auto text-xs text-faint">Add a command to save</span>}
          {!isNew && (
            <Button variant="ghost" onClick={() => setDraft(initial)}>
              Revert
            </Button>
          )}
          {isNew && (
            <Button
              variant="ghost"
              onClick={() => {
                setDirty(false);
                select(null);
              }}
            >
              Cancel
            </Button>
          )}
          <Button variant="primary" disabled={!canSave} onClick={() => void save()}>
            {saving ? "Saving…" : "Save"} <Kbd className="border-transparent bg-black/10 text-current">{kbd("Mod+S")}</Kbd>
          </Button>
        </footer>
      )}

      <Dialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        title={`Delete ${entry?.name}?`}
        description="It's removed from its bookmarks file. Run history is kept."
      >
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setConfirmDelete(false)}>
            Cancel
          </Button>
          <Button variant="primary" className="bg-err text-white" onClick={doDelete} autoFocus>
            Delete
          </Button>
        </div>
      </Dialog>
    </div>
  );
}

/** The shell steps run in on this device: this OS's own, else the bookmark's. */
function stepShell(d: Draft): string {
  return d.variants[platform]?.shell || d.shell;
}

/**
 * Pick the shell for this device. A shell only this OS has (PowerShell, cmd), or one for a
 * command only this OS runs, is kept for this OS, so the bookmark keeps working elsewhere.
 */
function withShellHere(d: Draft, shell: string): Draft {
  const here = d.variants[platform];
  if (hasOwnCommand(here) || OS_ONLY_SHELLS.includes(shell)) {
    const v = here ?? { mode: "command", command: "", steps: [], shell: "" };
    return { ...d, variants: { ...d.variants, [platform]: { ...v, shell } } };
  }
  const variants = { ...d.variants };
  if (here) variants[platform] = { ...here, shell: "" };
  const left = variants[platform];
  if (left && !variantTo(left)) delete variants[platform];
  return { ...d, shell, variants };
}

/** The command (or workflow) for every system, and what runs instead on one OS. */
function CommandField({
  draft,
  setDraft,
  tab,
  setTab,
  userShell,
}: {
  draft: Draft;
  setDraft: (d: Draft) => void;
  tab: Os | "all";
  setTab: (t: Os | "all") => void;
  userShell: string;
}) {
  const remote = !!draft.server;
  const variant = tab === "all" ? undefined : draft.variants[tab];
  const cur: VariantDraft = variant ?? { mode: draft.mode, command: draft.command, steps: draft.steps, shell: draft.shell };
  const update = (patch: Partial<VariantDraft>) => {
    if (tab === "all") setDraft({ ...draft, ...patch });
    else setDraft({ ...draft, variants: { ...draft.variants, [tab]: { ...cur, ...patch } } });
  };
  const withVariants = OSES.filter((os) => draft.variants[os]);
  const without = OSES.filter((os) => !draft.variants[os]);

  const addVariant = (os: Os) => {
    // Start from what runs everywhere: usually a small edit away.
    const v: VariantDraft = { mode: draft.mode, command: draft.command, steps: draft.steps.map((s) => ({ ...s, key: newKey() })), shell: "" };
    setDraft({ ...draft, variants: { ...draft.variants, [os]: v } });
    setTab(os);
  };
  const removeVariant = (os: Os) => {
    const variants = { ...draft.variants };
    delete variants[os];
    setDraft({ ...draft, variants });
    setTab("all");
  };
  const setMode = (m: VariantDraft["mode"]) => {
    if (m === cur.mode) return;
    if (m === "steps") {
      update({ mode: m, steps: cur.steps.length ? cur.steps : [{ key: newKey(), run: cur.command, name: "", cwd: "", continue_on_error: false }] });
    } else {
      update({ mode: m, command: cur.command || cur.steps[0]?.run || "" });
    }
  };

  const others = withVariants.map((os) => osLabel[os]).join(" and ");
  const hint = variant ? (
    <>On {osLabel[tab as Os]}, this runs instead{hasOwnCommand(variant) ? "" : " (empty: the same command as everywhere, in this shell)"}.</>
  ) : withVariants.length ? (
    <>Runs everywhere except {others}, which {withVariants.length === 1 ? "has its own" : "have their own"}.</>
  ) : (
    <>
      Use <code className="font-mono text-muted">{"{{name}}"}</code> or <code className="font-mono text-muted">{"{{name:default}}"}</code> for values
      you fill in at run time.
    </>
  );

  return (
    <Field
      label={cur.mode === "command" ? "Command" : "Steps"}
      aside={
        <div className="flex rounded-md bg-hover p-0.5 text-xs">
          {(["command", "steps"] as const).map((m) => (
            <button
              key={m}
              onClick={() => setMode(m)}
              className={cn("rounded px-2 py-0.5 font-medium transition-colors", cur.mode === m ? "bg-raised text-fg shadow-sm" : "text-muted hover:text-fg")}
            >
              {m === "command" ? "Single command" : "Workflow"}
            </button>
          ))}
        </div>
      }
      hint={hint}
    >
      {!remote && (
        <div className="flex flex-wrap items-center gap-1 text-xs">
          {withVariants.length > 0 &&
            (["all", ...withVariants] as const).map((t) => (
              <span
                key={t}
                className={cn(
                  "flex items-center rounded-md transition-colors",
                  tab === t ? "bg-accent-soft text-accent" : "text-muted hover:bg-hover hover:text-fg",
                )}
              >
                <button onClick={() => setTab(t)} className="flex items-center gap-1.5 px-2 py-0.5 font-medium">
                  {t === "all" ? "All systems" : osLabel[t]}
                  {t === platform && <span title={`${thisComputer} runs this`} className="size-1.5 rounded-full bg-current opacity-70" />}
                </button>
                {t !== "all" && (
                  <button aria-label={`Remove the ${osLabel[t]} command`} onClick={() => removeVariant(t)} className="pr-1.5 opacity-60 hover:opacity-100">
                    <X size={11} />
                  </button>
                )}
              </span>
            ))}
          {without.length > 0 && (
            <select
              value=""
              aria-label="Use a different command on one OS"
              onChange={(e) => addVariant(e.target.value as Os)}
              className="cursor-pointer rounded-md bg-transparent px-1.5 py-0.5 text-xs text-faint outline-none hover:bg-hover hover:text-fg"
            >
              <option value="" disabled>
                {withVariants.length ? "+ Another OS" : "Different on Windows, macOS or Linux?"}
              </option>
              {without.map((os) => (
                <option key={os} value={os}>
                  A {osLabel[os]} command
                </option>
              ))}
            </select>
          )}
        </div>
      )}
      {cur.mode === "command" ? (
        <Textarea
          mono
          rows={Math.min(10, Math.max(3, cur.command.split("\n").length + 1))}
          value={cur.command}
          onChange={(e) => update({ command: e.target.value })}
          placeholder={variant ? draft.command || "Same as all systems" : "python train.py --epochs {{epochs:10}}"}
        />
      ) : (
        <StepsEditor steps={cur.steps} onChange={(s) => update({ steps: s })} />
      )}
      {variant && tab !== "all" && (
        <div className="flex items-center gap-2 text-xs">
          <span className="text-faint">Shell on {osLabel[tab]}</span>
          <Select className="w-[230px]" value={variant.shell} onChange={(e) => update({ shell: e.target.value })}>
            {shellOptions(tab, tab === platform ? userShell : "", { fish: tab !== "windows" }).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
        </div>
      )}
    </Field>
  );
}

function PreviewCard({ preview }: { preview: Preview }) {
  const plan = preview.plan;
  return (
    <div className={cn("rounded-lg border p-3 text-xs", preview.error ? "border-err/30 bg-err-soft" : "border-line bg-panel")}>
      {preview.error ? (
        <div className="flex items-start gap-2 text-err">
          <TriangleAlert size={13} className="mt-px shrink-0" />
          <span className="selectable leading-relaxed whitespace-pre-wrap">{preview.error}</span>
        </div>
      ) : plan ? (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-baseline gap-2">
            <span className="w-14 shrink-0 text-faint">runs in</span>
            <span className="selectable font-mono text-[11.5px] text-fg">{plan.cwd}</span>
          </div>
          <div className="flex items-baseline gap-2">
            <span className="w-14 shrink-0 text-faint">env</span>
            <span className="text-fg">{plan.env_description}</span>
          </div>
          {plan.env_changes.length > 0 && (
            <div className="flex items-baseline gap-2">
              <span className="w-14 shrink-0 text-faint">vars</span>
              <span className="selectable font-mono text-[11px] break-all text-muted">{plan.env_changes.join("  ")}</span>
            </div>
          )}
          {plan.steps.map((s, i) => (
            <div key={i} className="flex items-baseline gap-2">
              <span className="w-14 shrink-0 text-faint">{plan.steps.length > 1 ? `step ${i + 1}` : "command"}</span>
              <CommandText text={s.command} className="selectable text-[11.5px] break-all whitespace-pre-wrap text-fg" />
            </div>
          ))}
        </div>
      ) : null}
      {preview.warnings.length > 0 && (
        <div className="mt-2 flex flex-col gap-1 border-t border-line pt-2">
          {preview.warnings.map((w) => (
            <div key={w} className="flex items-start gap-1.5 text-warn">
              <TriangleAlert size={12} className="mt-0.5 shrink-0" />
              <span className="leading-relaxed">{w}</span>
            </div>
          ))}
        </div>
      )}
      {preview.params.length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5 border-t border-line pt-2">
          <span className="text-faint">asks for</span>
          {preview.params.map((p) => (
            <Badge key={p.name} tone="accent" className="font-mono">
              {p.name}
              {p.default !== null && <span className="opacity-60">= {p.default || '""'}</span>}
            </Badge>
          ))}
        </div>
      )}
    </div>
  );
}
