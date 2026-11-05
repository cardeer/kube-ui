#!/usr/bin/env bash
# Builds "Kube UI.app" in target/release/bundle. Pass --install to copy it to /Applications.
set -euo pipefail

cd "$(dirname "$0")/.."
APP_NAME="Kube UI"
BUNDLE_ID="dev.kube-ui.app"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
OUT="target/release/bundle"
APP="$OUT/$APP_NAME.app"

# Icon: assets/icon.png is also embedded in the binary (the running app's Dock icon).
# Re-render it when the drawing script changes, then build the .icns from it.
ICON_PNG="assets/icon.png"
if [[ ! -f "$ICON_PNG" || scripts/make-icon.swift -nt "$ICON_PNG" ]]; then
    # Compiled rather than run with `swift`, whose interpreter can't always link AppKit.
    mkdir -p target
    swiftc -O -framework AppKit -o target/make-icon scripts/make-icon.swift
    target/make-icon "$ICON_PNG"
fi

cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/kube-ui "$APP/Contents/MacOS/kube-ui"

ICONSET="target/AppIcon.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z $s $s "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key><string>kube-ui</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc sign so Gatekeeper treats the locally built app consistently.
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true
echo "Built $APP"

if [[ "${1:-}" == "--install" ]]; then
    DEST="/Applications/$APP_NAME.app"
    rm -rf "$DEST"
    ditto "$APP" "$DEST"
    echo "Installed $DEST"
fi
