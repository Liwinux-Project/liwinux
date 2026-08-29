#!/usr/bin/env bash
R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# polkit reddi mi, polkit'e ulasamama mi? Kesin ayrim.
set -u
[ "$(id -u)" = 0 ] && echo "WARNING: do not run this as root!" 
D=id.liwinux.Helper1; P=/id/liwinux/Helper1
S() { echo; echo "=== $* ==="; }

S "0. Install the new build and restart"
sudo install -m755 "$R/target/release/liwd-helper" /usr/local/bin/liwd-helper
sudo systemctl set-environment LIWD_LOG=debug 2>/dev/null || true
sudo systemctl restart liwd-helper
sudo systemctl set-environment LIWD_LOG=debug 2>/dev/null
sleep 2; systemctl is-active liwd-helper

S "1. What polkit says DIRECTLY (helper out of the picture)"
echo "  --- read-property for my own process ---"
pkcheck --action-id id.liwinux.helper.read-property --process $$ 2>&1 | sed 's/^/    /'
echo "    exit code: $?"
echo "  --- net-repair (yonetici istemeli) ---"
pkcheck --action-id id.liwinux.helper.net-repair --process $$ 2>&1 | sed 's/^/    /'
echo "    exit code: $?"

S "2. Does polkit consider my session ACTIVE"
loginctl show-session "$(loginctl show-user "$USER" -p Display --value)" \
  -p Active -p Remote -p Type -p State 2>&1 | sed 's/^/  /'
echo "  --- all sessions ---"; loginctl list-sessions 2>&1 | sed 's/^/  /'

S "3. Retry the call (the build with logging)"
busctl --system call $D $P $D GetProp s "sys.boot_completed" 2>&1 | sed 's/^/  /'

S "4. HELPER LOGLARI (asil kanit)"
sudo journalctl -u liwd-helper -n 25 --no-pager 2>&1 | tail -20 | sed 's/^/  /'

S "5. polkit servis loglari"
sudo journalctl -u polkit -n 15 --no-pager 2>&1 | tail -12 | sed 's/^/  /'

S "6. is polkit running and does it see the actions"
systemctl is-active polkit 2>&1 | sed 's/^/  polkit: /'
pkaction 2>/dev/null | grep -c "^id.liwinux" | sed 's/^/  taninan liwinux eylemi: /'
