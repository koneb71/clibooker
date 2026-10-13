import { Command } from "cmdk";
import { ArrowLeft, Search, SquareTerminal } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { EnvBadge, StatusDot, statusOfExit } from "../components/bits";
import { RuntimeBadges } from "../components/RuntimesEditor";
import { Kbd } from "../components/ui";
import { api, copyText, inTauri, on } from "../lib/api";
import { sortEntries, useBackendInvalidation, useLibrary, useServers } from "../lib/queries";
import { prefillParams } from "../lib/run";
import type { Entry } from "../lib/types";
import { cn } from "../lib/utils";
import { useTheme } from "../stores/theme";

type Mode = "app" | "background" | "terminal" | "copy";

/** Every token must appear somewhere; name hits rank above command/folder hits. */
function paletteFilter(_value: string, search: string, keywords?: string[]): number {
  const tokens = search.toLowerCase().split(/\s+/).filter(Boolean);
  if (!tokens.length) return 1;
  const [name = "", ...rest] = (keywords ?? []).map((k) => k.toLowerCase());
  const hay = `${name} ${rest.join(" ")}`;
  if (!tokens.every((t) => hay.includes(t))) return 0;
  if (name.startsWith(tokens[0])) return 1;
  if (tokens.every((t) => name.includes(t))) return 0.8;
  return 0.4;
}

export function PaletteApp() {
  useBackendInvalidation();
  const { data: lib, refetch } = useLibrary();
  const { data: servers } = useServers();
  const [query, setQuery] = useState("");
  const [value, setValue] = useState("");
  const [paramsFor, setParamsFor] = useState<{ entry: Entry; mode: Mode } | null>(null);
  const [values, setValues] = useState<Record<string, string>>({});
  const [flash, setFlash] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (lib) useTheme.getState().setPref(lib.settings.theme ?? "system");
  }, [lib]);

  useEffect(
    () =>
      on("palette-open", () => {
        setQuery("");
        setParamsFor(null);
        setFlash(null);
        void refetch();
        requestAnimationFrame(() => inputRef.current?.focus());
      }),
    [refetch],
  );

  const entries = useMemo(() => sortEntries((lib?.entries ?? []).filter((e) => !e.shadowed)), [lib]);
  const pinned = entries.filter((e) => e.bookmark.pinned);
  const rest = entries.filter((e) => !e.bookmark.pinned);
  const byValue = (v: string) => entries.find((e) => e.id === v);

  const hide = () => {
    if (inTauri) void api.hidePalette();
  };

  const launch = async (entry: Entry, mode: Mode, params: Record<string, string>) => {
    try {
      if (mode === "app") await api.paletteRun(entry.id, params);
      else if (mode === "background") {
        await api.startRun(entry.id, params, { cols: 120, rows: 32, background: true });
        hide();
      } else if (mode === "terminal") {
        await api.openInTerminal(entry.id, params);
        hide();
      } else {
        await copyText(await api.oneLiner(entry.id, params));
        setFlash(`Copied ${entry.name} as a shell one-liner`);
        setTimeout(hide, 600);
      }
    } catch (e) {
      setFlash(String(e));
    }
  };

  const choose = (entry: Entry | undefined, mode: Mode) => {
    if (!entry) return;
    if (entry.params.length && !paramsFor) {
      setParamsFor({ entry, mode });
      setValues(prefillParams(entry));
      return;
    }
    void launch(entry, mode, paramsFor ? values : prefillParams(entry));
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      if (paramsFor) {
        setParamsFor(null);
        requestAnimationFrame(() => inputRef.current?.focus());
      } else hide();
      return;
    }
    if (paramsFor) return;
    if (e.key === "Enter" && (e.altKey || e.metaKey)) {
      e.preventDefault();
      choose(byValue(value), e.altKey ? "background" : "terminal");
    } else if (e.key === "c" && e.metaKey && !window.getSelection()?.toString()) {
      e.preventDefault();
      choose(byValue(value), "copy");
    }
  };

  const item = (e: Entry) => (
    <Command.Item
      key={e.id}
      value={e.id}
      keywords={[e.name, e.bookmark.description ?? "", e.summary, e.cwd ?? "", ...(e.bookmark.tags ?? [])]}
      onSelect={() => choose(e, "app")}
      className="group flex h-11 cursor-default items-center gap-3 rounded-lg px-3 data-[selected=true]:bg-accent-soft"
    >
      <StatusDot status={statusOfExit(e.last_run ? e.last_run.exit_code : undefined)} />
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center gap-2">
          <span className="truncate text-[13.5px] font-semibold">{e.name}</span>
          {e.params.length > 0 && <span className="text-[11px] text-faint">{e.params.length} param{e.params.length > 1 ? "s" : ""}</span>}
        </div>
        <span className="truncate font-mono text-[11px] text-muted">{e.summary}</span>
      </div>
      <span className="flex shrink-0 items-center gap-1">
        <EnvBadge env={e.bookmark.env} />
        <RuntimeBadges runtimes={e.bookmark.runtimes} max={1} />
      </span>
    </Command.Item>
  );

  const groupHeading = "[&_[cmdk-group-heading]]:px-3 [&_[cmdk-group-heading]]:pt-2 [&_[cmdk-group-heading]]:pb-1 [&_[cmdk-group-heading]]:text-[11px] [&_[cmdk-group-heading]]:font-semibold [&_[cmdk-group-heading]]:text-faint";

  return (
    <div className={cn("flex h-full items-center justify-center", !inTauri && "bg-black/30 p-10")}>
      <Command
        label="Quick run"
        value={value}
        onValueChange={setValue}
        onKeyDown={onKeyDown}
        filter={paletteFilter}
        loop
        className={cn(
          "flex h-full w-full flex-col overflow-hidden rounded-[14px] border border-line-strong text-fg",
          inTauri ? "bg-bg/75" : "h-[440px] max-w-[680px] bg-bg shadow-2xl",
        )}
      >
        {paramsFor ? (
          <ParamsForm
            entry={paramsFor.entry}
            values={values}
            setValues={setValues}
            onBack={() => setParamsFor(null)}
            onSubmit={() => void launch(paramsFor.entry, paramsFor.mode, values)}
          />
        ) : (
          <>
            <div className="flex h-14 shrink-0 items-center gap-3 border-b border-line px-4">
              <Search size={18} className="text-faint" />
              <Command.Input
                ref={inputRef}
                autoFocus
                value={query}
                onValueChange={setQuery}
                placeholder="Run a bookmark…"
                className="h-full flex-1 bg-transparent text-[17px] outline-none placeholder:text-faint focus-visible:outline-none"
              />
            </div>
            <Command.List className={cn("scroll-thin min-h-0 flex-1 overflow-y-auto p-1.5", groupHeading)}>
              <Command.Empty className="py-10 text-center text-[13px] text-muted">No bookmark matches “{query}”.</Command.Empty>
              {pinned.length > 0 && <Command.Group heading="Pinned">{pinned.map(item)}</Command.Group>}
              <Command.Group heading={pinned.length ? "Bookmarks" : undefined}>{rest.map(item)}</Command.Group>
              {servers && servers.length > 0 && (
                <Command.Group heading="Connect to server">
                  {servers.map((s) => (
                    <Command.Item
                      key={s.name}
                      value={`server:${s.name}`}
                      keywords={[s.name, "ssh", s.destination, s.description ?? ""]}
                      onSelect={() => void api.paletteConnect(s.name)}
                      className="flex h-10 cursor-default items-center gap-3 rounded-lg px-3 data-[selected=true]:bg-accent-soft"
                    >
                      <SquareTerminal size={14} className="text-muted" />
                      <span className="text-[13.5px] font-semibold">{s.name}</span>
                      <span className="truncate font-mono text-[11px] text-muted">{s.destination}</span>
                    </Command.Item>
                  ))}
                </Command.Group>
              )}
            </Command.List>
          </>
        )}
        <footer className="flex h-9 shrink-0 items-center gap-4 border-t border-line px-4 text-[11.5px] text-muted">
          {flash ? (
            <span className="truncate text-accent">{flash}</span>
          ) : (
            <>
              <span className="flex items-center gap-1.5"><Kbd>↩</Kbd> Run</span>
              <span className="flex items-center gap-1.5"><Kbd>⌥↩</Kbd> In background</span>
              <span className="flex items-center gap-1.5"><Kbd>⌘↩</Kbd> In terminal</span>
              <span className="flex items-center gap-1.5"><Kbd>⌘C</Kbd> Copy</span>
              <span className="ml-auto flex items-center gap-1.5"><Kbd>esc</Kbd></span>
            </>
          )}
        </footer>
      </Command>
    </div>
  );
}

