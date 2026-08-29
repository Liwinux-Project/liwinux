#!/usr/bin/env bash
# Call CheckAuthorization BY HAND with busctl and verify the signature.
# Helper'in yaptigi cagrinin aynisini yapar; boylece hatanin kodda mi
# yoksa cagri bicminde mi oldugu ayrilir.
set -u
ME=$(busctl --system --list --no-legend 2>/dev/null | head -1 >/dev/null; echo)
A=org.freedesktop.PolicyKit1
P=/org/freedesktop/PolicyKit1/Authority
I=org.freedesktop.PolicyKit1.Authority

echo "=== with a unix-process subject (what pkcheck does) ==="
START=$(awk '{print $22}' /proc/$$/stat)
busctl --system call $A $P $I CheckAuthorization \
  "(sa{sv})sa{ss}us" "unix-process" 2 pid u $$ start-time t "$START" \
  id.liwinux.helper.read-property 0 0 "" 2>&1 | sed 's/^/  /'

echo
echo "=== with a system-bus-name subject (what the helper does) ==="
echo "  note: busctl uses its own unique name"
busctl --system call $A $P $I CheckAuthorization \
  "(sa{sv})sa{ss}us" "system-bus-name" 1 name s ":1.999999" \
  id.liwinux.helper.read-property 0 0 "" 2>&1 | sed 's/^/  /'
