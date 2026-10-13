import { QueryClientProvider } from "@tanstack/react-query";
import React from "react";
import ReactDOM from "react-dom/client";
import { MainApp } from "./App";
import "./index.css";
import { inTauri } from "./lib/api";
import { platform } from "./lib/platform";
import { queryClient } from "./lib/queries";
import { PaletteApp } from "./palette/Palette";

const isPalette = new URLSearchParams(window.location.search).get("window") === "palette";
if (inTauri) document.documentElement.classList.add("tauri");
document.documentElement.classList.add(`platform-${platform}`);

// The palette is a borderless window; the native context menu just gets in the way there.
if (isPalette) window.addEventListener("contextmenu", (e) => e.preventDefault());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>{isPalette ? <PaletteApp /> : <MainApp />}</QueryClientProvider>
  </React.StrictMode>,
);
