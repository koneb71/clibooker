import { useRuns } from "../stores/runs";
import { useUi } from "../stores/ui";
import type { Entry } from "./types";

/** Defaults overlaid with the values used last time. */
export function prefillParams(entry: Entry): Record<string, string> {
  const out: Record<string, string> = {};
  for (const p of entry.params) {
    const last = entry.last_run?.params?.[p.name];
    out[p.name] = last ?? p.default ?? "";
  }
  return out;
}

/**
 * Run a bookmark from the UI. Bookmarks with params or `confirm` go through the
 * run dialog first; given `params` (e.g. from the palette) skip it.
 */
export function requestRun(entry: Entry, opts?: { params?: Record<string, string>; background?: boolean }) {
  const needsPrompt = (entry.params.length > 0 && !opts?.params) || entry.bookmark.confirm || !entry.trusted;
  if (needsPrompt) {
    useUi.getState().setRunPrompt({ entryId: entry.id, params: { ...prefillParams(entry), ...opts?.params } });
    return;
  }
  void useRuns.getState().start(entry.id, opts?.params ?? prefillParams(entry), { background: opts?.background });
}