function ParamsForm({
  entry,
  values,
  setValues,
  onBack,
  onSubmit,
}: {
  entry: Entry;
  values: Record<string, string>;
  setValues: (v: Record<string, string>) => void;
  onBack: () => void;
  onSubmit: () => void;
}) {
  const missing = entry.params.some((p) => p.default === null && !values[p.name]);
  return (
    <form
      className="flex min-h-0 flex-1 flex-col"
      onSubmit={(e) => {
        e.preventDefault();
        if (!missing) onSubmit();
      }}
    >
      <div className="flex h-14 shrink-0 items-center gap-3 border-b border-line px-4">
        <button type="button" onClick={onBack} aria-label="Back" className="flex size-7 items-center justify-center rounded-md text-muted hover:bg-hover">
          <ArrowLeft size={16} />
        </button>
        <span className="text-[15px] font-semibold">{entry.name}</span>
        <EnvBadge env={entry.bookmark.env} />
      </div>
      <div className="scroll-thin flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-5 py-4">
        {entry.params.map((p, i) => (
          <label key={p.name} className="flex items-center gap-3">
            <span className="w-[140px] shrink-0 truncate text-right font-mono text-[12.5px] text-muted">{p.name}</span>
            <input
              autoFocus={i === 0}
              value={values[p.name] ?? ""}
              onChange={(e) => setValues({ ...values, [p.name]: e.target.value })}
              placeholder={p.default ?? "required"}
              spellCheck={false}
              className="h-8 flex-1 rounded-md border border-line-strong bg-raised px-2.5 font-mono text-[13px] outline-none focus:border-accent focus:ring-3 focus:ring-accent-soft"
            />
          </label>
        ))}
        <p className="mt-1 pl-[152px] text-xs text-faint">{missing ? "Fill in the required values" : "Press ↩ to run"}</p>
      </div>
      <button type="submit" hidden />
    </form>
  );
}
