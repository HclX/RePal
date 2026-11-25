#!/usr/bin/env bash
# Install the RePal desktop launcher and Flying Sword icon on Linux so file managers,
# application menus, and docks show the RePal icon instead of the generic ELF gear.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
ICON_SRC="$DIR/repal_icon.png"
BIN_SRC="$DIR/repal"

if [ ! -f "$BIN_SRC" ] || [ ! -f "$ICON_SRC" ]; then
  echo "Error: run this script from the extracted RePal directory containing repal and repal_icon.png."
  exit 1
fi

ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$ICON_DIR" "$APP_DIR"

cp "$ICON_SRC" "$ICON_DIR/repal.png"

cat > "$APP_DIR/repal.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=RePal — 仙剑奇侠传 HD
Comment=Pure-Rust HD remake of 1995 Chinese Paladin
Exec="$BIN_SRC"
Path=$DIR
Icon=repal
Terminal=false
Categories=Game;RolePlaying;
StartupWMClass=repal
EOF

chmod +x "$BIN_SRC" "$APP_DIR/repal.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APP_DIR" || true
echo "Installed RePal icon & launcher at $APP_DIR/repal.desktop"
