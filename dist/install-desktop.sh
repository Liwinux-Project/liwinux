#!/usr/bin/env bash
# Installs the launcher entry and its icon for the current user.
#
# Wayland has no way to hand a window a picture: the compositor looks up the
# window's app_id in the desktop entries and takes the icon from there. So the
# entry and the icon theme are how liwinux gets an icon at all, and the app_id
# in the code has to match StartupWMClass here.
set -euo pipefail
R="$(cd "$(dirname "$0")/.." && pwd)"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"

install -Dm644 "$R/dist/desktop/liwinux.desktop" "$apps/liwinux.desktop"
for s in 32 48 64 128 256; do
  install -Dm644 "$R/dist/icons/liwinux-$s.png" "$icons/${s}x${s}/apps/liwinux.png"
done
update-desktop-database "$apps" 2>/dev/null || true
gtk-update-icon-cache -f -t "$icons" 2>/dev/null || true
echo "Installed. KDE may need a moment, or a re-login, to pick up a new icon."
