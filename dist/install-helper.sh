#!/usr/bin/env bash
# liwd-helper installation (needs root)
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0"; exit 1; }
R="$(cd "$(dirname "$0")/.." && pwd)"
install -Dm755 "$R/target/release/liwd-helper" /usr/local/bin/liwd-helper
install -Dm644 "$R/dist/polkit/id.liwinux.policy" /usr/share/polkit-1/actions/id.liwinux.policy
install -Dm644 "$R/dist/dbus/id.liwinux.Helper1.conf" /usr/share/dbus-1/system.d/id.liwinux.Helper1.conf
install -Dm644 "$R/dist/systemd/liwd-helper.service" /etc/systemd/system/liwd-helper.service
systemctl daemon-reload
systemctl reload dbus 2>/dev/null || systemctl reload dbus-broker 2>/dev/null || true
echo "Installed. To start:  sudo systemctl enable --now liwd-helper"
