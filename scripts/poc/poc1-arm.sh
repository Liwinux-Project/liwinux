#!/usr/bin/env bash
# liwinux PoC 1 - the ARM64 translation layer (libhoudini, for Intel CPUs)
# Usage: sudo bash poc1-arm.sh
set -u
WS="${WS:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/tools/waydroid_script}"
S() { echo; echo "=== $* ==="; }

S "0. State before (the reference)"
grep -E "abilist|native.bridge" /var/lib/waydroid/waydroid_base.prop 2>/dev/null || echo "  (no ARM property in base.prop - expected)"
echo "  --- backing up the nvidia/venus properties ---"
cp -a /var/lib/waydroid/waydroid.cfg /var/lib/waydroid/waydroid.cfg.bak-$(date +%s) 2>/dev/null && echo "  waydroid.cfg backed up"
grep -iE "venus|nv|gralloc|angle|vulkan" /var/lib/waydroid/waydroid.cfg 2>/dev/null | head -20

S "1. libhoudini kurulumu (Android 13)"
cd "$WS" || exit 1
./.venv/bin/python main.py -a 13 install libhoudini
rc=$?
echo "  exit code: $rc"

S "2. Properties after installation"
grep -E "abilist|native.bridge|isa.arm" /var/lib/waydroid/waydroid_base.prop 2>/dev/null

S "3. NVIDIA/Venus prop'lari HALA duruyor mu? (KRITIK)"
grep -iE "venus|gralloc|angle|vulkan|hwui" /var/lib/waydroid/waydroid.cfg /var/lib/waydroid/waydroid_base.prop 2>/dev/null | head -20
echo "  --- if the above is empty: sudo waydroid-nvidia-setup must be run AGAIN ---"
