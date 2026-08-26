#!/usr/bin/env bash
# CheckAuthorization'i busctl ile ELLE cagirip imzayi dogrula.
# Helper'in yaptigi cagrinin aynisini yapar; boylece hatanin kodda mi
# yoksa cagri bicminde mi oldugu ayrilir.
set -u
ME=$(busctl --system --list --no-legend 2>/dev/null | head -1 >/dev/null; echo)
A=org.freedesktop.PolicyKit1
P=/org/freedesktop/PolicyKit1/Authority
I=org.freedesktop.PolicyKit1.Authority

echo "=== unix-process oznesi ile (pkcheck'in yaptigi) ==="
START=$(awk '{print $22}' /proc/$$/stat)
busctl --system call $A $P $I CheckAuthorization \
  "(sa{sv})sa{ss}us" "unix-process" 2 pid u $$ start-time t "$START" \
  id.liwinux.helper.read-property 0 0 "" 2>&1 | sed 's/^/  /'

echo
echo "=== system-bus-name oznesi ile (helper'in yaptigi) ==="
echo "  not: busctl kendi benzersiz adini kullanir"
busctl --system call $A $P $I CheckAuthorization \
  "(sa{sv})sa{ss}us" "system-bus-name" 1 name s ":1.999999" \
  id.liwinux.helper.read-property 0 0 "" 2>&1 | sed 's/^/  /'
