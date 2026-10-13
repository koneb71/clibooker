import { Plus, Sparkles, X } from "lucide-react";
import type { FolderScan, Inventory, RuntimeLang } from "../lib/types";
import { RUNTIME_LANGS } from "../lib/types";
import { cn } from "../lib/utils";
import { Input } from "./ui";

export const RUNTIME_INFO: Record<RuntimeLang, { label: string; color: string; hint: string }> = {
  node: { label: "Node.js", color: "bg-[#3c873a]", hint: "nvm · fnm · volta · mise · Homebrew; adds node_modules/.bin" },
  java: { label: "Java", color: "bg-[#e76f00]", hint: "JDKs in /Library/Java · SDKMAN · Homebrew; sets JAVA_HOME" },
  ruby: { label: "Ruby", color: "bg-[#cc342d]", hint: "rbenv · chruby · rvm · mise · Homebrew" },
  go: { label: "Go", color: "bg-[#00add8]", hint: "~/sdk · goenv · mise, or GOTOOLCHAIN for a full version" },
  rust: { label: "Rust", color: "bg-[#b7410e]", hint: "rustup toolchain (stable, nightly, 1.80…)" },
  php: { label: "PHP", color: "bg-[#777bb4]", hint: "Homebrew php@X · mise; adds vendor/bin" },
};

type Runtimes = Partial<Record<RuntimeLang, string>>;

/** Version suggestions: auto, the installed versions, and their major versions. */
function suggestions(lang: RuntimeLang, inventory?: Inventory): string[] {
  const installs = inventory?.runtimes[lang] ?? [];
  const out = ["auto", "system"];
  if (lang === "node") out.push("lts");
  if (lang === "rust") out.push("stable", "nightly", "beta");
  for (const i of installs) {
    const v = lang === "rust" ? i.version.split("-")[0] : i.version;
    const major = v.split(".")[0];
    if (lang !== "rust" && /^\d+$/.test(major) && !out.includes(major)) out.push(major);
    if (!out.includes(v)) out.push(v);
  }
  return out;
}

export function RuntimesEditor({
  value,
  onChange,
  inventory,
  scan,
}: {
  value: Runtimes;
  onChange: (v: Runtimes) => void;
  inventory?: Inventory;
  scan?: FolderScan;
}) {
  const used = Object.keys(value) as RuntimeLang[];
  const unused = RUNTIME_LANGS.filter((l) => !used.includes(l));
  const hints = (Object.entries(scan?.runtime_hints ?? {}) as [RuntimeLang, [string, string]][]).filter(
    ([lang]) => !used.includes(lang),
  );

  const set = (lang: RuntimeLang, v: string | null) => {
    const next = { ...value };
    if (v === null) delete next[lang];
    else next[lang] = v;
    onChange(next);
  };

  return (
    <div className="flex flex-col gap-2">
      {used.map((lang) => {
        const installs = inventory?.runtimes[lang] ?? [];
        return (
          <div key={lang} className="flex items-center gap-2">
            <span className="flex w-[150px] shrink-0 items-center gap-2 text-[13px]">
              <span className={cn("size-2 rounded-full", RUNTIME_INFO[lang].color)} />
              {RUNTIME_INFO[lang].label}
            </span>
            <Input
              mono
              className="min-w-0 flex-1"
              list={`rt-${lang}`}
              value={value[lang] ?? ""}
              onChange={(e) => set(lang, e.target.value)}
              placeholder="auto"
            />
            <datalist id={`rt-${lang}`}>
              {suggestions(lang, inventory).map((s) => (
                <option key={s} value={s} />
              ))}
            </datalist>
            <span className="hidden w-[150px] shrink-0 truncate text-[11px] text-faint lg:inline" title={RUNTIME_INFO[lang].hint}>
              {installs.length ? `${installs.length} installed` : "none installed"}
            </span>
            <button
              aria-label={`Remove ${lang}`}
              onClick={() => set(lang, null)}
              className="flex size-6 shrink-0 items-center justify-center rounded text-muted hover:bg-err-soft hover:text-err"
            >
              <X size={13} />
            </button>
          </div>
        );
      })}

      <div className="flex flex-wrap items-center gap-1.5">
        {unused.length > 0 && (
          <label className="relative flex h-6 items-center gap-1 rounded px-1.5 text-xs font-medium text-accent hover:bg-accent-soft">
            <Plus size={12} /> Add runtime
            <select
              aria-label="Add runtime"
              value=""
              onChange={(e) => e.target.value && set(e.target.value as RuntimeLang, "auto")}
              className="absolute inset-0 cursor-default opacity-0"
            >
              <option value="" disabled>
                Add runtime
              </option>
              {unused.map((l) => (
                <option key={l} value={l}>
                  {RUNTIME_INFO[l].label}
                </option>
              ))}
            </select>
          </label>
        )}
        {hints.map(([lang, [version, file]]) => (
          <button
            key={lang}
            onClick={() => set(lang, "auto")}
            className="inline-flex h-6 items-center gap-1 rounded px-1.5 text-xs font-medium text-accent hover:bg-accent-soft"
            title={`Follows ${file}, so the bookmark stays in sync with the project`}
          >
            <Sparkles size={12} /> Use {lang} {version} from {file}
          </button>
        ))}
      </div>
      {used.length > 0 && (
        <p className="text-xs text-faint">
          <code className="font-mono">auto</code> follows the project's version file (.nvmrc, .java-version, …); a number like{" "}
          <code className="font-mono">22</code> picks the newest installed 22.x.
        </p>
      )}
    </div>
  );
}

export function RuntimeBadges({ runtimes, max = 2 }: { runtimes?: Runtimes; max?: number }) {
  const entries = Object.entries(runtimes ?? {}) as [RuntimeLang, string][];
  if (!entries.length) return null;
  return (
    <>
      {entries.slice(0, max).map(([lang, v]) => (
        <span
          key={lang}
          className="inline-flex h-[18px] shrink-0 items-center gap-1 rounded bg-hover px-1.5 text-[11px] font-medium text-muted"
        >
          <span className={cn("size-1.5 rounded-full", RUNTIME_INFO[lang]?.color ?? "bg-faint")} />
          {lang}
          <span className="font-mono text-[10.5px]">{v}</span>
        </span>
      ))}
      {entries.length > max && <span className="text-[11px] text-faint">+{entries.length - max}</span>}
    </>
  );
}
