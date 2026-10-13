#!/usr/bin/env bash
# Build a distributable Linux bundle for Periscrawl.
#
# Produces dist/Periscrawl-linux-x86_64.tar.gz containing the GUI binary, the
# CLI binary, a .desktop entry and the app icon.
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="Periscrawl"
VERSION="0.2.0"
DIST="dist"
STAGE="$DIST/$NAME-linux"

echo "==> Building release binaries"
cargo build --release -p periscrawl-gui -p periscrawl-cli

echo "==> Staging"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/icons/hicolor/512x512/apps"
cp target/release/periscrawl-gui "$STAGE/bin/periscrawl-gui"
cp target/release/periscrawl "$STAGE/bin/periscrawl"
cp scripts/make_icon.py "$STAGE/share/"
python3 scripts/make_icon.py "$STAGE/share/icons/hicolor/512x512/apps/periscrawl.png"

cat > "$STAGE/share/applications/periscrawl.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$NAME
Comment=Website crawler and SEO audit toolkit
Exec=periscrawl-gui
Icon=periscrawl
Categories=Development;Utility;
Terminal=false
DESKTOP

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

echo
echo "Done: $DIST/$NAME-linux-x86_64.tar.gz"
echo "Teammates: extract and run ./install.sh (installs to ~/.local)."
