import { create } from "zustand";

export type Resolved = "light" | "dark";

const media = typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null;

interface ThemeState {
  pref: "system" | "light" | "dark";
  resolved: Resolved;
  setPref: (p: ThemeState["pref"]) => void;
}

function resolve(pref: ThemeState["pref"]): Resolved {
  if (pref === "system") return media?.matches ? "dark" : "light";
  return pref;
}

function apply(r: Resolved) {
  document.documentElement.dataset.theme = r;
}

export const useTheme = create<ThemeState>((set) => ({
  pref: "system",
  resolved: resolve("system"),
  setPref: (pref) => {
    const resolved = resolve(pref);
    apply(resolved);
    set({ pref, resolved });
  },
}));

apply(useTheme.getState().resolved);
media?.addEventListener("change", () => {
  const { pref, setPref } = useTheme.getState();
  if (pref === "system") setPref("system");
});
