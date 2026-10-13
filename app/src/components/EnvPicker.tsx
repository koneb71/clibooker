import { Sparkles } from "lucide-react";
import type { EnvSpec, EnvType, FolderScan, Inventory } from "../lib/types";
import { envLabel } from "../lib/utils";
import { Input, Select, Switch } from "./ui";

const TYPES: { type: EnvType; label: string; hint: string }[] = [
  { type: "inherit", label: "Inherit", hint: "Use whatever environment Clibooker / bk runs with" },
  { type: "conda", label: "Conda env", hint: "Activates a conda environment (runs its activate.d hooks)" },
  { type: "venv", label: "Virtualenv", hint: "A venv folder, e.g. .venv" },
  { type: "uv", label: "uv project", hint: "The project's uv environment (.venv)" },
  { type: "poetry", label: "Poetry", hint: "The env `poetry env info --path` points to" },
  { type: "pyenv", label: "pyenv version", hint: "Sets PYENV_VERSION" },
  { type: "auto", label: "Auto-detect", hint: "Detect from the folder at run time" },
  { type: "mise", label: "mise", hint: "Every tool pinned in the folder's mise.toml / .tool-versions" },
  { type: "none", label: "Clean", hint: "Strip any active conda env / venv" },
];

function defaultFor(type: EnvType, inv?: Inventory, scan?: FolderScan): EnvSpec {
  switch (type) {
    case "conda":
      return { type, name: inv?.conda_envs.find((e) => e.name !== "base")?.name ?? "base" };
    case "venv":
      return { type, path: scan?.venvs[0] ?? ".venv" };
    case "uv":
      return { type };
    case "pyenv":
      return { type, version: scan?.python_version ?? inv?.pyenv_versions[0] ?? "" };
    default:
      return { type } as EnvSpec;
  }
}

export function EnvPicker({
  value,
  onChange,
  inventory,
  scan,
}: {
  value: EnvSpec;
  onChange: (v: EnvSpec) => void;
  inventory?: Inventory;
  scan?: FolderScan;
}) {
  const detected = scan?.detected;
  const showDetected = detected && envLabel(detected) !== envLabel(value);
  const condaNames = inventory?.conda_envs.map((e) => e.name) ?? [];
  const condaMissing = !!inventory && value.type === "conda" && value.name && !condaNames.includes(value.name);
  const typeInfo = TYPES.find((t) => t.type === value.type);

  return (
    <div className="flex flex-col gap-2">
      <div className="flex gap-2">
        <Select
          className="w-[150px] shrink-0"
          value={value.type}
          onChange={(e) => onChange(defaultFor(e.target.value as EnvType, inventory, scan))}
        >
          {TYPES.map((t) => (
            <option key={t.type} value={t.type}>
              {t.label}
            </option>
          ))}
        </Select>

        {value.type === "conda" && (
          <Select
            className="min-w-0 flex-1 font-mono text-[12px]"
            value={value.prefix ? `prefix:${value.prefix}` : (value.name ?? "")}
            onChange={(e) => {
              const v = e.target.value;
              onChange(v.startsWith("prefix:") ? { type: "conda", prefix: v.slice(7) } : { type: "conda", name: v });
            }}
          >
            {condaMissing && <option value={value.name}>{value.name} (not found)</option>}
            {value.prefix && <option value={`prefix:${value.prefix}`}>{value.prefix}</option>}
            {inventory?.conda_envs.map((env) => (
              <option key={env.prefix} value={env.name}>
                {env.name}
              </option>
            ))}
          </Select>
        )}

        {value.type === "venv" && (
          <>
            <Input
              mono
              className="min-w-0 flex-1"
              list="venv-suggestions"
              value={value.path}
              onChange={(e) => onChange({ type: "venv", path: e.target.value })}
              placeholder=".venv"
            />
            <datalist id="venv-suggestions">
              {scan?.venvs.map((v) => <option key={v} value={v} />)}
            </datalist>
          </>
        )}

        {value.type === "pyenv" &&
          (inventory?.pyenv_versions.length ? (
            <Select
              className="min-w-0 flex-1 font-mono text-[12px]"
              value={value.version}
              onChange={(e) => onChange({ type: "pyenv", version: e.target.value })}
            >
              {!inventory.pyenv_versions.includes(value.version) && <option value={value.version}>{value.version || "—"}</option>}
              {inventory.pyenv_versions.map((v) => (
                <option key={v}>{v}</option>
              ))}
            </Select>
          ) : (
            <Input
              mono
              className="min-w-0 flex-1"
              value={value.version}
              onChange={(e) => onChange({ type: "pyenv", version: e.target.value })}
              placeholder="3.12.4"
            />
          ))}
      </div>

      {value.type === "uv" && (
        <Switch
          checked={!!value.sync}
          onChange={(sync) => onChange({ type: "uv", sync })}
          label="Run uv sync first"
          hint="Installs/updates dependencies before every run"
        />
      )}

      <div className="flex min-h-4 items-center gap-2 text-xs">
        <span className="text-faint">{typeInfo?.hint}</span>
        {showDetected && (
          <button
            onClick={() => onChange(detected)}
            className="ml-auto inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 font-medium text-accent hover:bg-accent-soft"
          >
            <Sparkles size={12} />
            Use detected {envLabel(detected)}
          </button>
        )}
      </div>
    </div>
  );
}
