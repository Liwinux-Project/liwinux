#!/usr/bin/env bash
# Sanal dokunmatik ekran gercekten olusuyor ve TANINIYOR mu?
set -u
S() { echo; echo "=== $* ==="; }

S "1. Sanal cihaz olusturuluyor (5sn ayakta kalacak)"
timeout 6 liw keymap test /home/wintone01/Projects/liwinux/profiles/subway-surfers.toml --inject &
LIW=$!
sleep 3

S "2. Cekirdek cihazi gordu mu"
grep -A6 "liwinux-virtual-touchscreen" /proc/bus/input/devices 2>/dev/null | sed 's/^/  /' \
  || echo "  BULUNAMADI"

S "3. libinput cihazi NASIL siniflandirdi (kritik)"
if command -v libinput >/dev/null; then
  timeout 5 sudo -n libinput list-devices 2>/dev/null | grep -B3 -A12 "liwinux" | sed 's/^/  /' \
    || echo "  (root gerekli: sudo libinput list-devices | grep -A12 liwinux)"
else
  echo "  libinput araci yok (paket: libinput)"
fi

S "4. KWin cihazi gordu mu"
qdbus6 org.kde.KWin /org/kde/KWin/InputDevice org.freedesktop.DBus.Properties.Get \
  org.kde.KWin.InputDeviceManager devicesSysNames 2>/dev/null | tr ',' '\n' | sed 's/^/  /' | head -20 \
  || echo "  (qdbus sorgusu basarisiz)"

wait $LIW 2>/dev/null
S "5. Cihaz temizlendi mi (surec olunce kaybolmali)"
grep -c "liwinux-virtual-touchscreen" /proc/bus/input/devices 2>/dev/null | sed 's/^/  kalan kayit: /'
