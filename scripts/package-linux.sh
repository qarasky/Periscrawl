#!/usr/bin/env bash
# Build the Linux deliverables for Periscrawl (unsigned).
#
# Produces:
#   dist/Periscrawl-linux-x86_64.tar.gz       — portable bundle + install.sh
#   dist/Periscrawl-<version>-x86_64.AppImage — single-file app (when
#                                               appimagetool is available)
#
# Environment:
#   PERISCRAWL_VERSION     version string (default 0.2.0)
#   PERISCRAWL_TARGET_DIR  cargo output dir (default target/release)
#   APPIMAGETOOL           path to appimagetool (default: search PATH)
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="Periscrawl"
VERSION="${PERISCRAWL_VERSION:-0.2.0}"
TARGET_DIR="${PERISCRAWL_TARGET_DIR:-target/release}"
DIST="dist"
STAGE="$DIST/$NAME-linux"

echo "==> Building release binaries"
cargo build --release -p periscrawl-gui -p periscrawl-cli

echo "==> Staging tar.gz bundle"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/icons/hicolor/512x512/apps"
cp "$TARGET_DIR/periscrawl-gui" "$STAGE/bin/periscrawl-gui"
cp "$TARGET_DIR/periscrawl" "$STAGE/bin/periscrawl"
cp scripts/make_icon.py "$STAGE/share/"
python3 scripts/make_icon.py "$STAGE/share/icons/hicolor/512x512/apps/periscrawl.png"
cp packaging/linux/periscrawl.desktop "$STAGE/share/applications/periscrawl.desktop"

cat > "$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install Periscrawl for the current user (~/.local).
set -euo pipefail
cd "$(dirname "$0")"
install -Dm755 bin/periscrawl-gui "$HOME/.local/bin/periscrawl-gui"
install -Dm755 bin/periscrawl "$HOME/.local/bin/periscrawl"
install -Dm644 share/applications/periscrawl.desktop "$HOME/.local/share/applications/periscrawl.desktop"
install -Dm644 share/icons/hicolor/512x512/apps/periscrawl.png "$HOME/.local/share/icons/hicolor/512x512/apps/periscrawl.png"
echo "Installed. Ensure ~/.local/bin is on your PATH."
INSTALL
chmod +x "$STAGE/install.sh"

echo "==> Archiving"
rm -f "$DIST/$NAME-linux-x86_64.tar.gz"
tar -C "$DIST" -czf "$DIST/$NAME-linux-x86_64.tar.gz" "$(basename "$STAGE")"

echo "==> Building AppImage"
APPDIR="$DIST/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
cp "$TARGET_DIR/periscrawl-gui" "$TARGET_DIR/periscrawl" "$APPDIR/usr/bin/"
cp packaging/linux/AppRun "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun"
cp packaging/linux/periscrawl.desktop "$APPDIR/periscrawl.desktop"
cp packaging/linux/periscrawl.desktop "$APPDIR/usr/share/applications/periscrawl.desktop"
python3 scripts/make_icon.py "$APPDIR/usr/share/icons/hicolor/256x256/apps/periscrawl.png" 256
cp "$APPDIR/usr/share/icons/hicolor/256x256/apps/periscrawl.png" "$APPDIR/periscrawl.png"
cp "$APPDIR/periscrawl.png" "$APPDIR/.DirIcon"

APPIMAGETOOL="${APPIMAGETOOL:-appimagetool}"
if command -v "$APPIMAGETOOL" >/dev/null 2>&1 || [[ -x "$APPIMAGETOOL" ]]; then
    APPIMAGE_OUT="$DIST/$NAME-$VERSION-x86_64.AppImage"
    rm -f "$APPIMAGE_OUT"
    # APPIMAGE_EXTRACT_AND_RUN lets the tool run without FUSE (CI containers).
    ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGETOOL" "$APPDIR" "$APPIMAGE_OUT"
    rm -rf "$APPDIR"
    echo
    echo "Done: $DIST/$NAME-linux-x86_64.tar.gz"
    echo "      $APPIMAGE_OUT"
else
    rm -rf "$APPDIR"
    echo
    echo "Done: $DIST/$NAME-linux-x86_64.tar.gz"
    echo
    echo "AppImage skipped: appimagetool not found."
    echo "Set APPIMAGETOOL=/path/to/appimagetool (CI downloads it) to build one."
fi
echo "Teammates: chmod +x the AppImage and run it, or extract the tar.gz and run ./install.sh."
