#!/usr/bin/env node
// Build the bk CLI and put it where Tauri expects the sidecar:
// src-tauri/binaries/bk-<target-triple>[.exe]. The app bundles it next to its own executable,
// and Settings → "Install bk command" puts it on your PATH.
//
//   node scripts/prepare-sidecar.mjs [release|debug]
//
// Builds for TAURI_ENV_TARGET_TRIPLE when Tauri sets it (cross builds), else for the host.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const profile = process.argv[2] === "debug" ? "debug" : "release";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const host = /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1].trim();
const triple = process.env.TAURI_ENV_TARGET_TRIPLE || host;
const cross = triple !== host;

const args = ["build", "-p", "bk-cli", "--manifest-path", join(root, "Cargo.toml")];
if (profile === "release") args.push("--release");
if (cross) args.push("--target", triple);
execFileSync("cargo", args, { stdio: "inherit" });

const targetDir = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(root, "target");
const exe = triple.includes("windows") ? ".exe" : "";
const built = join(targetDir, ...(cross ? [triple] : []), profile, `bk${exe}`);
const dest = join(root, "app", "src-tauri", "binaries");
mkdirSync(dest, { recursive: true });
copyFileSync(built, join(dest, `bk-${triple}${exe}`));
console.log(`sidecar: binaries/bk-${triple}${exe} (${profile})`);
