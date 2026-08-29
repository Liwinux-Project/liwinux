#!/usr/bin/env bash
# Are DNS packets really flowing? A definite test using nft counters (no tcpdump needed)
set -u
[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0"; exit 1; }
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
T=liwdiag
CIP=$(waydroid status 2>/dev/null | awk -F'\t' '/IP address/{print $2}')
echo "Konteyner IP: ${CIP:-bilinmiyor}"

cleanup() { nft delete table inet $T 2>/dev/null; }
trap cleanup EXIT

# Sadece SAYAR, hicbir verdict vermez -> trafigi etkilemez.
nft -f - <<NFT
add table inet $T
flush table inet $T
add chain inet $T pre  { type filter hook prerouting  priority -300 ; }
add chain inet $T post { type filter hook postrouting priority -300 ; }
add rule inet $T pre  iifname "waydroid0" udp dport 53 counter comment "q_in"
add rule inet $T pre  iifname "waydroid0" tcp dport 53 counter comment "q_in_tcp"
add rule inet $T post oifname "waydroid0" udp sport 53 counter comment "r_out"
add rule inet $T pre  iifname "waydroid0" ip daddr != 192.168.240.0/24 counter comment "wan_out"
NFT

echo "=== DNS tetikleniyor ==="
W ping -c 1 -W 3 google.com  >/dev/null 2>&1
W ping -c 1 -W 3 f-droid.org >/dev/null 2>&1
W ping -c 1 -W 2 8.8.8.8     >/dev/null 2>&1
sleep 3

echo
echo "=== COUNTERS ==="
nft list table inet $T | grep -E "counter packets" | sed 's/^/  /'
get() { nft list table inet $T | grep "\"$1\"" | grep -oE "packets [0-9]+" | awk '{print $2}'; }
Q=$(get q_in); QT=$(get q_in_tcp); R=$(get r_out); WAN=$(get wan_out)

echo
echo "=== DIAGNOSIS ==="
echo "  Android'den cikan DNS sorgusu (udp): ${Q:-0}   (tcp: ${QT:-0})"
echo "  dnsmasq'ten donen cevap            : ${R:-0}"
echo "  packets from Android towards the WAN : ${WAN:-0}"
echo
if [ "${Q:-0}" = 0 ] && [ "${QT:-0}" = 0 ]; then
  echo "  >> Android DNS sorgusunu HIC URETMIYOR."
  echo "     Firewall masum. Sorun Android'in resolver'inda (netd)."
  echo "     Likely: the network is not VALIDATED, so apps are given no network."
elif [ "${R:-0}" = 0 ]; then
  echo "  >> Sorgu cikiyor ama CEVAP DONMUYOR -> ufw INPUT veya dnsmasq."
else
  echo "  >> DNS is flowing. The problem is not resolution, it is Android validation."
fi

echo
echo "=== Captive portal / validation ayarlari ==="
for k in captive_portal_mode captive_portal_detection_enabled captive_portal_http_url captive_portal_https_url; do
  printf "  %-36s = %s\n" "$k" "$(W settings get global $k | tail -1)"
done
echo
echo "=== Son DNS/validation loglari ==="
waydroid --details-to-stdout shell -- logcat -d 2>/dev/null | tr -d '\r' \
  | grep -iE "NetworkMonitor|resolv|dns|validation|captive" | tail -20
