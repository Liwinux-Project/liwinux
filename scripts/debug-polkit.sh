#!/usr/bin/env bash
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# polkit reddi mi, polkit'e ulasamama mi? Kesin ayrim.
set -u
[ "$(id -u)" = 0 ] && echo "UYARI: root olarak calistirma!" 
D=id.liwinux.Helper1; P=/id/liwinux/Helper1
S() { echo; echo "=== $* ==="; }

S "0. Yeni surumu kur ve yeniden baslat"
sudo install -m755 "$R/target/release/liwd-helper" /usr/local/bin/liwd-helper
sudo systemctl set-environment LIWD_LOG=debug 2>/dev/null || true
sudo systemctl restart liwd-helper
sudo systemctl set-environment LIWD_LOG=debug 2>/dev/null
sleep 2; systemctl is-active liwd-helper

S "1. polkit DOGRUDAN ne diyor (helper devre disi)"
echo "  --- kendi surecim icin read-property ---"
pkcheck --action-id id.liwinux.helper.read-property --process $$ 2>&1 | sed 's/^/    /'
echo "    cikis kodu: $?"
echo "  --- net-repair (yonetici istemeli) ---"
pkcheck --action-id id.liwinux.helper.net-repair --process $$ 2>&1 | sed 's/^/    /'
echo "    cikis kodu: $?"

S "2. Oturumum polkit'e AKTIF gorunuyor mu"
loginctl show-session "$(loginctl show-user "$USER" -p Display --value)" \
  -p Active -p Remote -p Type -p State 2>&1 | sed 's/^/  /'
echo "  --- tum oturumlar ---"; loginctl list-sessions 2>&1 | sed 's/^/  /'

S "3. Cagriyi tekrar dene (loglu surum)"
busctl --system call $D $P $D GetProp s "sys.boot_completed" 2>&1 | sed 's/^/  /'

S "4. HELPER LOGLARI (asil kanit)"
sudo journalctl -u liwd-helper -n 25 --no-pager 2>&1 | tail -20 | sed 's/^/  /'

S "5. polkit servis loglari"
sudo journalctl -u polkit -n 15 --no-pager 2>&1 | tail -12 | sed 's/^/  /'

S "6. is polkit running and does it see the actions"
systemctl is-active polkit 2>&1 | sed 's/^/  polkit: /'
pkaction 2>/dev/null | grep -c "^id.liwinux" | sed 's/^/  taninan liwinux eylemi: /'
