// Adapters so the env/runtime pickers can show what's on a server.

import type { FolderScan, Inventory, RemoteFolder, RemoteInventory, RuntimeLang } from "./types";

export function inventoryFromRemote(r?: RemoteInventory): Inventory | undefined {
  if (!r) return undefined;
  const has = (t: string) => r.tools.includes(t);
  return {
    conda_base: r.conda_base,
    conda_envs: r.conda_envs,
    pyenv_versions: (r.runtimes.python ?? []).map((i) => i.version),
    tools: { conda: Boolean(r.conda_base), uv: has("uv"), poetry: has("poetry"), pyenv: has("pyenv"), mise: has("mise") },
    runtimes: r.runtimes as Inventory["runtimes"],
  };
}

export function folderFromRemote(f?: RemoteFolder): FolderScan | undefined {
  if (!f) return undefined;
  const has = (n: string) => f.files.includes(n);
  const venvs = [".venv", "venv"].filter(has);
  const detected = has("uv.lock")
    ? ({ type: "uv" } as const)
    : venvs.length
      ? ({ type: "venv", path: venvs[0] } as const)
      : has("poetry.lock")
        ? ({ type: "poetry" } as const)
        : has("mise.toml") || has(".mise.toml")
          ? ({ type: "mise" } as const)
          : null;
  const hints: FolderScan["runtime_hints"] = {};
  for (const [lang, v] of Object.entries(f.runtime_hints)) if (v) hints[lang as RuntimeLang] = [v, "the server"];
  return {
    exists: true,
    detected,
    venvs,
    has_uv_lock: has("uv.lock"),
    has_poetry_lock: has("poetry.lock"),
    dotenv_files: f.files.filter((n) => n === ".env" || n.startsWith(".env.")),
    conda_env_file: null,
    python_version: null,
    runtime_hints: hints,
    package_manager: null,
    script_count: f.scripts.length,
  };
}
