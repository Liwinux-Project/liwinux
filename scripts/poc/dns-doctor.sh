#!/usr/bin/env bash
# Waydroid DNS diagnosis, from the Android side
set -u
W() { waydroid --details-to-stdout shell -- "$@" 2>&1 | tr -d '\r' | grep -vE "% lxc-info|^\[.*\] RUNNING$"; }
S() { echo; echo "=== $* ==="; }
[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0"; exit 1; }

S "1. Android ag arayuzu"
W ip addr show eth0
W ip route

S "2. Android'in DNS ayarlari"
for p in net.dns1 net.dns2 net.eth0.dns1 dhcp.eth0.dns1 dhcp.eth0.gateway dhcp.eth0.ipaddress; do
  printf "  %-24s = %s\n" "$p" "$(W getprop $p | tail -1)"
done

S "3. Private DNS modu (DoT captive portal'i bozabilir)"
echo "  private_dns_mode      = $(W settings get global private_dns_mode | tail -1)"
echo "  private_dns_specifier = $(W settings get global private_dns_specifier | tail -1)"
echo "  captive_portal_mode   = $(W settings get global captive_portal_mode | tail -1)"

S "4. netd resolver yapilandirmasi"
W ndc resolver getresolverinfo 2>&1 | head -20

S "5. Validation state of the networks"
W dumpsys connectivity 2>&1 | grep -iE "NetworkAgentInfo|Validated|VALIDATED|everValidated|captive" | head -15

S "6. Direct DNS test (from inside the container)"
echo "  --- UDP/53 to 192.168.240.1 ---"
W sh -c 'echo -n "" >/dev/udp/192.168.240.1/53 2>&1 && echo "UDP 53 could be opened" || echo "UDP 53 COULD NOT be opened"'
echo "  --- name resolution ---"
W ping -c 2 -W 3 google.com 2>&1 | tail -3
echo "  --- by IP ---"
W ping -c 2 -W 3 8.8.8.8 2>&1 | tail -3

S "7. dnsmasq host tarafinda sorgu goruyor mu"
echo "  (compare the counts before and after the test below)"
journalctl -u waydroid-container --since "5 min ago" --no-pager 2>/dev/null | grep -i dnsmasq | tail -5
echo "  dnsmasq PID: $(pgrep -f 'dnsmasq.*waydroid0')"
