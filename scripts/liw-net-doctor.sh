#!/usr/bin/env bash
# liw net doctor — Waydroid ag teshisi ve onarimi (liwinux prototipi)
#
#   bash liw-net-doctor.sh            # teshis, degisiklik yok
#   sudo bash liw-net-doctor.sh apply # tespit edilen firewall'a gore duzelt
#   sudo bash liw-net-doctor.sh revert
#
# Waydroid kendi NAT + accept kurallarini nftables'ta "lxc" tablosuna kurar
# (waydroid-net.sh). Sorun sudur: host firewall'unun kendi base chain'i ayni
# hook'ta DROP derse, Waydroid'in accept'i bunu gecersiz KILAMAZ. netfilter'da
# ayni hook'taki her base chain calisir ve biri drop derse paket olur.
# Bu yuzden cozum firewall'a ozgudur, Waydroid'e degil.

set -u
MODE="${1:-check}"
BRIDGE=waydroid0
TAG="liwinux-waydroid"
ok=0; warn=0; err=0; rules_ok=0; lease_ok=0; net_ok=0; dns_ok=0; val_ok=0
say()  { printf '  %s\n' "$*"; }
OK()   { printf '  \033[32mOK\033[0m   %s\n' "$*"; ok=$((ok+1)); }
WARN() { printf '  \033[33mUYARI\033[0m %s\n' "$*"; warn=$((warn+1)); }
ERR()  { printf '  \033[31mHATA\033[0m %s\n' "$*"; err=$((err+1)); }
H()    { printf '\n== %s ==\n' "$*"; }
need_root() { [ "$(id -u)" = 0 ] || { echo "Bu mod root gerektiriyor: sudo bash $0 $MODE"; exit 1; }; }

# ---------- tespit ----------
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

