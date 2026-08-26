#!/usr/bin/env bash
# DNS paketi hangi zincirde oluyor? ufw sayaclarini oku.
set -u
[ "$(id -u)" = 0 ] || { echo "root: sudo bash $0"; exit 1; }
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }

echo "=== 1. ufw kural listesi ==="
ufw status verbose 2>/dev/null | sed 's/^/  /'

echo
echo "=== 2. Sayaclari sifirla ve DNS tetikle ==="
iptables -Z 2>/dev/null
W ping -c 1 -W 3 google.com  >/dev/null 2>&1
W ping -c 1 -W 3 f-droid.org >/dev/null 2>&1
sleep 2

echo
echo "=== 3. ufw-user-input (bizim 53 kuralimiz eslesti mi) ==="
iptables -L ufw-user-input -n -v --line-numbers 2>/dev/null | sed 's/^/  /'

echo
echo "=== 4. ufw-before-input ==="
iptables -L ufw-before-input -n -v 2>/dev/null | grep -viE "^Chain|pkts" | awk '$1!="0"' | head -15 | sed 's/^/  /'

echo
echo "=== 5. REJECT zincirleri (ECONNREFUSED buradan gelir) ==="
for c in ufw-reject-input ufw-after-input ufw-after-logging-input; do
  echo "  --- $c ---"
  iptables -L "$c" -n -v 2>/dev/null | grep -viE "^Chain|pkts" | awk '$1!="0"' | sed 's/^/    /'
done

echo
echo "=== 6. INPUT zinciri genel ==="
iptables -L INPUT -n -v 2>/dev/null | head -12 | sed 's/^/  /'

echo
echo "=== 7. nft: 53 iceren TUM kurallar (ufw disi tablolar dahil) ==="
nft list ruleset 2>/dev/null | grep -nE "dport (53|\{ ?53|.*53 ?\})" | head -20 | sed 's/^/  /'

echo
echo "=== 8. Diger tablolar hangi hooklarda? (cakisma avi) ==="
nft list tables 2>/dev/null | sed 's/^/  /'
