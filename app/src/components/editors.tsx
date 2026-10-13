import { ArrowDown, ArrowUp, Plus, X } from "lucide-react";
import { useState } from "react";
import { cn } from "../lib/utils";
import { Input, Textarea } from "./ui";

let keySeq = 0;
export const newKey = () => `k${++keySeq}`;

export interface StepDraft {
  key: string;
  run: string;
  name: string;
  cwd: string;
  continue_on_error: boolean;
}

export function StepsEditor({ steps, onChange }: { steps: StepDraft[]; onChange: (s: StepDraft[]) => void }) {
  const update = (i: number, patch: Partial<StepDraft>) => onChange(steps.map((s, j) => (j === i ? { ...s, ...patch } : s)));
  const move = (i: number, d: -1 | 1) => {
    const next = [...steps];
    [next[i], next[i + d]] = [next[i + d], next[i]];
    onChange(next);
  };
  return (
    <div className="flex flex-col gap-2">
      {steps.map((s, i) => (
        <div key={s.key} className="group rounded-lg border border-line bg-raised p-2.5">
          <div className="mb-2 flex items-center gap-2">
            <span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-accent-soft text-[11px] font-semibold text-accent tabular-nums">
              {i + 1}
            </span>
            <input
              value={s.name}
              onChange={(e) => update(i, { name: e.target.value })}
              placeholder="Step name (optional)"
              className="min-w-0 flex-1 bg-transparent text-[13px] font-medium outline-none placeholder:font-normal placeholder:text-faint"
            />
            <div className="flex items-center opacity-60 transition-opacity group-hover:opacity-100">
              <button
                aria-label="Move up"
                disabled={i === 0}
                onClick={() => move(i, -1)}
                className="flex size-6 items-center justify-center rounded text-muted hover:bg-hover disabled:opacity-30"
              >
                <ArrowUp size={13} />
              </button>
              <button
                aria-label="Move down"
                disabled={i === steps.length - 1}
                onClick={() => move(i, 1)}
                className="flex size-6 items-center justify-center rounded text-muted hover:bg-hover disabled:opacity-30"
              >
                <ArrowDown size={13} />
              </button>
              <button
                aria-label="Remove step"
                onClick={() => onChange(steps.filter((_, j) => j !== i))}
                className="flex size-6 items-center justify-center rounded text-muted hover:bg-err-soft hover:text-err"
              >
                <X size={13} />
              </button>
            </div>
          </div>
          <Textarea
            mono
            rows={Math.min(6, Math.max(1, s.run.split("\n").length))}
            value={s.run}
            onChange={(e) => update(i, { run: e.target.value })}
            placeholder="command to run"
          />
          <div className="mt-2 flex items-center gap-3">
            <Input
              mono
              className="h-6 max-w-[220px] text-[11.5px]"
              value={s.cwd}
              onChange={(e) => update(i, { cwd: e.target.value })}
              placeholder="in subfolder (optional)"
            />
            <label className="flex items-center gap-1.5 text-xs text-muted">
              <input
                type="checkbox"
                checked={s.continue_on_error}
                onChange={(e) => update(i, { continue_on_error: e.target.checked })}
                className="accent-[var(--c-accent)]"
              />
              Continue if it fails
            </label>
          </div>
        </div>
      ))}
      <button
        onClick={() => onChange([...steps, { key: newKey(), run: "", name: "", cwd: "", continue_on_error: false }])}
        className="flex h-8 items-center justify-center gap-1.5 rounded-lg border border-dashed border-line-strong text-[13px] text-muted transition-colors hover:border-accent hover:text-accent"
      >
        <Plus size={14} /> Add step
      </button>
    </div>
  );
}

export interface VarDraft {
  key: string;
  k: string;
  v: string;
}

export function KeyValueEditor({
  rows,
  onChange,
  addLabel = "Add variable",
  keyPlaceholder = "NAME",
  valuePlaceholder = "value or ${OTHER}",
}: {
  rows: VarDraft[];
  onChange: (r: VarDraft[]) => void;
  addLabel?: string;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}) {
  const update = (i: number, patch: Partial<VarDraft>) => onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="flex flex-col gap-1.5">
      {rows.map((r, i) => (
        <div key={r.key} className="flex items-center gap-1.5">
          <Input
            mono
            className="w-[40%]"
            value={r.k}
            onChange={(e) => update(i, { k: e.target.value.replace(/[^A-Za-z0-9_]/g, "") })}
            placeholder={keyPlaceholder}
          />
          <span className="text-faint">=</span>
          <Input mono className="flex-1" value={r.v} onChange={(e) => update(i, { v: e.target.value })} placeholder={valuePlaceholder} />
          <button
            aria-label="Remove variable"
            onClick={() => onChange(rows.filter((_, j) => j !== i))}
            className="flex size-6 shrink-0 items-center justify-center rounded text-muted hover:bg-err-soft hover:text-err"
          >
            <X size={13} />
          </button>
        </div>
      ))}
      <button
        onClick={() => onChange([...rows, { key: newKey(), k: "", v: "" }])}
        className="flex h-6 w-fit items-center gap-1 rounded px-1.5 text-xs font-medium text-accent hover:bg-accent-soft"
      >
        <Plus size={12} /> {addLabel}
      </button>
    </div>
  );
}

export function TagInput({
  tags,
  onChange,
  suggestions,
  prefix = "#",
  placeholder = "Add tags…",
  mono,
}: {
  tags: string[];
  onChange: (t: string[]) => void;
  suggestions: string[];
  prefix?: string;
  placeholder?: string;
  mono?: boolean;
}) {
  const [text, setText] = useState("");
  const listId = `suggest-${placeholder.replace(/\W/g, "")}`;
  const add = (raw: string) => {
    const t = raw.trim().replace(/^#/, "");
    if (t && !tags.includes(t)) onChange([...tags, t]);
    setText("");
  };
  return (
    <div
      className={cn(
        "flex min-h-7 flex-wrap items-center gap-1 rounded-md border border-line-strong bg-raised px-1.5 py-1",
        "focus-within:border-accent focus-within:ring-3 focus-within:ring-accent-soft",
      )}
    >
      {tags.map((t) => (
        <span key={t} className={cn("inline-flex h-5 items-center gap-0.5 rounded bg-hover pr-0.5 pl-1.5 text-xs", mono && "font-mono")}>
          {prefix}
          {t}
          <button
            aria-label={`Remove tag ${t}`}
            onClick={() => onChange(tags.filter((x) => x !== t))}
            className="flex size-4 items-center justify-center rounded text-faint hover:text-fg"
          >
            <X size={11} />
          </button>
        </span>
      ))}
      <input
        list={listId}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === "," || (e.key === " " && prefix === "#") || (e.key === ":" && prefix !== "#")) {
            e.preventDefault();
            add(text);
          } else if (e.key === "Backspace" && !text && tags.length) {
            onChange(tags.slice(0, -1));
          }
        }}
        onBlur={() => text && add(text)}
        placeholder={tags.length ? "" : placeholder}
        className={cn("h-5 min-w-[80px] flex-1 bg-transparent text-[13px] outline-none placeholder:text-faint", mono && "font-mono text-[12px]")}
      />
      <datalist id={listId}>
        {suggestions.filter((s) => !tags.includes(s)).map((s) => <option key={s} value={s} />)}
      </datalist>
    </div>
  );
}
