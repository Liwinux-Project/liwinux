#!/usr/bin/env bash
# Turn off unwall gateway mode so the DNS hijack on waydroid0 goes away
set -u
[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0"; exit 1; }
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Current setting, and a backup"
grep -n "GATEWAY_MODE" /etc/unwall/unwall.conf
cp -a /etc/unwall/unwall.conf "/etc/unwall/unwall.conf.bak-$(date +%s)"
echo "  backed up: /etc/unwall/unwall.conf.bak-*"

S "2. Setting GATEWAY_MODE=0"
sed -i 's/^GATEWAY_MODE=.*/GATEWAY_MODE=0/' /etc/unwall/unwall.conf
grep -n "GATEWAY_MODE" /etc/unwall/unwall.conf

S "3. Restarting unwall"
systemctl restart unwall.service
sleep 3
systemctl is-active unwall.service
systemctl status unwall.service --no-pager -n 3 2>&1 | tail -4

S "4. Is the DNS hijack rule GONE? (CRITICAL)"
if nft list ruleset 2>/dev/null | grep -q "dnat to .*:53"; then
  echo "  STILL THERE:"; nft list ruleset 2>/dev/null | grep -n "dnat to .*:53" | sed 's/^/    /'
else
  echo "  Clean - no DNAT rule left on port 53."
fi

S "5. DNS test inside Android"
W ping -c 2 -W 3 google.com 2>&1 | tail -3
echo "  --- f-droid ---"
W ping -c 2 -W 3 f-droid.org 2>&1 | tail -3

S "6. Has the network been VALIDATED"
W dumpsys connectivity 2>&1 | grep -oE "network\{100\}.*Capabilities: [A-Z_&]+" | head -1 | sed 's/^/  /'
W dumpsys connectivity 2>&1 | grep -icE "VALIDATED" | sed 's/^/  lines mentioning VALIDATED: /'

S "7. Is unwall still doing its own job (host side)"
echo "  dnscrypt-proxy 5300: $(ss -tlnp 2>/dev/null | grep -c 5300) sockets"
echo "  nfqws queue:"; nft list ruleset 2>/dev/null | grep -cE "queue (num|flags)" | sed 's/^/    rule count: /'
echo "  host DNS test:"; dig +short +time=2 +tries=1 google.com 2>&1 | head -2 | sed 's/^/    /'
