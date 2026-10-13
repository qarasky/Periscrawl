#!/usr/bin/env bash
# Build a distributable Linux bundle for RustySEO.
#
# Produces dist/RustySEO-linux-x86_64.tar.gz containing the GUI binary, the
# CLI binary, a .desktop entry and the app icon.
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="RustySEO"
VERSION="0.2.0"
DIST="dist"
STAGE="$DIST/$NAME-linux"

echo "==> Building release binaries"
cargo build --release -p rustyseo-gui -p rustyseo-cli

echo "==> Staging"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/icons/hicolor/512x512/apps"
cp target/release/rustyseo-gui "$STAGE/bin/rustyseo-gui"
cp target/release/rustyseo "$STAGE/bin/rustyseo"
cp scripts/make_icon.py "$STAGE/share/"
python3 scripts/make_icon.py "$STAGE/share/icons/hicolor/512x512/apps/rustyseo.png"

cat > "$STAGE/share/applications/rustyseo.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$NAME
Comment=Website crawler and SEO audit toolkit
Exec=rustyseo-gui
Icon=rustyseo
Categories=Development;Utility;
Terminal=false
DESKTOP

cat > "$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install RustySEO for the current user (~/.local).
set -euo pipefail
cd "$(dirname "$0")"
install -Dm755 bin/rustyseo-gui "$HOME/.local/bin/rustyseo-gui"
install -Dm755 bin/rustyseo "$HOME/.local/bin/rustyseo"
install -Dm644 share/applications/rustyseo.desktop "$HOME/.local/share/applications/rustyseo.desktop"
install -Dm644 share/icons/hicolor/512x512/apps/rustyseo.png "$HOME/.local/share/icons/hicolor/512x512/apps/rustyseo.png"
echo "Installed. Ensure ~/.local/bin is on your PATH."
INSTALL
chmod +x "$STAGE/install.sh"

echo "==> Archiving"
rm -f "$DIST/$NAME-linux-x86_64.tar.gz"
tar -C "$DIST" -czf "$DIST/$NAME-linux-x86_64.tar.gz" "$(basename "$STAGE")"

echo
echo "Done: $DIST/$NAME-linux-x86_64.tar.gz"
echo "Teammates: extract and run ./install.sh (installs to ~/.local)."
