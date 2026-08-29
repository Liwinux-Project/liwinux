#!/usr/bin/env bash
# liw net doctor - diagnose and repair Waydroid networking (liwinux prototype)
#
#   bash liw-net-doctor.sh            # diagnose only, changes nothing
#   sudo bash liw-net-doctor.sh apply # repair, matched to the firewall found
#   sudo bash liw-net-doctor.sh revert
#
# Waydroid installs its own NAT and accept rules into the nftables table
# "lxc" (waydroid-net.sh). The catch: if the host firewall has its own base
# chain on the same hook and that chain says DROP, Waydroid's accept CANNOT
# override it. In netfilter every base chain on a hook runs, and one drop is
# enough to kill the packet. So the fix belongs to the firewall, not to
# Waydroid.

set -u
MODE="${1:-check}"
BRIDGE=waydroid0
TAG="liwinux-waydroid"
ok=0; warn=0; err=0; rules_ok=0; lease_ok=0; net_ok=0; dns_ok=0; val_ok=0
say()  { printf '  %s\n' "$*"; }
OK()   { printf '  \033[32mOK\033[0m   %s\n' "$*"; ok=$((ok+1)); }
WARN() { printf '  \033[33mWARN\033[0m  %s\n' "$*"; warn=$((warn+1)); }
ERR()  { printf '  \033[31mERROR\033[0m %s\n' "$*"; err=$((err+1)); }
H()    { printf '\n== %s ==\n' "$*"; }
need_root() { [ "$(id -u)" = 0 ] || { echo "this mode needs root: sudo bash $0 $MODE"; exit 1; }; }

# ---------- detection ----------
detect_fw() {
  if systemctl is-active --quiet ufw 2>/dev/null; then echo ufw
  elif systemctl is-active --quiet firewalld 2>/dev/null; then echo firewalld
  elif systemctl is-active --quiet nftables 2>/dev/null; then echo nftables
  elif systemctl is-active --quiet iptables 2>/dev/null; then echo iptables
  else echo none; fi
}
WAN=$(ip route show default 2>/dev/null | awk '/^default/{print $5; exit}')
SUBNET=$(ip -4 -o addr show "$BRIDGE" 2>/dev/null | awk '{print $4}')
FW=$(detect_fw)

