#!/usr/bin/env bash
# Build the macOS DMG installer.
#
# Works around a macOS 15 bug: the Tauri bundler passes --hide-extension to
# bundle_dmg.sh, which causes the AppleScript to fail with error -10006
# (Finder can't locate the item by name). We let Tauri build the .app with
# CI=true (which skips its broken DMG step), then recreate the DMG ourselves
# without --hide-extension.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cd "$ROOT/app"

echo "==> Building sidecar…"
npm run prepare-sidecar

echo "==> Building app (CI=true skips Tauri's broken DMG pass)…"
CI=true npx tauri build

APP="$ROOT/target/release/bundle/macos/llmproxy.app"
SCRIPT="$ROOT/target/release/bundle/dmg/bundle_dmg.sh"
DMG_DIR="$ROOT/target/release/bundle/dmg"
VERSION=$(node -e "console.log(require('./package.json').version)")
DMG_OUT="$DMG_DIR/llmproxy_${VERSION}_aarch64.dmg"

echo "==> Recreating DMG without --hide-extension…"
rm -f "$DMG_OUT"
STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT
cp -R "$APP" "$STAGING/"

# Positions/sizes mirror Tauri defaults; omit --hide-extension (breaks macOS 15)
bash "$SCRIPT" \
  --volname "llmproxy" \
  --icon "llmproxy.app" 180 170 \
  --app-drop-link 480 170 \
  --window-size 660 400 \
  "$DMG_OUT" \
  "$STAGING"

echo "==> Done: $DMG_OUT"
