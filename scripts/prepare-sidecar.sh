#!/usr/bin/env bash
# Build the llmproxy CLI binary and copy it into the Tauri sidecar directory
# so the desktop app can start/stop the proxy.
#
# Usage (from repo root):
#   ./scripts/prepare-sidecar.sh           # debug build (fast, for `tauri dev`)
#   ./scripts/prepare-sidecar.sh --release # release build (for `tauri build`)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SIDECAR_DIR="$REPO_ROOT/app/src-tauri/binaries"

RELEASE=0
for arg in "$@"; do
  [[ "$arg" == "--release" ]] && RELEASE=1
done

# Determine the host target triple (e.g. aarch64-apple-darwin).
TRIPLE=$(rustc -vV | awk '/^host:/ { print $2 }')

if [[ $RELEASE -eq 1 ]]; then
  echo "Building llmproxy (release)…"
  cargo build --release -p llmproxy-server --manifest-path "$REPO_ROOT/Cargo.toml"
  SRC="$REPO_ROOT/target/release/llmproxy"
else
  echo "Building llmproxy (debug)…"
  cargo build -p llmproxy-server --manifest-path "$REPO_ROOT/Cargo.toml"
  SRC="$REPO_ROOT/target/debug/llmproxy"
fi

mkdir -p "$SIDECAR_DIR"
DEST="$SIDECAR_DIR/llmproxy-$TRIPLE"
cp "$SRC" "$DEST"
echo "Sidecar ready: $DEST"