# ---------- diagnosis ----------
diagnose() {
  H "1. Waydroid network plumbing"
  [ -d "/sys/class/net/$BRIDGE" ] && OK "$BRIDGE bridge exists (${SUBNET:-no address})" || ERR "$BRIDGE bridge is MISSING"
  [ -f /run/waydroid-lxc/network_up ] && OK "waydroid-net.sh ran" \
    || ERR "waydroid-net.sh did not run (the container service may be down)"
  pgrep -f "dnsmasq.*$BRIDGE" >/dev/null && OK "dnsmasq is running (DHCP+DNS)" || ERR "dnsmasq is not running"
  [ "$(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null)" = 1 ] && OK "ip_forward is on" || ERR "ip_forward is off"
  say "WAN interface: ${WAN:-not detected}"

  H "2. Waydroid's own nftables rules"
  if [ "$(id -u)" != 0 ]; then
    WARN "not root - nftables cannot be read (for a full check: sudo bash $0)"
  elif command -v nft >/dev/null; then
    if nft list table ip lxc >/dev/null 2>&1; then
      nft list table ip lxc 2>/dev/null | grep -q masquerade \
        && OK "NAT/masquerade is in place (table ip lxc)" || ERR "table ip lxc exists but has no masquerade"
    else ERR "table ip lxc is MISSING - waydroid-net.sh could not install NAT"; fi
    nft list table inet lxc >/dev/null 2>&1 \
      && OK "input/forward accept rules are in place (table inet lxc)" \
      || WARN "table inet lxc is absent"
  fi

  H "3. Host firewall"
  case "$FW" in
    none) OK "no active firewall - Waydroid's own rules should be enough"; rules_ok=1 ;;
    ufw)
      WARN "ufw is active"
      if [ "$(id -u)" = 0 ]; then
        if ufw status 2>/dev/null | grep -q "$BRIDGE"; then
          OK "ufw has rules for $BRIDGE"; rules_ok=1
        else ERR "ufw has NO rule for $BRIDGE -> traffic is dropped"; fi
        grep -q 'DEFAULT_FORWARD_POLICY="DROP"' /etc/default/ufw 2>/dev/null \
          && say "FORWARD policy: DROP (expected; the rules below get past it)"
      else WARN "root is needed for the detail"; fi ;;
    firewalld)
      WARN "firewalld is active"
      if command -v firewall-cmd >/dev/null && [ "$(id -u)" = 0 ]; then
        firewall-cmd --get-zone-of-interface="$BRIDGE" >/dev/null 2>&1 \
          && OK "$BRIDGE is assigned to a zone: $(firewall-cmd --get-zone-of-interface=$BRIDGE 2>/dev/null)" \
          || ERR "$BRIDGE is in no zone at all"
      fi ;;
    nftables|iptables) WARN "the $FW service is active - its own DROP policy can block Waydroid" ;;
  esac

  H "4. What the container actually got"
  local lease="/var/lib/misc/dnsmasq.$BRIDGE.leases"
  if [ -s "$lease" ]; then OK "a DHCP lease was handed out:"; lease_ok=1; sed 's/^/       /' "$lease"
  else ERR "no DHCP lease - the container never got an IP"; fi
  if command -v waydroid >/dev/null; then
    local ip; ip=$(waydroid status 2>/dev/null | awk -F'\t' '/IP address/{print $2}')
    [ -n "${ip:-}" ] && [ "$ip" != "UNKNOWN" ] && OK "waydroid IP: $ip" || ERR "waydroid IP: UNKNOWN"
  fi

  H "5. Reaching the internet"
  net_ok=0; dns_ok=0
  # 5a: is dnsmasq working, asked from the host - independent of Android,
  # which makes it the trustworthy half of this test
  if command -v dig >/dev/null 2>&1; then
    if [ -n "$(dig +short +time=3 +tries=1 @192.168.240.1 google.com 2>/dev/null)" ]; then
      OK "dnsmasq resolves names (confirmed from the host)"; dns_ok=1
    else ERR "dnsmasq cannot resolve - check its upstream DNS"; fi
  else WARN "dig is not installed - dnsmasq test skipped"; fi
  # 5b: can the container get out
  if [ "$(id -u)" = 0 ] && command -v waydroid >/dev/null \
     && waydroid status 2>/dev/null | grep -qi "Session.*RUNNING"; then
    r=$(waydroid --details-to-stdout shell -- ping -c 2 -W 3 8.8.8.8 2>/dev/null | tr -d '\r' | grep -cE "bytes from")
    if [ "${r:-0}" -gt 0 ]; then OK "the container can reach the outside (8.8.8.8)"; net_ok=1
    else ERR "the container cannot reach the outside"; fi
    # 5c: the DNS server Android was configured with
    d=$(waydroid --details-to-stdout shell -- getprop net.dns1 2>/dev/null | tr -d '\r' | tail -1)
    [ -n "${d:-}" ] && say "Android net.dns1 = $d" \
      || say "net.dns1 is empty (normal on Android 9+; the resolver lives in netd)"
    # NOTE: "ping <name>" from the shell can behave differently from an app
    # because netd routes per UID. A failure here is not proof on its own.
  else WARN "needs root and a running session - container test skipped"; fi

  H "5b. Android network VALIDATION (required by Google apps)"
  val_ok=0
  if [ "$(id -u)" = 0 ] && command -v waydroid >/dev/null \
     && waydroid status 2>/dev/null | grep -qi "Session.*RUNNING"; then
    caps=$(waydroid --details-to-stdout shell -- dumpsys connectivity 2>/dev/null | tr -d '\r' \
           | grep -oE "Capabilities: [A-Z_&]+" | head -1)
    echo "  $caps"
    if echo "$caps" | grep -q "VALIDATED"; then
      OK "the network is VALIDATED - Google apps can work"; val_ok=1
    else
      ERR "the network is NOT VALIDATED -> Play Store says 'no network connection'"
      say "Android could not confirm for itself that there is internet."
      say "The validation probe uses connectivitycheck.gstatic.com / www.google.com"
      d1=$(waydroid --details-to-stdout shell -- ping -c 1 -W 3 connectivitycheck.gstatic.com 2>/dev/null | tr -d '\r' | grep -cE "bytes from")
      [ "${d1:-0}" -gt 0 ] && say "  -> the probe address IS reachable (validation will retry)" \
                           || say "  -> the probe address is NOT reachable (this is the real problem)"
      say "To force a retry: waydroid session stop && waydroid session start"
    fi
    echo "  --- the last validation attempt ---"
    waydroid --details-to-stdout shell -- logcat -d 2>/dev/null | tr -d '\r' \
      | grep -E "NetworkMonitor|validation" | tail -6 | sed 's/^/    /'
  else WARN "needs root and a running session - skipped"; fi

  H "SUMMARY"
  printf '  ok=%d  warnings=%d  errors=%d\n' "$ok" "$warn" "$err"
  if [ "$err" -gt 0 ]; then
    if [ "$rules_ok" = 1 ] && [ "$lease_ok" = 0 ]; then
      say "The firewall rules are IN PLACE but the container has no IP."
      say "Cause: Android tried DHCP BEFORE the rules existed and gave up."
      say "Fix:    waydroid session stop && waydroid session start"
    elif [ "$lease_ok" = 1 ] && [ "$net_ok" = 0 ]; then
      say "There is an IP but no way out -> inspect the masquerade/forward chain."
    elif [ "$dns_ok" = 0 ]; then
      say "Packets get out but names do not resolve -> dnsmasq upstream (/etc/resolv.conf)."
    elif [ "$dns_ok" = 1 ] && [ "$net_ok" = 1 ] && [ "$val_ok" = 0 ]; then
      say "The network works but Android has not validated it. Restart the session:"
      say "  waydroid session stop && waydroid session start"
    elif [ "$rules_ok" = 0 ]; then
      say "To repair:  sudo bash $0 apply"
    else
      say "For the remaining errors, read the sections above."
    fi
  else say "The network looks healthy."; fi
}

