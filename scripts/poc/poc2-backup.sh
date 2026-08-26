#!/usr/bin/env bash
# liwinux PoC 2 asama A — CALISIR DURUMUN YEDEGI
# Kullanim: sudo bash poc2-backup.sh
set -euo pipefail
TS=$(date +%Y%m%d-%H%M%S)
DST=/var/lib/waydroid-backup/$TS
S() { echo; echo "=== $* ==="; }

S "1. Session durduruluyor"
sudo -u wintone01 XDG_RUNTIME_DIR=/run/user/1000 waydroid session stop 2>/dev/null || true
sleep 3
waydroid status 2>&1 | head -3

S "2. Boyutlar"
du -sh /var/lib/waydroid/overlay /var/lib/waydroid/images 2>/dev/null
du -sh /home/wintone01/.local/share/waydroid 2>/dev/null
df -h / | tail -1

S "3. Yedekleniyor -> $DST"
mkdir -p "$DST"
cp -a /var/lib/waydroid/overlay              "$DST/overlay"
cp -a /var/lib/waydroid/waydroid.cfg         "$DST/waydroid.cfg"
cp -a /var/lib/waydroid/waydroid_base.prop   "$DST/waydroid_base.prop"
[ -d /var/lib/waydroid/overlay_rw ] && cp -a /var/lib/waydroid/overlay_rw "$DST/overlay_rw" || true
cp -a /home/wintone01/.local/share/waydroid  "$DST/userdata"
echo "  kopyalama bitti"

S "4. YEDEK DOGRULAMA (gecmezse microG kurulmayacak)"
ok=1
for f in overlay/system/lib64/libhoudini.so overlay/system/bin/houdini64 waydroid.cfg waydroid_base.prop; do
  if [ -e "$DST/$f" ]; then echo "  OK   $f"; else echo "  EKSIK $f"; ok=0; fi
done
n=$(ls "$DST/overlay/system/lib64/arm64/" 2>/dev/null | wc -l)
echo "  arm64 lib adedi yedekte: $n"; [ "$n" -ge 300 ] || ok=0
grep -q "ro.hardware.vulkan" "$DST/waydroid.cfg" && echo "  OK   venus prop'lari" || { echo "  EKSIK venus prop"; ok=0; }
grep -q "native.bridge" "$DST/waydroid.cfg" && echo "  OK   houdini prop'lari" || { echo "  EKSIK houdini prop"; ok=0; }

echo
if [ "$ok" = "1" ]; then
  echo "YEDEK SAGLAM -> $DST"
  echo "$DST" > /var/lib/waydroid-backup/LATEST
  du -sh "$DST"
else
  echo "YEDEK BOZUK — DEVAM ETME!"; exit 1
fi

S "5. Geri donus komutu (not al)"
cat <<NOTE
  Bir sey bozulursa:
    sudo systemctl stop waydroid-container
    sudo rm -rf /var/lib/waydroid/overlay
    sudo cp -a $DST/overlay /var/lib/waydroid/overlay
    sudo cp -a $DST/waydroid.cfg /var/lib/waydroid/waydroid.cfg
    sudo cp -a $DST/waydroid_base.prop /var/lib/waydroid/waydroid_base.prop
    rm -rf /home/wintone01/.local/share/waydroid
    cp -a $DST/userdata /home/wintone01/.local/share/waydroid
    sudo systemctl start waydroid-container
NOTE
