#!/usr/bin/env bash
# liwd-helper + polkit dogrulama (ROOT OLMADAN calistir!)
set -u
[ "$(id -u)" != 0 ] && echo "iyi: normal kullanici olarak calisiyor" || \
  { echo "UYARI: root olarak calistirma - polkit testi anlamsizlasir"; }
D=id.liwinux.Helper1; P=/id/liwinux/Helper1
S() { echo; echo "=== $* ==="; }

S "1. Servis ve veri yolu kaydi"
systemctl is-active liwd-helper 2>&1 | sed 's/^/  servis: /'
busctl list 2>/dev/null | grep -i liwinux | sed 's/^/  /' || echo "  veri yolunda YOK"

S "2. polkit eylemleri tanindi mi"
pkaction --action-id id.liwinux.helper.read-property 2>&1 | head -6 | sed 's/^/  /'
echo "  --- net-repair varsayilani ---"
pkaction --action-id id.liwinux.helper.net-repair --verbose 2>&1 | grep -iE "implicit" | sed 's/^/  /'

S "3. GetProp (salt okunur, izin verilmeli)"
busctl --system call $D $P $D GetProp s "sys.boot_completed" 2>&1 | sed 's/^/  /'

S "4. BootCompleted"
busctl --system call $D $P $D BootCompleted 2>&1 | sed 's/^/  /'

S "5. GECERSIZ anahtar reddediliyor mu (enjeksiyon testi)"
for k in 'a; id' 'a`id`' 'a$(id)' 'a b'; do
  r=$(busctl --system call $D $P $D GetProp s "$k" 2>&1 | head -1)
  echo "  $k  ->  $r"
done

S "6. NetDiagnose (salt okunur teshis)"
busctl --system call $D $P $D NetDiagnose 2>&1 | head -3 | sed 's/^/  /'

S "7. NetRepair (YONETICI yetkisi istemeli - parola sorulmali)"
echo "  not: bu cagri polkit parola istemi acabilir; iptal edersen"
echo "        AccessDenied donmeli - dogru davranis budur."
timeout 25 busctl --system call $D $P $D NetRepair 2>&1 | head -4 | sed 's/^/  /'
