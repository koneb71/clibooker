#!/bin/sh
# Build the bk CLI and put it where Tauri expects the sidecar: src-tauri/binaries/bk-<target-triple>.
# The app bundles it as Contents/MacOS/bk, and Settings → "Install bk command" links it into ~/.local/bin.
set -e
profile=${1:-release}
root=$(cd "$(dirname "$0")/../.." && pwd)
triple=$(rustc -vV | sed -n 's/host: //p')
if [ "$profile" = release ]; then
  cargo build -p bk-cli --release --manifest-path "$root/Cargo.toml"
else
  cargo build -p bk-cli --manifest-path "$root/Cargo.toml"
fi
mkdir -p "$root/app/src-tauri/binaries"
cp "$root/target/$profile/bk" "$root/app/src-tauri/binaries/bk-$triple"
