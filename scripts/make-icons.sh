#!/usr/bin/env bash
# Renders the committed icon files from crates/ternarygrid-app/assets/icon.svg, the one
# source of the app icon. Run it after changing the SVG (macOS: it draws with AppKit).
# The macOS .icns is made from the same SVG by scripts/bundle-macos.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
SVG=crates/ternarygrid-app/assets/icon.svg
swift scripts/render-icon.swift "$SVG" crates/ternarygrid-app/assets/icon-256.png 256
python3 scripts/make-windows-icon.py "$SVG" packaging/windows/ternarygrid.ico
