#!/usr/bin/env bash
# 1) GetProp neden bos?  2) NetRepair gercekten yetki istedi mi?
set -u
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
S() { echo; echo "=== $* ==="; }

S "1. polkit KAYDI: net-repair icin kimlik dogrulamasi yapildi mi?"
echo "  (asagida 'successfully authenticated' varsa polkit gorevini yapti)"
sudo journalctl -u polkit --since "20 min ago" --no-pager 2>&1 | grep -iE "authenticat|authoriz|net-repair|liwinux" | tail -15 | sed 's/^/  /'
echo "  --- polkit gecici yetkiler ---"
pkcheck --action-id id.liwinux.helper.net-repair --process $$ 2>&1 | sed 's/^/  /'; echo "    cikis: $?"

S "2. waydroid shell DOGRUDAN root olarak (referans)"
sudo waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>&1 | sed 's/^/  stdout: /'

S "3. AYNI komut, helper'in systemd baglaminda (sertlestirme etkisi)"
sudo systemd-run --quiet --pipe --wait \
  --property=NoNewPrivileges=yes \
  --property=ProtectHome=read-only \
  --property=ProtectKernelModules=yes \
  --property=ProtectControlGroups=yes \
  --property=MemoryDenyWriteExecute=yes \
  waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>&1 | sed 's/^/  /'
echo "  ---- ayni ama ProtectHome KAPALI ----"
sudo systemd-run --quiet --pipe --wait \
  --property=NoNewPrivileges=yes \
  waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>&1 | sed 's/^/  /'

S "4. Hangi sertlestirme suclu? tek tek"
for prop in ProtectHome=read-only MemoryDenyWriteExecute=yes ProtectControlGroups=yes NoNewPrivileges=yes; do
  out=$(sudo systemd-run --quiet --pipe --wait --property="$prop" \
        waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>&1 | tr -d '\r\n ')
  printf "  %-32s -> '%s'\n" "$prop" "$out"
done

S "5. Helper loglari (GetProp cagrisi ne dedi)"
sudo journalctl -u liwd-helper -n 20 --no-pager 2>&1 | tail -12 | sed 's/^/  /'