# ---------- teshis ----------
diagnose() {
  H "1. Waydroid ag altyapisi"
  [ -d "/sys/class/net/$BRIDGE" ] && OK "$BRIDGE koprusu var (${SUBNET:-adres yok})" || ERR "$BRIDGE koprusu YOK"
  [ -f /run/waydroid-lxc/network_up ] && OK "waydroid-net.sh calismis" \
    || ERR "waydroid-net.sh calismamis (konteyner servisi kapali olabilir)"
  pgrep -f "dnsmasq.*$BRIDGE" >/dev/null && OK "dnsmasq calisiyor (DHCP+DNS)" || ERR "dnsmasq calismiyor"
  [ "$(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null)" = 1 ] && OK "ip_forward acik" || ERR "ip_forward kapali"
  say "WAN arayuzu: ${WAN:-tespit edilemedi}"

  H "2. Waydroid'in kendi nftables kurallari"
  if [ "$(id -u)" != 0 ]; then
    WARN "root degilsin - nftables okunamiyor (teshis icin: sudo bash $0)"
  elif command -v nft >/dev/null; then
    if nft list table ip lxc >/dev/null 2>&1; then
      nft list table ip lxc 2>/dev/null | grep -q masquerade \
        && OK "NAT/masquerade kurulu (table ip lxc)" || ERR "table ip lxc var ama masquerade yok"
    else ERR "table ip lxc YOK - waydroid-net.sh NAT kuramamis"; fi
    nft list table inet lxc >/dev/null 2>&1 \
      && OK "input/forward accept kurallari kurulu (table inet lxc)" \
      || WARN "table inet lxc yok"
  fi

  H "3. Host firewall"
  case "$FW" in
    none) OK "Aktif firewall yok - Waydroid kurallari tek basina yeterli olmali"; rules_ok=1 ;;
    ufw)
      WARN "ufw aktif"
      if [ "$(id -u)" = 0 ]; then
        if ufw status 2>/dev/null | grep -q "$BRIDGE"; then
          OK "ufw'de $BRIDGE kurallari var"; rules_ok=1
        else ERR "ufw'de $BRIDGE kurali YOK -> trafik dusuyor"; fi
        grep -q 'DEFAULT_FORWARD_POLICY="DROP"' /etc/default/ufw 2>/dev/null \
          && say "FORWARD politikasi: DROP (beklenen; kural ile asilacak)"
      else WARN "detay icin root gerekiyor"; fi ;;
    firewalld)
      WARN "firewalld aktif"
      if command -v firewall-cmd >/dev/null && [ "$(id -u)" = 0 ]; then
        firewall-cmd --get-zone-of-interface="$BRIDGE" >/dev/null 2>&1 \
          && OK "$BRIDGE bir zone'a atanmis: $(firewall-cmd --get-zone-of-interface=$BRIDGE 2>/dev/null)" \
          || ERR "$BRIDGE hicbir zone'a atanmamis"
      fi ;;
    nftables|iptables) WARN "$FW servisi aktif - kendi DROP politikan Waydroid'i engelleyebilir" ;;
  esac

  H "4. Konteynerin gercek durumu"
  local lease="/var/lib/misc/dnsmasq.$BRIDGE.leases"
  if [ -s "$lease" ]; then OK "DHCP lease verilmis:"; lease_ok=1; sed 's/^/       /' "$lease"
  else ERR "DHCP lease YOK - konteyner IP alamamis"; fi
  if command -v waydroid >/dev/null; then
    local ip; ip=$(waydroid status 2>/dev/null | awk -F'\t' '/IP address/{print $2}')
    [ -n "${ip:-}" ] && [ "$ip" != "UNKNOWN" ] && OK "waydroid IP: $ip" || ERR "waydroid IP: UNKNOWN"
  fi

  H "5. Internet erisimi"
  net_ok=0; dns_ok=0
  # 5a: dnsmasq host tarafindan calisiyor mu (Android'den bagimsiz, guvenilir)
  if command -v dig >/dev/null 2>&1; then
    if [ -n "$(dig +short +time=3 +tries=1 @192.168.240.1 google.com 2>/dev/null)" ]; then
      OK "dnsmasq isim cozumluyor (host'tan dogrulandi)"; dns_ok=1
    else ERR "dnsmasq isim cozumleyemiyor - upstream DNS'ini kontrol et"; fi
  else WARN "dig yok - dnsmasq testi atlandi"; fi
  # 5b: konteynerden cikis
  if [ "$(id -u)" = 0 ] && command -v waydroid >/dev/null \
     && waydroid status 2>/dev/null | grep -qi "Session.*RUNNING"; then
    r=$(waydroid --details-to-stdout shell -- ping -c 2 -W 3 8.8.8.8 2>/dev/null | tr -d '\r' | grep -cE "bytes from")
    if [ "${r:-0}" -gt 0 ]; then OK "konteyner disari cikabiliyor (8.8.8.8)"; net_ok=1
    else ERR "konteyner disari cikamiyor"; fi
    # 5c: Android'in yapilandirilmis DNS sunucusu
    d=$(waydroid --details-to-stdout shell -- getprop net.dns1 2>/dev/null | tr -d '\r' | tail -1)
    [ -n "${d:-}" ] && say "Android net.dns1 = $d" \
      || say "net.dns1 bos (Android 9+ icin normal; resolver netd'de)"
    # NOT: kabuktan "ping <isim>" netd'nin UID bazli yonlendirmesi yuzunden
    # uygulamalardan farkli davranabilir; basarisizligi tek basina kanit sayma.
  else WARN "root+calisan session gerekiyor - konteyner testi atlandi"; fi

  H "5b. Android ag DOGRULAMASI (Google uygulamalari icin sart)"
  val_ok=0
  if [ "$(id -u)" = 0 ] && command -v waydroid >/dev/null \
     && waydroid status 2>/dev/null | grep -qi "Session.*RUNNING"; then
    caps=$(waydroid --details-to-stdout shell -- dumpsys connectivity 2>/dev/null | tr -d '\r' \
           | grep -oE "Capabilities: [A-Z_&]+" | head -1)
    echo "  $caps"
    if echo "$caps" | grep -q "VALIDATED"; then
      OK "ag VALIDATED - Google uygulamalari calisabilir"; val_ok=1
    else
      ERR "ag VALIDATED DEGIL -> Play Store 'no network connection' der"
      say "Android internet var oldugunu dogrulayamamis."
      say "Dogrulama sondasi: connectivitycheck.gstatic.com / www.google.com"
      d1=$(waydroid --details-to-stdout shell -- ping -c 1 -W 3 connectivitycheck.gstatic.com 2>/dev/null | tr -d '\r' | grep -cE "bytes from")
      [ "${d1:-0}" -gt 0 ] && say "  -> sonda adresi ERISILEBILIR (dogrulama tekrar denenecek)" \
                           || say "  -> sonda adresi ERISILEMIYOR (asil sorun burada)"
      say "Zorlamak icin: waydroid session stop && waydroid session start"
    fi
    echo "  --- son dogrulama denemesi ---"
    waydroid --details-to-stdout shell -- logcat -d 2>/dev/null | tr -d '\r' \
      | grep -E "NetworkMonitor|validation" | tail -6 | sed 's/^/    /'
  else WARN "root+calisan session gerekiyor - atlandi"; fi

  H "SONUC"
  printf '  basarili=%d  uyari=%d  hata=%d\n' "$ok" "$warn" "$err"
  if [ "$err" -gt 0 ]; then
    if [ "$rules_ok" = 1 ] && [ "$lease_ok" = 0 ]; then
      say "Firewall kurallari YERINDE ama konteyner IP almamis."
      say "Sebep: Android DHCP'yi kurallar eklenmeden ONCE deneyip vazgecti."
      say "Cozum:  waydroid session stop && waydroid session start"
    elif [ "$lease_ok" = 1 ] && [ "$net_ok" = 0 ]; then
      say "IP var ama disari cikis yok -> masquerade/forward zincirini incele."
    elif [ "$dns_ok" = 0 ]; then
      say "Cikis var ama isim cozumlenmiyor -> dnsmasq upstream'i (/etc/resolv.conf)."
    elif [ "$dns_ok" = 1 ] && [ "$net_ok" = 1 ] && [ "$val_ok" = 0 ]; then
      say "Ag calisiyor ama Android dogrulamamis. Session'i yeniden baslat:"
      say "  waydroid session stop && waydroid session start"
    elif [ "$rules_ok" = 0 ]; then
      say "Onarim icin:  sudo bash $0 apply"
    else
      say "Kalan hatalar icin yukaridaki bolumlere bak."
    fi
  else say "Ag saglikli gorunuyor."; fi
}

