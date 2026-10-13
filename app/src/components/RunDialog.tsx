import { useQueryClient } from "@tanstack/react-query";
import { Play, ShieldAlert } from "lucide-react";
import { toast } from "sonner";
import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { keys, useEntry, useLibrary } from "../lib/queries";
import type { PlanSummary } from "../lib/types";
import { useRuns } from "../stores/runs";
import { useUi } from "../stores/ui";
import { CommandText, EnvBadge } from "./bits";
import { Button, Dialog, Field, Input } from "./ui";

/** Collects params (and confirmation) before starting a run. */
export function RunDialog() {
  const { runPrompt, setRunPrompt } = useUi();
  const entry = useEntry(runPrompt?.entryId ?? null);
  const { data: lib } = useLibrary();
  const start = useRuns((s) => s.start);
  const qc = useQueryClient();
  const [values, setValues] = useState<Record<string, string>>({});
  const [plan, setPlan] = useState<PlanSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => setValues(runPrompt?.params ?? {}), [runPrompt]);

  // Resolve the plan with the current values so the user sees exactly what runs.
  useEffect(() => {
    if (!entry || !lib) return;
    let alive = true;
    const t = setTimeout(() => {
      const file = entry.file === lib.global_file ? null : entry.file;
      api
        .preview(file, entry.name, entry.bookmark, values)
        .then((p) => {
          if (!alive) return;
          setPlan(p.plan);
          setError(p.error);
        })
        .catch((e) => alive && setError(String(e)));
    }, 150);
    return () => {
      alive = false;
      clearTimeout(t);
    };
  }, [entry, lib, values]);

  if (!runPrompt || !entry) return null;
  const close = () => setRunPrompt(null);
  const missing = entry.params.filter((p) => p.default === null && !values[p.name]?.length);

  const go = async () => {
    if (missing.length) return;
    if (!entry.trusted) {
      try {
        await api.trustProject(entry.file);
        await qc.invalidateQueries({ queryKey: keys.library });
      } catch (e) {
        toast.error("Could not trust the project file", { description: String(e) });
        return;
      }
    }
    close();
    void start(entry.id, values);
  };

  return (
    <Dialog
      open
      onOpenChange={(o) => !o && close()}
      title={
        <span className="flex items-center gap-2">
          Run {entry.name} <EnvBadge env={entry.bookmark.env} />
        </span>
      }
      description={entry.bookmark.description}
      className="w-[min(600px,calc(100vw-32px))]"
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void go();
        }}
        className="flex flex-col gap-4"
      >
        {!entry.trusted && (
          <div className="flex gap-2.5 rounded-lg border border-warn/30 bg-warn/10 p-3 text-xs leading-relaxed">
            <ShieldAlert size={16} className="mt-px shrink-0 text-warn" />
            <p>
              This bookmark comes from <code className="selectable font-mono">{entry.file.replace(lib?.home ?? "~", "~")}</code>, which is
              new or has changed since you last approved it. Project files can run any command, so check what runs below before
              trusting it.
            </p>
          </div>
        )}
        {entry.params.length > 0 && (
          <div className="grid grid-cols-2 gap-3">
            {entry.params.map((p, i) => (
              <Field key={p.name} label={<span className="font-mono">{p.name}</span>} hint={p.default !== null ? `default: ${p.default || '""'}` : "required"}>
                <Input
                  mono
                  autoFocus={i === 0}
                  value={values[p.name] ?? ""}
                  onChange={(e) => setValues({ ...values, [p.name]: e.target.value })}
                  placeholder={p.default ?? ""}
                />
              </Field>
            ))}
          </div>
        )}

        <div className="max-h-[220px] overflow-y-auto rounded-lg border border-line bg-panel p-3 text-xs">
          {error ? (
            <p className="selectable text-err">{error}</p>
          ) : plan ? (
            <div className="flex flex-col gap-1.5">
              <div className="text-muted">
                in <span className="font-mono text-fg">{plan.cwd}</span> · {plan.env_description}
              </div>
              {plan.steps.map((s, i) => (
                <div key={i} className="flex gap-2">
                  <span className="shrink-0 text-faint">{plan.steps.length > 1 ? `${i + 1}.` : "$"}</span>
                  <CommandText text={s.command} className="selectable text-[11.5px] break-all whitespace-pre-wrap" />
                </div>
              ))}
            </div>
          ) : (
            <p className="text-faint">Resolving…</p>
          )}
        </div>

        <div className="flex items-center justify-end gap-2">
          <Button type="button" variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button type="submit" variant="run" disabled={missing.length > 0 || !!error} autoFocus={!entry.params.length}>
            <Play size={12} fill="currentColor" /> {entry.trusted ? "Run" : "Trust & run"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
