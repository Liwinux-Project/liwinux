#!/usr/bin/env bash
# Is unwall still working? (regression check after GATEWAY_MODE=0)
set -u
[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0"; exit 1; }
S() { echo; echo "=== $* ==="; }

S "1. nft gercekten okunabiliyor mu (onceki testin gecerliligi)"
echo "  total rule lines: $(nft list ruleset 2>/dev/null | wc -l)"
echo "  tables:"; nft list tables 2>&1 | sed 's/^/    /'

S "2. unwall tablosunun TAM icerigi"
nft list table ip unwall 2>&1 | sed 's/^/  /'

S "3. queue / mark rules (diversion to the DPI engine)"
echo "  rules mentioning 'queue num': $(nft list ruleset 2>/dev/null | grep -cE "queue (num|flags)")"
nft list ruleset 2>/dev/null | grep -nE "queue (num|flags)" | sed 's/^/    /'

S "4. The nfqws process and its queue"
pgrep -af nfqws2 | cut -c1-100 | sed 's/^/  /'
echo "  --- kernel NFQUEUE state ---"
cat /proc/net/netfilter/nfnetlink_queue 2>/dev/null | sed 's/^/    /' || echo "    (no queue - no rule sends anything to one)"

S "5. What unwallctl nft-apply says when run by hand"
/usr/local/bin/unwallctl nft-apply 2>&1 | tail -10 | sed 's/^/  /'
echo "  --- queue rule count afterwards: $(nft list ruleset 2>/dev/null | grep -cE "queue (num|flags)") ---"

S "6. unwall status command (if there is one)"
/usr/local/bin/unwallctl status 2>&1 | head -20 | sed 's/^/  /'
