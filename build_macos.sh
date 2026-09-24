#!/bin/bash
set -e
cd "$(dirname "$0")"

APP_NAME="NguonC Downloader"

echo "Building macOS app with flet build..."
rm -rf dist build
# --arch arm64: skip default x86_64 pass (Rust/cryptography link fails via
# xcrun when only arm64 CLT is installed; CI macos-latest is also arm64).
# --python-version 3.12: match CI uv Python; avoid flet defaulting to 3.14.
# Requires full Xcode (xcodebuild) — CLT alone is not enough for Flutter macOS.
uv run --no-sync flet build macos --yes \
    --arch arm64 \
    --python-version 3.12 \
    --product "$APP_NAME" \
    --description "Cross-platform desktop app that downloads movies from phim.nguonc.com" \
    -o dist

# Locate the built .app (copy_build_output puts it under -o)
APP_PATH="dist/$APP_NAME.app"
if [ ! -d "$APP_PATH" ]; then
    APP_PATH=$(find dist -maxdepth 3 -name '*.app' -type d | head -1)
fi
if [ -z "$APP_PATH" ] || [ ! -d "$APP_PATH" ]; then
    echo "ERROR: built .app not found under dist/" >&2
    find . -name '*.app' -maxdepth 5 -type d 2>/dev/null || true
    exit 1
fi

# Ensure bundle name is correct
PLIST="$APP_PATH/Contents/Info.plist"
if [ -f "$PLIST" ]; then
    plutil -replace CFBundleName -string "$APP_NAME" "$PLIST"
    plutil -replace CFBundleDisplayName -string "$APP_NAME" "$PLIST"
fi

# Stage a stable path for CI dmg step
if [ "$APP_PATH" != "dist/$APP_NAME.app" ]; then
    rm -rf "dist/$APP_NAME.app"
    mv "$APP_PATH" "dist/$APP_NAME.app"
fi

echo "Done. App at: dist/$APP_NAME.app"
