#!/usr/bin/env bash
# unwall gateway mode'u kapat -> waydroid0'daki DNS hijack'i kalksin
set -u
[ "$(id -u)" = 0 ] || { echo "root gerekiyor: sudo bash $0"; exit 1; }
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Mevcut ayar + yedek"
grep -n "GATEWAY_MODE" /etc/unwall/unwall.conf
cp -a /etc/unwall/unwall.conf "/etc/unwall/unwall.conf.bak-$(date +%s)"
echo "  yedeklendi: /etc/unwall/unwall.conf.bak-*"

S "2. GATEWAY_MODE=0 yapiliyor"
sed -i 's/^GATEWAY_MODE=.*/GATEWAY_MODE=0/' /etc/unwall/unwall.conf
grep -n "GATEWAY_MODE" /etc/unwall/unwall.conf

S "3. unwall yeniden baslatiliyor"
systemctl restart unwall.service
sleep 3
systemctl is-active unwall.service
systemctl status unwall.service --no-pager -n 3 2>&1 | tail -4

S "4. DNS hijack kurali GITTI mi? (KRITIK)"
if nft list ruleset 2>/dev/null | grep -q "dnat to .*:53"; then
  echo "  HALA VAR:"; nft list ruleset 2>/dev/null | grep -n "dnat to .*:53" | sed 's/^/    /'
else
  echo "  Temiz - 53 uzerinde DNAT kurali kalmadi."
fi

S "5. Android'de DNS testi"
W ping -c 2 -W 3 google.com 2>&1 | tail -3
echo "  --- f-droid ---"
W ping -c 2 -W 3 f-droid.org 2>&1 | tail -3

S "6. Ag dogrulandi mi (VALIDATED)"
W dumpsys connectivity 2>&1 | grep -oE "network\{100\}.*Capabilities: [A-Z_&]+" | head -1 | sed 's/^/  /'
W dumpsys connectivity 2>&1 | grep -icE "VALIDATED" | sed 's/^/  VALIDATED gecen satir sayisi: /'

S "7. unwall kendi islevini koruyor mu (host tarafi)"
echo "  dnscrypt-proxy 5300: $(ss -tlnp 2>/dev/null | grep -c 5300) soket"
echo "  nfqws kuyrugu:"; nft list ruleset 2>/dev/null | grep -cE "queue (num|flags)" | sed 's/^/    kural sayisi: /'
echo "  host DNS testi:"; dig +short +time=2 +tries=1 google.com 2>&1 | head -2 | sed 's/^/    /'
