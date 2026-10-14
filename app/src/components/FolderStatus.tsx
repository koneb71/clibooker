import { useQueryClient } from "@tanstack/react-query";
import { FolderSearch, MapPin, MapPinOff } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { api, pickFolder } from "../lib/api";
import { computerNoun, thisComputer } from "../lib/platform";
import { keys } from "../lib/queries";
import type { Entry } from "../lib/types";
import { cn } from "../lib/utils";
import { Button } from "./ui";

/**
 * Say where a synced bookmark's folder is on this device. The mapping stays on this device
 * and the bookmark is untouched, so it keeps working on the others. Resolves to whether a
 * folder was located.
 */
export function useLocate(entry: Entry) {
  const qc = useQueryClient();
  return async (dir: string | null | undefined): Promise<boolean> => {
    if (!dir) return false;
    try {
      const r = await api.locateFolder(entry.id, dir);
      toast.success(`Found it: ${r.shared} is ${r.local} on this ${computerNoun}`, {
        description: r.fixed.length ? `That found ${r.fixed.join(", ")} too.` : undefined,
      });
      await qc.invalidateQueries({ queryKey: keys.library });
      return true;
    } catch (e) {
      toast.error("Couldn't use that folder", { description: String(e) });
      return false;
    }
  };
}

/** "Not on this Mac" with a one-click fix: the folder found here, or a folder picker. */
export function FolderNotHere({ entry, onLocated, className }: { entry: Entry; onLocated?: () => void; className?: string }) {
  const locate = useLocate(entry);
  const [busy, setBusy] = useState(false);
  const f = entry.folder;
  if (f?.state !== "missing") return null;
  const go = async (dir: string | null | undefined) => {
    setBusy(true);
    const ok = await locate(dir);
    setBusy(false);
    if (ok) onLocated?.();
  };
  return (
    <div className={cn("flex flex-col gap-2 rounded-lg border border-warn/30 bg-warn/10 p-3 text-xs leading-relaxed", className)}>
      <div className="flex gap-2.5">
        <MapPinOff size={15} className="mt-px shrink-0 text-warn" />
        <p>
          <code className="selectable font-mono">{f.path}</code> isn't on this {computerNoun}
          {f.foreign ? ` (it's a ${f.foreign} path)` : ""}. Say where it is here: your other devices keep their own.
        </p>
      </div>
      <div className="flex flex-wrap gap-2 pl-[25px]">
        {f.suggestion && (
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void go(f.suggestion)}>
            Use <span className="font-mono">{f.suggestion}</span>
          </Button>
        )}
        <Button size="sm" disabled={busy} onClick={async () => void go(await pickFolder())}>
          <FolderSearch size={12} /> {f.suggestion ? "Another folder…" : "Locate…"}
        </Button>
      </div>
    </div>
  );
}

/** Where the folder is found on this device, when that's through a mapping. */
export function FolderVia({ entry }: { entry: Entry }) {
  const f = entry.folder;
  if (f?.state !== "mapped" || !f.via) return null;
  return (
    <span className="flex min-w-0 items-center gap-1 text-xs text-faint">
      <MapPin size={12} className="shrink-0" />
      <span className="truncate">
        {thisComputer}: <span className="selectable font-mono text-muted">{f.path}</span> (
        <span className="font-mono">{f.via.shared}</span> is <span className="font-mono">{f.via.local}</span> here)
      </span>
    </span>
  );
}
