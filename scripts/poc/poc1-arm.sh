#!/usr/bin/env bash
# liwinux PoC 1 — ARM64 ceviri katmani (libhoudini, Intel CPU icin)
# Kullanim: sudo bash poc1-arm.sh
set -u
WS=/home/wintone01/Projects/liwinux/tools/waydroid_script
S() { echo; echo "=== $* ==="; }

S "0. Onceki durum (referans)"
grep -E "abilist|native.bridge" /var/lib/waydroid/waydroid_base.prop 2>/dev/null || echo "  (base.prop'ta ARM prop'u yok - beklenen)"
echo "  --- nvidia/venus prop yedegi ---"
cp -a /var/lib/waydroid/waydroid.cfg /var/lib/waydroid/waydroid.cfg.bak-$(date +%s) 2>/dev/null && echo "  waydroid.cfg yedeklendi"
grep -iE "venus|nv|gralloc|angle|vulkan" /var/lib/waydroid/waydroid.cfg 2>/dev/null | head -20

S "1. libhoudini kurulumu (Android 13)"
cd "$WS" || exit 1
./.venv/bin/python main.py -a 13 install libhoudini
rc=$?
echo "  cikis kodu: $rc"

S "2. Kurulum sonrasi property'ler"
grep -E "abilist|native.bridge|isa.arm" /var/lib/waydroid/waydroid_base.prop 2>/dev/null

S "3. NVIDIA/Venus prop'lari HALA duruyor mu? (KRITIK)"
grep -iE "venus|gralloc|angle|vulkan|hwui" /var/lib/waydroid/waydroid.cfg /var/lib/waydroid/waydroid_base.prop 2>/dev/null | head -20
echo "  --- yukarisi bossa: sudo waydroid-nvidia-setup TEKRAR calistirilmali ---"
