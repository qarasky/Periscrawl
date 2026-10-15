#!/usr/bin/env bash
# Build the macOS deliverables for Periscrawl (unsigned / ad-hoc signed).
#
# Produces:
#   dist/Periscrawl.app             — double-clickable app bundle
#   dist/Periscrawl-<version>.dmg   — disk image (app + /Applications link)
#   dist/Periscrawl-macos.zip       — zipped app bundle
#   dist/periscrawl                 — the CLI binary
#
# Environment:
#   PERISCRAWL_VERSION       version string (default 0.2.0)
#   PERISCRAWL_CARGO_TARGET  "universal2" builds both architectures and lipos
#                            them together (what CI uses); any other value is
#                            passed to `cargo build --target`
#   PERISCRAWL_TARGET_DIR    cargo output dir for non-universal builds
#                            (default target/release)
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="Periscrawl"
VERSION="${PERISCRAWL_VERSION:-0.2.0}"
TARGET_DIR="${PERISCRAWL_TARGET_DIR:-target/release}"
BUNDLE_ID="com.periscrawl.app"
DIST="dist"
APP="$DIST/$NAME.app"

echo "==> Building release binaries"
if [[ "${PERISCRAWL_CARGO_TARGET:-}" == "universal2" ]]; then
    cargo build --release -p periscrawl-gui -p periscrawl-cli --target x86_64-apple-darwin
    cargo build --release -p periscrawl-gui -p periscrawl-cli --target aarch64-apple-darwin
    TARGET_DIR="target/universal2-release"
    mkdir -p "$TARGET_DIR"
    for bin in periscrawl-gui periscrawl; do
        lipo -create -output "$TARGET_DIR/$bin" \
            "target/x86_64-apple-darwin/release/$bin" \
            "target/aarch64-apple-darwin/release/$bin"
    done
elif [[ -n "${PERISCRAWL_CARGO_TARGET:-}" ]]; then
    cargo build --release -p periscrawl-gui -p periscrawl-cli --target "$PERISCRAWL_CARGO_TARGET"
else
    cargo build --release -p periscrawl-gui -p periscrawl-cli
fi

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$TARGET_DIR/periscrawl-gui" "$APP/Contents/MacOS/periscrawl-gui"
cp "$TARGET_DIR/periscrawl" "$DIST/periscrawl" 2>/dev/null || true

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
    <key>CFBundleExecutable</key><string>periscrawl-gui</string>
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

echo "==> Building DMG"
DMG_STAGE="$DIST/dmg"
DMG="$DIST/$NAME-$VERSION.dmg"
rm -rf "$DMG_STAGE"
mkdir -p "$DMG_STAGE"
cp -R "$APP" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "$NAME $VERSION" -srcfolder "$DMG_STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$DMG_STAGE"

echo
echo "Done:"
echo "  $APP"
echo "  $DIST/periscrawl  (CLI)"
echo "  $DIST/$NAME-macos.zip"
echo "  $DMG"
echo
echo "Note: builds are unsigned — teammates opening the app must allow it once"
echo "(right-click → Open, or System Settings → Privacy & Security → Open Anyway)."
