#!/usr/bin/env bash
# Build a distributable macOS app bundle for RustySEO.
#
# Produces:
#   dist/RustySEO.app        — double-clickable desktop app (ad-hoc signed)
#   dist/rustyseo            — the CLI binary
#   dist/RustySEO-macos.zip  — zipped bundle for sharing
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="RustySEO"
VERSION="0.2.0"
BUNDLE_ID="com.rustyseo.app"
DIST="dist"
APP="$DIST/$NAME.app"

echo "==> Building release binaries"
cargo build --release -p rustyseo-gui -p rustyseo-cli

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/rustyseo-gui" "$APP/Contents/MacOS/rustyseo-gui"
cp "target/release/rustyseo" "$DIST/rustyseo" 2>/dev/null || true

echo "==> Generating icon"
python3 scripts/make_icon.py "$DIST/icon_1024.png"
ICONSET="$DIST/icon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$DIST/icon_1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$DIST/icon_1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

echo "==> Writing Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$NAME</string>
    <key>CFBundleDisplayName</key><string>$NAME</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key><string>rustyseo-gui</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST
printf 'APPL????' > "$APP/Contents/PkgInfo"

echo "==> Ad-hoc code signing (Gatekeeper-friendly locally)"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "   (codesign unavailable; skipping)"

echo "==> Zipping"
rm -f "$DIST/$NAME-macos.zip"
ditto -c -k --keepParent "$APP" "$DIST/$NAME-macos.zip"

echo
echo "Done:"
echo "  $APP"
echo "  $DIST/rustyseo  (CLI)"
echo "  $DIST/$NAME-macos.zip"
echo
echo "Note: teammates opening the zip must allow it once (unsigned developer) —"
echo "right-click → Open, or System Settings → Privacy & Security → Open Anyway."
