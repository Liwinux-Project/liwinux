#!/usr/bin/env bash
# liwinux PoC 2 stage A - BACK UP THE WORKING STATE
# Usage: sudo bash poc2-backup.sh
set -euo pipefail
TS=$(date +%Y%m%d-%H%M%S)
DST=/var/lib/waydroid-backup/$TS
S() { echo; echo "=== $* ==="; }

S "1. Stopping the session"
# The invoking user, not a name baked into the script.
U="${SUDO_USER:-$(logname 2>/dev/null)}"
sudo -u "$U" XDG_RUNTIME_DIR="/run/user/$(id -u "$U")" waydroid session stop 2>/dev/null || true
sleep 3
waydroid status 2>&1 | head -3

S "2. Sizes"
du -sh /var/lib/waydroid/overlay /var/lib/waydroid/images 2>/dev/null
du -sh "$HOME/.local/share/waydroid" 2>/dev/null
df -h / | tail -1

S "3. Backing up -> $DST"
mkdir -p "$DST"
cp -a /var/lib/waydroid/overlay              "$DST/overlay"
cp -a /var/lib/waydroid/waydroid.cfg         "$DST/waydroid.cfg"
cp -a /var/lib/waydroid/waydroid_base.prop   "$DST/waydroid_base.prop"
[ -d /var/lib/waydroid/overlay_rw ] && cp -a /var/lib/waydroid/overlay_rw "$DST/overlay_rw" || true
cp -a "$HOME/.local/share/waydroid"  "$DST/userdata"
echo "  copy finished"

S "4. VERIFY THE BACKUP (if this fails, microG is not installed)"
ok=1
for f in overlay/system/lib64/libhoudini.so overlay/system/bin/houdini64 waydroid.cfg waydroid_base.prop; do
  if [ -e "$DST/$f" ]; then echo "  OK   $f"; else echo "  MISSING $f"; ok=0; fi
done
n=$(ls "$DST/overlay/system/lib64/arm64/" 2>/dev/null | wc -l)
echo "  arm64 lib count in the backup: $n"; [ "$n" -ge 300 ] || ok=0
grep -q "ro.hardware.vulkan" "$DST/waydroid.cfg" && echo "  OK   venus properties" || { echo "  MISSING venus properties"; ok=0; }
grep -q "native.bridge" "$DST/waydroid.cfg" && echo "  OK   houdini properties" || { echo "  MISSING houdini properties"; ok=0; }

echo
if [ "$ok" = "1" ]; then
  echo "BACKUP IS SOUND -> $DST"
  echo "$DST" > /var/lib/waydroid-backup/LATEST
  du -sh "$DST"
else
  echo "BACKUP IS BROKEN - DO NOT CONTINUE!"; exit 1
fi

S "5. How to roll back (write this down)"
cat <<NOTE
  If something breaks:
    sudo systemctl stop waydroid-container
    sudo rm -rf /var/lib/waydroid/overlay
    sudo cp -a $DST/overlay /var/lib/waydroid/overlay
    sudo cp -a $DST/waydroid.cfg /var/lib/waydroid/waydroid.cfg
    sudo cp -a $DST/waydroid_base.prop /var/lib/waydroid/waydroid_base.prop
    rm -rf "$HOME/.local/share/waydroid"
    cp -a $DST/userdata "$HOME/.local/share/waydroid"
    sudo systemctl start waydroid-container
NOTE
