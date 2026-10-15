import { QueryClient, useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { api, on } from "./api";
import type { Entry, Library } from "./types";

export const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: 30_000, refetchOnWindowFocus: false, retry: 1 } },
});

export const keys = {
  library: ["library"] as const,
  inventory: ["inventory"] as const,
  history: (name?: string) => ["history", name ?? "all"] as const,
  scan: (path: string) => ["scan", path] as const,
};

export function useLibrary() {
  return useQuery({ queryKey: keys.library, queryFn: api.getLibrary });
}

export function useEntry(id: string | null): Entry | undefined {
  const { data } = useLibrary();
  return id ? data?.entries.find((e) => e.id === id) : undefined;
}

export function useInventory() {
  return useQuery({ queryKey: keys.inventory, queryFn: api.getInventory, staleTime: 60_000 });
}

export function useServers() {
  return useQuery({ queryKey: ["servers"], queryFn: api.listServers, staleTime: 15_000 });
}

/** Cached discovery of a server (conda envs, runtimes, tools); doesn't connect if cached. */
export function useServerInventory(name: string | null | undefined) {
  return useQuery({
    queryKey: ["server-inventory", name],
    queryFn: () => api.serverInventory(name!),
    enabled: Boolean(name),
    staleTime: 5 * 60_000,
    retry: false,
  });
}

/** Env markers, version pins and scripts in a folder on a server. */
export function useRemoteFolder(server: string | null | undefined, dir: string | undefined) {
  return useQuery({
    queryKey: ["remote-folder", server, dir],
    queryFn: () => api.scanRemoteFolder(server!, dir || "~"),
    enabled: Boolean(server),
    staleTime: 30_000,
    retry: false,
  });
}

/** Connection status, re-checked at most once a minute. */
export function useServerStatus(name: string | null | undefined, enabled = true) {
  return useQuery({
    queryKey: ["server-status", name],
    queryFn: () => api.checkServer(name!),
    enabled: Boolean(name) && enabled,
    staleTime: 60_000,
    retry: false,
  });
}

/** Saved SSH keys, with which ones ssh-agent holds. */
export function useKeys() {
  return useQuery({ queryKey: ["keys"], queryFn: api.keysList, staleTime: 10_000 });
}

export function useHistory(name?: string, limit = 300) {
  return useQuery({ queryKey: keys.history(name), queryFn: () => api.getHistory(limit, name) });
}

export function useFolderScan(path: string | undefined) {
  return useQuery({
    queryKey: keys.scan(path ?? ""),
    queryFn: () => api.scanFolder(path!),
    enabled: Boolean(path),
    staleTime: 10_000,
  });
}

/** Keep queries fresh when files change on disk (CLI edits, other windows). */
export function useBackendInvalidation() {
  useEffect(() => {
    const offs = [
      on("library-changed", () => {
        queryClient.invalidateQueries({ queryKey: keys.library });
        queryClient.invalidateQueries({ queryKey: ["servers"] });
        // Folder mappings (folders.toml) change where folders are found.
        queryClient.invalidateQueries({ queryKey: ["folders"] });
        queryClient.invalidateQueries({ queryKey: ["scan"] });
      }),
      on("history-changed", () => {
        queryClient.invalidateQueries({ queryKey: ["history"] });
        queryClient.invalidateQueries({ queryKey: keys.library });
      }),
      on("shell-env-ready", () => {
        queryClient.invalidateQueries({ queryKey: keys.inventory });
        queryClient.invalidateQueries({ queryKey: ["scan"] });
      }),
    ];
    return () => offs.forEach((off) => off());
  }, []);
}

/** Pinned first, then most recently run, then by name. */
export function sortEntries(entries: Entry[]): Entry[] {
  return [...entries].sort((a, b) => {
    if (!!a.bookmark.pinned !== !!b.bookmark.pinned) return a.bookmark.pinned ? -1 : 1;
    const ra = a.last_run?.started_ms ?? 0;
    const rb = b.last_run?.started_ms ?? 0;
    if (ra !== rb) return rb - ra;
    return a.name.localeCompare(b.name);
  });
}

/** Every whitespace-separated token must match somewhere. */
export function matchesSearch(e: Entry, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const hay = [
    e.name,
    e.bookmark.description ?? "",
    e.summary,
    e.cwd ?? "",
    ...(e.bookmark.tags ?? []),
    e.bookmark.env?.type === "conda" ? (e.bookmark.env.name ?? "") : "",
    ...Object.entries(e.bookmark.runtimes ?? {}).map(([l, v]) => `${l} ${v}`),
  ]
    .join(" ")
    .toLowerCase();
  return q.split(/\s+/).every((t) => hay.includes(t));
}

export function allTags(lib?: Library): string[] {
  const set = new Set<string>();
  lib?.entries.forEach((e) => e.bookmark.tags?.forEach((t) => set.add(t)));
  return [...set].sort();
}
