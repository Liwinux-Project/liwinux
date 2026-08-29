#!/usr/bin/env bash
# liwinux PoC 2 stage B - install microG and Aurora Store
# Usage: sudo bash poc2-microg.sh
set -u
WS="${WS:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/tools/waydroid_script}"
BK=$(cat /var/lib/waydroid-backup/LATEST 2>/dev/null || echo "NONE")
S() { echo; echo "=== $* ==="; }

S "0. Safety check"
echo "  backup: $BK"
[ -d "$BK" ] || { echo "  NO BACKUP FOUND - STOPPING"; exit 1; }
waydroid status 2>&1 | head -2

S "1. Reference before install (compared afterwards)"
grep -cE "." /var/lib/waydroid/waydroid.cfg
echo "  venus prop:   $(grep -c 'ro.hardware.vulkan\|mesa.vtest' /var/lib/waydroid/waydroid.cfg)"
echo "  houdini prop: $(grep -c 'native.bridge\|abilist' /var/lib/waydroid/waydroid.cfg)"
echo "  arm64 lib:    $(ls /var/lib/waydroid/overlay/system/lib64/arm64/ 2>/dev/null | wc -l)"

S "2. Installing microG and Aurora Store"
cd "$WS" || exit 1
./.venv/bin/python main.py -a 13 install microg
echo "  exit code: $?"

S "3. After install - DID THE WORKING STACK SURVIVE? (CRITICAL)"
echo "  --- venus properties ---"
grep -iE "vulkan|egl|vtest|hwui|venus" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- houdini properties ---"
grep -iE "native.bridge|abilist64|isa.arm64" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- houdini files ---"
for f in system/lib64/libhoudini.so system/bin/houdini64; do
  [ -e "/var/lib/waydroid/overlay/$f" ] && echo "    OK  $f" || echo "    MISSING $f"
done
echo "    arm64 lib count: $(ls /var/lib/waydroid/overlay/system/lib64/arm64/ 2>/dev/null | wc -l)  (should be 323)"

S "4. New microG/Aurora files"
ls /var/lib/waydroid/overlay/system/priv-app/ 2>/dev/null | sed 's/^/    /'
ls /var/lib/waydroid/overlay/system/app/ 2>/dev/null | sed 's/^/    /'
