import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { type ITheme, Terminal as XTerm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef } from "react";
import { api, inTauri } from "../lib/api";
import { useRuns } from "../stores/runs";
import { type Resolved, useTheme } from "../stores/theme";

const themes: Record<Resolved, ITheme> = {
  dark: {
    background: "#141416",
    foreground: "#e4e4e7",
    cursor: "#f0b54a",
    cursorAccent: "#141416",
    selectionBackground: "rgba(240,181,74,0.28)",
    black: "#2a2a2e",
    red: "#ff6b5f",
    green: "#3ccf7f",
    yellow: "#e8b84a",
    blue: "#6ea8fe",
    magenta: "#d58cf5",
    cyan: "#5fd3e0",
    white: "#c7c7cc",
    brightBlack: "#6b6b72",
    brightRed: "#ff8a80",
    brightGreen: "#6ee7a8",
    brightYellow: "#f6cf74",
    brightBlue: "#9cc3ff",
    brightMagenta: "#e5b0fb",
    brightCyan: "#8be6f0",
    brightWhite: "#f4f4f5",
  },
  light: {
    background: "#fbfbfc",
    foreground: "#1d1d1f",
    cursor: "#b4690e",
    cursorAccent: "#fbfbfc",
    selectionBackground: "rgba(180,105,14,0.2)",
    black: "#1d1d1f",
    red: "#c4352b",
    green: "#18804a",
    yellow: "#9a6200",
    blue: "#2a5fc4",
    magenta: "#963bb0",
    cyan: "#117684",
    white: "#8e8e93",
    brightBlack: "#6e6e73",
    brightRed: "#dc4a3f",
    brightGreen: "#22a05d",
    brightYellow: "#b67700",
    brightBlue: "#3d74dc",
    brightMagenta: "#ad52c9",
    brightCyan: "#1a8d9d",
    brightWhite: "#3a3a3c",
  },
};

/** Live terminal instances, so the panel can copy output / search. */
export const terminals = new Map<string, XTerm>();

export function terminalText(runId: string): string {
  const term = terminals.get(runId);
  if (!term) return "";
  const buf = term.buffer.active;
  const lines: string[] = [];
  for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i)?.translateToString(true) ?? "");
  return lines.join("\n").replace(/\n+$/, "\n");
}

let seq = 0;

export function Terminal({ runId, visible, fontSize = 12.5 }: { runId: string; visible: boolean; fontSize?: number }) {
  const host = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const termRef = useRef<XTerm | null>(null);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const resolved = useTheme((s) => s.resolved);

  useEffect(() => {
    const el = host.current!;
    const term = new XTerm({
      fontFamily: '"SF Mono", ui-monospace, Menlo, Monaco, monospace',
      fontSize,
      lineHeight: 1.25,
      theme: themes[useTheme.getState().resolved],
      cursorBlink: true,
      scrollback: 20000,
      macOptionIsMeta: true,
      allowProposedApi: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new SearchAddon());
    term.loadAddon(
      new WebLinksAddon((_e, uri) => {
        if (inTauri) void import("@tauri-apps/plugin-opener").then((m) => m.openUrl(uri));
        else window.open(uri, "_blank");
      }),
    );
    term.open(el);
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      /* falls back to the DOM renderer */
    }
    termRef.current = term;
    fitRef.current = fit;
    terminals.set(runId, term);

    const refit = () => {
      if (!visibleRef.current || el.clientHeight < 40 || el.clientWidth < 80) return;
      try {
        fit.fit();
      } catch {
        /* not laid out yet */
      }
    };
    refit();

    const key = `panel-${++seq}`;
    void api.attachRun(runId, key, (bytes) => term.write(bytes));
    const onData = term.onData((d) => void api.writeRun(runId, d).catch(() => {}));
    const onResize = term.onResize(({ cols, rows }) => {
      void api.resizeRun(runId, cols, rows).catch(() => {});
      useRuns.getState().setTermSize(cols, rows);
    });
    if (visibleRef.current) void api.resizeRun(runId, term.cols, term.rows).catch(() => {});

    const ro = new ResizeObserver(() => requestAnimationFrame(refit));
    ro.observe(el);
    return () => {
      ro.disconnect();
      onData.dispose();
      onResize.dispose();
      void api.detachRun(runId, key).catch(() => {});
      terminals.delete(runId);
      term.dispose();
    };
    // fontSize changes are applied below without recreating the terminal.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runId]);

  useEffect(() => {
    if (termRef.current) termRef.current.options.theme = themes[resolved];
  }, [resolved]);

  useEffect(() => {
    if (!termRef.current) return;
    termRef.current.options.fontSize = fontSize;
    try {
      fitRef.current?.fit();
    } catch {
      /* ignore */
    }
  }, [fontSize]);

  useEffect(() => {
    if (!visible) return;
    requestAnimationFrame(() => {
      try {
        fitRef.current?.fit();
      } catch {
        /* ignore */
      }
      termRef.current?.focus();
    });
  }, [visible]);

  return (
    <div className="h-full w-full bg-term py-1.5 pr-1 pl-2.5">
      <div ref={host} className="h-full w-full" />
    </div>
  );
}
