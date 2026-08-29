#!/usr/bin/env bash
# Read the Waydroid window geometry from KWin and work out the liw --region value.
set -u
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript "liwinux-geom" >/dev/null 2>&1
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript \
  "$R/scripts/kwin/geometry.js" "liwinux-geom" >/dev/null 2>&1
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.start >/dev/null 2>&1
sleep 1
RAW=$(journalctl --user -n 100 --no-pager --since "20 sec ago" 2>/dev/null \
      | grep -oE "(window|desktop|fullscreen)=[0-9,a-z]+" | tail -3)
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript "liwinux-geom" >/dev/null 2>&1
echo "$RAW"

WIN=$(echo "$RAW" | grep "^window=" | tail -1 | cut -d= -f2)
DESK=$(echo "$RAW" | grep "^desktop=" | tail -1 | cut -d= -f2)
[ -n "$WIN" ] || { echo "Waydroid penceresi bulunamadi (acik mi?)"; exit 1; }

# Masaustu bilgisi gelmediyse kscreen-doctor'dan hesapla
if [ -z "$DESK" ]; then
  DESK=$(kscreen-doctor -j 2>/dev/null | python3 -c "
import sys,json
d=json.load(sys.stdin); mx=my=0
for o in d.get('outputs',[]):
    if o.get('enabled'):
        p,s=o['pos'],o['size']
        mx=max(mx,p['x']+s['width']); my=max(my,p['y']+s['height'])
print(f'{mx},{my}')")
fi

python3 - "$WIN" "$DESK" <<'PY'
import sys
wx,wy,ww,wh = map(float, sys.argv[1].split(','))
dw,dh = map(float, sys.argv[2].split(','))
print()
print(f"  window   : {int(wx)},{int(wy)}  {int(ww)}x{int(wh)}")
print(f"  desktop  : {int(dw)}x{int(dh)}")
print()
print("  If the touchscreen maps to the WHOLE DESKTOP:")
print(f"    --region {wx/dw:.5f},{wy/dh:.5f},{ww/dw:.5f},{wh/dh:.5f}")
print()
# Find the output that contains the window
print("  If the touchscreen maps ONLY to the output holding the window:")
print("    (pick the output size from below)")
PY
kscreen-doctor -j 2>/dev/null | python3 -c "
import sys,json
d=json.load(sys.stdin)
import os
wx,wy = [float(v) for v in os.environ.get('WINXY','0,0').split(',')]
for o in d.get('outputs',[]):
    if o.get('enabled'):
        p,s=o['pos'],o['size']
        print(f\"      {o.get('name')}: pos={p['x']},{p['y']} size={s['width']}x{s['height']}\")
"
