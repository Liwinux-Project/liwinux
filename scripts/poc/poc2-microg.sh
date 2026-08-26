#!/usr/bin/env bash
# liwinux PoC 2 asama B — microG + Aurora Store kurulumu
# Kullanim: sudo bash poc2-microg.sh
set -u
WS=/home/wintone01/Projects/liwinux/tools/waydroid_script
BK=$(cat /var/lib/waydroid-backup/LATEST 2>/dev/null || echo "YOK")
S() { echo; echo "=== $* ==="; }

S "0. Guvenlik kontrolu"
echo "  yedek: $BK"
[ -d "$BK" ] || { echo "  YEDEK BULUNAMADI — DURULUYOR"; exit 1; }
waydroid status 2>&1 | head -2

S "1. Kurulum oncesi referans (sonra karsilastiracagiz)"
grep -cE "." /var/lib/waydroid/waydroid.cfg
echo "  venus prop:   $(grep -c 'ro.hardware.vulkan\|mesa.vtest' /var/lib/waydroid/waydroid.cfg)"
echo "  houdini prop: $(grep -c 'native.bridge\|abilist' /var/lib/waydroid/waydroid.cfg)"
echo "  arm64 lib:    $(ls /var/lib/waydroid/overlay/system/lib64/arm64/ 2>/dev/null | wc -l)"

S "2. microG + Aurora Store kuruluyor"
cd "$WS" || exit 1
./.venv/bin/python main.py -a 13 install microg
echo "  cikis kodu: $?"

S "3. Kurulum sonrasi — CALISAN YIGIN BOZULDU MU? (KRITIK)"
echo "  --- venus prop'lari ---"
grep -iE "vulkan|egl|vtest|hwui|venus" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- houdini prop'lari ---"
grep -iE "native.bridge|abilist64|isa.arm64" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- houdini dosyalari ---"
for f in system/lib64/libhoudini.so system/bin/houdini64; do
  [ -e "/var/lib/waydroid/overlay/$f" ] && echo "    OK  $f" || echo "    KAYIP $f"
done
echo "    arm64 lib adedi: $(ls /var/lib/waydroid/overlay/system/lib64/arm64/ 2>/dev/null | wc -l)  (olmasi gereken: 323)"

S "4. Yeni gelen microG/Aurora dosyalari"
ls /var/lib/waydroid/overlay/system/priv-app/ 2>/dev/null | sed 's/^/    /'
ls /var/lib/waydroid/overlay/system/app/ 2>/dev/null | sed 's/^/    /'
