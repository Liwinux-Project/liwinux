#!/usr/bin/env bash
# VANILLA -> GAPPS yeniden kurulum + NVIDIA/Venus + libhoudini geri yukleme
# Kullanim: sudo bash rebuild-gapps.sh --yes
set -u
[ "$(id -u)" = 0 ] || { echo "root gerekiyor"; exit 1; }
[ "${1:-}" = "--yes" ] || { cat <<MSG
Bu betik YIKICI islemler yapar:
  * Android verisini siler (~/.local/share/waydroid)
  * overlay'i temizler (microG dahil)
  * system/vendor imajlarini YENIDEN INDIRIR (~1 GB)
  * waydroid.cfg'yi sifirlar
Once yedek alir. Onaylamak icin:  sudo bash $0 --yes
MSG
exit 1; }

WS=/home/wintone01/Projects/liwinux/tools/waydroid_script
U=wintone01; UH=/home/$U
TS=$(date +%Y%m%d-%H%M%S); BK=/var/lib/waydroid-backup/pre-gapps-$TS
S() { echo; echo "=== $* ==="; }
run_u() { sudo -u "$U" XDG_RUNTIME_DIR=/run/user/1000 "$@"; }

S "1. Durduruluyor"
run_u waydroid session stop 2>/dev/null || true; sleep 3
systemctl stop waydroid-container.service; sleep 2
waydroid status 2>&1 | head -2

S "2. Yedek -> $BK"
mkdir -p "$BK"
cp -a /var/lib/waydroid/waydroid.cfg        "$BK/" 2>/dev/null
cp -a /var/lib/waydroid/waydroid_base.prop  "$BK/" 2>/dev/null
cp -a /var/lib/waydroid/overlay             "$BK/overlay" 2>/dev/null
cp -a "$UH/.local/share/waydroid"           "$BK/userdata" 2>/dev/null
echo "  yedek boyutu: $(du -sh "$BK" | cut -f1)"
[ -f "$BK/waydroid.cfg" ] || { echo "  YEDEK BASARISIZ - DURULUYOR"; exit 1; }
echo "$BK" > /var/lib/waydroid-backup/LATEST

S "3. Eski durum temizleniyor (microG dahil)"
rm -rf /var/lib/waydroid/overlay /var/lib/waydroid/overlay_rw
rm -rf "$UH/.local/share/waydroid"
echo "  overlay ve Android verisi silindi"

S "4. GAPPS imaji indiriliyor (uzun surer)"
waydroid init -f -s GAPPS || { echo "  INIT BASARISIZ"; exit 1; }
grep -iE "system_type|vendor_type" /var/lib/waydroid/waydroid.cfg

S "5. NVIDIA/Venus yeniden kuruluyor"
waydroid-nvidia-setup || { echo "  NVIDIA SETUP BASARISIZ"; exit 1; }

S "6. libhoudini (ARM64) yeniden kuruluyor"
cd "$WS" && ./.venv/bin/python main.py -a 13 install libhoudini
echo "  cikis: $?"

S "7. Ayarlar bir arada mi"
echo "  --- venus ---"; grep -iE "vulkan|egl|vtest|hwui" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- houdini ---"; grep -iE "native.bridge|abilist64" /var/lib/waydroid/waydroid.cfg | sed 's/^/    /'
echo "  --- arm64 lib: $(ls /var/lib/waydroid/overlay/system/lib64/arm64/ 2>/dev/null | wc -l) (beklenen 323) ---"

S "8. Baslatiliyor"
systemctl start waydroid-container.service; sleep 5
run_u waydroid session start >/dev/null 2>&1 &
for i in $(seq 1 30); do
  b=$(waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>/dev/null | tr -d '\r' | tail -1)
  echo "  [$i] boot=$b"; [ "$b" = "1" ] && break; sleep 5
done

S "9. DOGRULAMA"
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
echo "  --- GPU ---"; W dumpsys SurfaceFlinger | grep -iE "^GLES" | head -1 | sed 's/^/    /'
echo "  --- ARM64 ---"; W sh -c '/system/bin/houdini64 --version 2>&1 | grep -i version' | sed 's/^/    /'
echo "  --- ABI ---"; echo "    $(W getprop ro.product.cpu.abilist | tail -1)"
echo "  --- GERCEK GMS var mi (GAPPS dogrulamasi) ---"
W pm list packages 2>/dev/null | grep -iE "com.google.android.gms|com.android.vending|com.google.android.gsf" | sed 's/^/    /'
echo "  --- microG kalintisi var mi (OLMAMALI) ---"
W pm list packages 2>/dev/null | grep -iE "microg|aurora" | sed 's/^/    /' || echo "    temiz"
echo "  --- ag ---"; waydroid status 2>&1 | grep -i "IP address" | sed 's/^/    /'
