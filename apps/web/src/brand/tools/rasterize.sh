#!/bin/bash
# Step 2 of draw.py: rasterise the SVGs with the headless Chromium of the
# Playwright image, at the exact pixel size of each icon.
set -euo pipefail
SHELL_BIN=$(ls /ms-playwright/chromium_headless_shell-*/chrome-*/chrome-headless-shell 2>/dev/null || ls /ms-playwright/chromium_headless_shell-*/chrome-*/headless_shell)
cd /out
r() { # svg size png
  printf '<!doctype html><html><head><style>html,body{margin:0;background:transparent}img{display:block}</style></head><body><img src="%s" width="%s" height="%s"></body></html>' "$1" "$2" "$2" > page.html
  "$SHELL_BIN" --no-sandbox --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
    --default-background-color=00000000 --window-size="$2,$2" --screenshot="/out/$3" "file:///out/page.html" >/dev/null 2>&1
}
r favicon.svg 32 favicon-32.png
r favicon.svg 16 favicon-16.png
r apple-touch-icon.svg 180 apple-touch-icon.png
r favicon.svg 192 icon-192.png
r favicon.svg 512 icon-512.png
r icon-maskable.svg 512 icon-maskable-512.png
rm page.html