# ---------- onarim ----------
apply_ufw() {
  say "ufw kurallari ekleniyor (sadece 53/67 + yonlendirme; host'un diger portlari acilmaz)"
  ufw allow in on "$BRIDGE" to any port 67 proto udp comment "$TAG dhcp"
  ufw allow in on "$BRIDGE" to any port 53             comment "$TAG dns"
  ufw route allow in on "$BRIDGE"                      comment "$TAG outbound"
  ufw reload
  say "NOT: masquerade eklenmedi - Waydroid onu zaten kuruyor."
}
revert_ufw() {
  say "ufw kurallari kaldiriliyor"
  while ufw status numbered 2>/dev/null | grep -q "$TAG"; do
    n=$(ufw status numbered 2>/dev/null | grep "$TAG" | head -1 | sed 's/^\[ *\([0-9]*\).*/\1/')
    [ -n "$n" ] || break
    yes | ufw delete "$n" >/dev/null
  done
  ufw reload
}
apply_firewalld() {
  say "$BRIDGE trusted zone'a aliniyor + masquerade"
  firewall-cmd --permanent --zone=trusted --change-interface="$BRIDGE"
  [ -n "$WAN" ] && firewall-cmd --permanent --zone=public --add-masquerade
  firewall-cmd --reload
}
revert_firewalld() {
  firewall-cmd --permanent --zone=trusted --remove-interface="$BRIDGE" 2>/dev/null
  firewall-cmd --reload
}
apply_nft() {
  say "nftables: $BRIDGE icin oncelikli accept tablosu ekleniyor"
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
  say "UYARI: base chain'ler bagimsizdir; baska bir tablo DROP diyorsa bu yetmez."
  say "Kalici olmasi icin dagitiminin nftables.conf dosyasina eklemelisin."
}
revert_nft() { nft delete table inet "$TAG" 2>/dev/null; }

case "$MODE" in
  check) diagnose ;;
  apply)
    need_root
    H "Tespit edilen firewall: $FW"
    case "$FW" in
      ufw) apply_ufw ;; firewalld) apply_firewalld ;;
      nftables|iptables) apply_nft ;;
      none) say "Aktif firewall yok - uygulanacak kural yok."; say "Sorun devam ediyorsa: sudo systemctl restart waydroid-container" ;;
    esac
    H "Onarim sonrasi teshis"; ok=0; warn=0; err=0; rules_ok=0; lease_ok=0; net_ok=0; dns_ok=0; val_ok=0; diagnose ;;
  revert)
    need_root
    case "$FW" in ufw) revert_ufw ;; firewalld) revert_firewalld ;; nftables|iptables) revert_nft ;;
      none) say "yapilacak bir sey yok" ;; esac ;;
  *) echo "Kullanim: bash $0 [check|apply|revert]"; exit 2 ;;
esac