# ---------- repair ----------
apply_ufw() {
  say "adding ufw rules (only 53/67 plus forwarding; no other host port is opened)"
  ufw allow in on "$BRIDGE" to any port 67 proto udp comment "$TAG dhcp"
  ufw allow in on "$BRIDGE" to any port 53             comment "$TAG dns"
  ufw route allow in on "$BRIDGE"                      comment "$TAG outbound"
  ufw reload
  say "NOTE: no masquerade was added - Waydroid installs that itself."
}
revert_ufw() {
  say "removing the ufw rules"
  while ufw status numbered 2>/dev/null | grep -q "$TAG"; do
    n=$(ufw status numbered 2>/dev/null | grep "$TAG" | head -1 | sed 's/^\[ *\([0-9]*\).*/\1/')
    [ -n "$n" ] || break
    yes | ufw delete "$n" >/dev/null
  done
  ufw reload
}
apply_firewalld() {
  say "moving $BRIDGE into the trusted zone, plus masquerade"
  firewall-cmd --permanent --zone=trusted --change-interface="$BRIDGE"
  [ -n "$WAN" ] && firewall-cmd --permanent --zone=public --add-masquerade
  firewall-cmd --reload
}
revert_firewalld() {
  firewall-cmd --permanent --zone=trusted --remove-interface="$BRIDGE" 2>/dev/null
  firewall-cmd --reload
}
apply_nft() {
  say "nftables: adding a higher-priority accept table for $BRIDGE"
  nft -f - <<NFT
add table inet ${TAG}
flush table inet ${TAG}
add chain inet ${TAG} input   { type filter hook input   priority -10 ; }
add chain inet ${TAG} forward { type filter hook forward priority -10 ; }
add rule inet ${TAG} input   iifname "${BRIDGE}" udp dport { 53, 67 } accept
add rule inet ${TAG} input   iifname "${BRIDGE}" tcp dport { 53, 67 } accept
add rule inet ${TAG} forward iifname "${BRIDGE}" accept
add rule inet ${TAG} forward oifname "${BRIDGE}" accept
NFT
  say "WARNING: base chains are independent; if another table says DROP this is not enough."
  say "To make it survive a reboot, add it to your distribution nftables.conf."
}
revert_nft() { nft delete table inet "$TAG" 2>/dev/null; }

case "$MODE" in
  check) diagnose ;;
  apply)
    need_root
    H "Firewall detected: $FW"
    case "$FW" in
      ufw) apply_ufw ;; firewalld) apply_firewalld ;;
      nftables|iptables) apply_nft ;;
      none) say "no active firewall - there is nothing to apply."; say "If the problem persists: sudo systemctl restart waydroid-container" ;;
    esac
    H "Diagnosis after repair"; ok=0; warn=0; err=0; rules_ok=0; lease_ok=0; net_ok=0; dns_ok=0; val_ok=0; diagnose ;;
  revert)
    need_root
    case "$FW" in ufw) revert_ufw ;; firewalld) revert_firewalld ;; nftables|iptables) revert_nft ;;
      none) say "nothing to do" ;; esac ;;
  *) echo "Usage: bash $0 [check|apply|revert]"; exit 2 ;;
esac
