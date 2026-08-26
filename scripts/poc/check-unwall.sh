#!/usr/bin/env bash
# unwall gercekten calisiyor mu? (GATEWAY_MODE=0 sonrasi regresyon kontrolu)
set -u
[ "$(id -u)" = 0 ] || { echo "root gerekiyor: sudo bash $0"; exit 1; }
S() { echo; echo "=== $* ==="; }

S "1. nft gercekten okunabiliyor mu (onceki testin gecerliligi)"
echo "  toplam kural satiri: $(nft list ruleset 2>/dev/null | wc -l)"
echo "  tablolar:"; nft list tables 2>&1 | sed 's/^/    /'

S "2. unwall tablosunun TAM icerigi"
nft list table ip unwall 2>&1 | sed 's/^/  /'

S "3. queue / mark kurallari (DPI motoruna yonlendirme)"
echo "  'queue num' gecen kural: $(nft list ruleset 2>/dev/null | grep -cE "queue (num|flags)")"
nft list ruleset 2>/dev/null | grep -nE "queue (num|flags)" | sed 's/^/    /'

S "4. nfqws sureci ve kuyrugu"
pgrep -af nfqws2 | cut -c1-100 | sed 's/^/  /'
echo "  --- kernel NFQUEUE durumu ---"
cat /proc/net/netfilter/nfnetlink_queue 2>/dev/null | sed 's/^/    /' || echo "    (kuyruk yok - hicbir kural queue'ya yollamiyor)"

S "5. unwallctl nft-apply elle calistirilinca ne diyor"
/usr/local/bin/unwallctl nft-apply 2>&1 | tail -10 | sed 's/^/  /'
echo "  --- sonra queue kural sayisi: $(nft list ruleset 2>/dev/null | grep -cE "queue (num|flags)") ---"

S "6. unwall durum komutu (varsa)"
/usr/local/bin/unwallctl status 2>&1 | head -20 | sed 's/^/  /'
