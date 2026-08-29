#!/usr/bin/env bash
# VANILLA -> GAPPS rebuild, then restore NVIDIA/Venus and libhoudini
# Usage: sudo bash rebuild-gapps.sh --yes
set -u
[ "$(id -u)" = 0 ] || { echo "root required"; exit 1; }
[ "${1:-}" = "--yes" ] || { cat <<MSG
This script is DESTRUCTIVE:
  * deletes the Android data (~/.local/share/waydroid)
  * wipes the overlay (microG included)
  * RE-DOWNLOADS the system/vendor images (~1 GB)
  * resets waydroid.cfg
It takes a backup first. To confirm:  sudo bash $0 --yes
MSG
exit 1; }

WS="${WS:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/tools/waydroid_script}"
# Run as the user who invoked sudo, not a name baked into the script.
U="${SUDO_USER:-$(logname 2>/dev/null)}"
[ -n "$U" ] || { echo "cannot tell which user to run as; set SUDO_USER"; exit 1; }
UH=$(getent passwd "$U" | cut -d: -f6)
[ -n "$UH" ] || { echo "no home directory for $U"; exit 1; }
TS=$(date +%Y%m%d-%H%M%S); BK=/var/lib/waydroid-backup/pre-gapps-$TS
S() { echo; echo "=== $* ==="; }
UID_OF_U=$(id -u "$U")
run_u() { sudo -u "$U" XDG_RUNTIME_DIR="/run/user/$UID_OF_U" "$@"; }

S "1. Stopping"
run_u waydroid session stop 2>/dev/null || true; sleep 3
systemctl stop waydroid-container.service; sleep 2
waydroid status 2>&1 | head -2

S "2. Backup -> $BK"
mkdir -p "$BK"
cp -a /var/lib/waydroid/waydroid.cfg        "$BK/" 2>/dev/null
cp -a /var/lib/waydroid/waydroid_base.prop  "$BK/" 2>/dev/null
cp -a /var/lib/waydroid/overlay             "$BK/overlay" 2>/dev/null
cp -a "$UH/.local/share/waydroid"           "$BK/userdata" 2>/dev/null
echo "  backup size: $(du -sh "$BK" | cut -f1)"
[ -f "$BK/waydroid.cfg" ] || { echo "  BACKUP FAILED - STOPPING"; exit 1; }
echo "$BK" > /var/lib/waydroid-backup/LATEST

S "3. Clearing the old state (microG included)"
rm -rf /var/lib/waydroid/overlay /var/lib/waydroid/overlay_rw
rm -rf "$UH/.local/share/waydroid"
echo "  overlay and Android data deleted"

S "4. Downloading the GAPPS image (this takes a while)"
waydroid init -f -s GAPPS || { echo "  INIT FAILED"; exit 1; }
grep -iE "system_type|vendor_type" /var/lib/waydroid/waydroid.cfg

S "5. Reinstalling NVIDIA/Venus"
waydroid-nvidia-setup || { echo "  NVIDIA SETUP FAILED"; exit 1; }

S "6. Reinstalling libhoudini (ARM64)"
cd "$WS" && ./.venv/bin/python main.py -a 13 install libhoudini
echo "  exit: $?"

S "7. Are the settings all present"
echo "  --- venus ---"; grep -iE "vulkan|egl|vtest|hwui" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- houdini ---"; grep -iE "native.bridge|abilist64" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- arm64 lib: $(ls /var/lib/waydroid/overlay/system/lib64/arm64/ 2>/dev/null | wc -l) (323 expected) ---"

S "8. Starting"
systemctl start waydroid-container.service; sleep 5
run_u waydroid session start >/dev/null 2>&1 &
for i in $(seq 1 30); do
  b=$(waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>/dev/null | tr -d '\r' | tail -1)
  echo "  [$i] boot=$b"; [ "$b" = "1" ] && break; sleep 5
done

S "9. VERIFICATION"
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
echo "  --- GPU ---"; W dumpsys SurfaceFlinger | grep -iE "^GLES" | head -1 | sed 's/^/    /'
echo "  --- ARM64 ---"; W sh -c '/system/bin/houdini64 --version 2>&1 | grep -i version' | sed 's/^/    /'
echo "  --- ABI ---"; echo "    $(W getprop ro.product.cpu.abilist | tail -1)"
echo "  --- is real GMS present (this is what proves GAPPS) ---"
W pm list packages 2>/dev/null | grep -iE "com.google.android.gms|com.android.vending|com.google.android.gsf" | sed 's/^/    /'
echo "  --- any microG left over (there MUST NOT be) ---"
W pm list packages 2>/dev/null | grep -iE "microg|aurora" | sed 's/^/    /' || echo "    clean"
echo "  --- network ---"; waydroid status 2>&1 | grep -i "IP address" | sed 's/^/    /'
