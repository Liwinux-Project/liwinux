#!/usr/bin/env bash
# NetworkMonitor cokmesi + baglanti kaybi teshisi (GAPPS imaji)
set -u
[ "$(id -u)" = 0 ] || { echo "root: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. NetworkMonitor cokme izi (BASLIKTAN)"
L logcat -b crash -d | grep -B2 -A30 "NetworkMonitor" | head -50

S "2. Tum FATAL EXCEPTION basliklari"
L logcat -b crash -d | grep -E "FATAL EXCEPTION|Process:|^.*AndroidRuntime: (java|android|kotlin)\.[a-zA-Z.]*Exception|Caused by" | tail -25

S "3. Baglanti gercekten kopuk mu (ham cikti)"
echo "  --- ping 8.8.8.8 ---"
L ping -c 3 -W 3 8.8.8.8 2>&1 | tail -6
echo "  --- ping 1.1.1.1 ---"
L ping -c 2 -W 3 1.1.1.1 2>&1 | tail -4
echo "  --- rota ---"
L ip route 2>&1
echo "  --- eth0 ---"
L ip -4 addr show eth0 2>&1 | grep inet

S "4. Host tarafinda paket akiyor mu (nft sayaci)"
nft delete table inet liwdiag2 2>/dev/null
nft -f - <<NFT
add table inet liwdiag2
add chain inet liwdiag2 pre { type filter hook prerouting priority -300 ; }
add rule inet liwdiag2 pre iifname "waydroid0" ip daddr 8.8.8.8 counter comment "icmp_out"
add rule inet liwdiag2 pre iifname "waydroid0" udp dport 53 counter comment "dns_out"
NFT
L ping -c 2 -W 2 8.8.8.8 >/dev/null 2>&1
L ping -c 1 -W 2 google.com >/dev/null 2>&1
sleep 2
nft list table inet liwdiag2 | grep counter | sed 's/^/  /'
nft delete table inet liwdiag2 2>/dev/null

S "5. Android firewall/netd durumu (GAPPS'te farkli olabilir)"
L dumpsys netd 2>&1 | head -20
echo "  --- iptables Android icinde ---"
L iptables -L -n 2>&1 | head -15

S "6. Captive portal ayarlari"
for k in captive_portal_mode captive_portal_detection_enabled captive_portal_http_url captive_portal_use_https; do
  printf "  %-36s = %s\n" "$k" "$(L settings get global $k | tail -1)"
done

S "7. Paket dogrulama: NetworkStack modulu var mi"
L pm list packages 2>/dev/null | grep -iE "networkstack|captiveportal|conscrypt|tethering" | sed 's/^/  /'
