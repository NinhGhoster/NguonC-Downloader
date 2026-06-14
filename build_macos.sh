#!/bin/bash
set -e
cd "$(dirname "$0")"

# Add Python user-installed binaries to PATH (for flet)
export PATH="$(python3 -c 'import site; print(site.USER_BASE)')/bin:$PATH"

APP_NAME="NguonC Downloader"
FLET_CACHE="$HOME/.flet/client/flet-desktop-full-0.85.3"
FLET_APP="$FLET_CACHE/Flet.app"
BACKUP="$FLET_CACHE/Flet.app.bak"

# Patch Flet.app's plist so the inner bundle has the right name
if [ -d "$FLET_APP" ]; then
    echo "Backing up original Flet.app..."
    rm -rf "$BACKUP"
    cp -R "$FLET_APP" "$BACKUP"
    echo "Patching CFBundleName to '$APP_NAME'..."
    plutil -replace CFBundleName -string "$APP_NAME" "$FLET_APP/Contents/Info.plist"
    plutil -replace CFBundleDisplayName -string "$APP_NAME" "$FLET_APP/Contents/Info.plist"
fi

echo "Building app..."
rm -rf dist build *.spec
PIP_REQUIRE_VIRTUALENV=0 flet pack nguonc_app.py --name "$APP_NAME" --icon assets/icon.icns

# Patch outer plist too
OUTER_PLIST="dist/$APP_NAME.app/Contents/Info.plist"
if [ -f "$OUTER_PLIST" ]; then
    plutil -replace CFBundleName -string "$APP_NAME" "$OUTER_PLIST"
    plutil -replace CFBundleDisplayName -string "$APP_NAME" "$OUTER_PLIST"
fi

# Restore original Flet.app
if [ -d "$BACKUP" ]; then
    echo "Restoring original Flet.app..."
    rm -rf "$FLET_APP"
    mv "$BACKUP" "$FLET_APP"
fi

echo "Done. App at: dist/$APP_NAME.app"
