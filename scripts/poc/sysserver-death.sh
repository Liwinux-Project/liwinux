#!/usr/bin/env bash
# system_server neden oldu? (GAPPS imaji)
set -u
[ "$(id -u)" = 0 ] || { echo "root: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Watchdog (system_server'i en sik oldure sey)"
L logcat -b main -d | grep -iE "watchdog|WATCHDOG KILLING|Blocked in handler" | tail -20

S "2. lowmemorykiller / OOM"
L logcat -b main -d | grep -iE "lmkd|Low on memory|Killing .*system|am_kill|oom_adj|kill.*perceptible" | tail -20
L logcat -b events -d 2>/dev/null | grep -iE "am_kill|am_proc_died.*system" | tail -10

S "3. Native cokme (tombstone) - system_server'a ait mi"
L ls -la /data/tombstones/ 2>&1 | tail -8
echo "  --- en son tombstone basligi ---"
L sh -c 'ls -t /data/tombstones/tombstone_* 2>/dev/null | head -1 | xargs head -30' 2>&1 | head -35

S "4. system_server yeniden basladi mi, kac kez"
L logcat -b main -d | grep -icE "Entered the Android system server" | sed 's/^/  baslatma sayisi: /'
L logcat -b main -d | grep -iE "Entered the Android system server|Watchdog.*restart|zygote.*died" | tail -6

S "5. Su anki durum"
echo "  system_server PID : $(L pidof system_server | tail -1)"
echo "  uptime            : $(L cat /proc/uptime | tail -1)"
echo "  boot_completed    : $(L getprop sys.boot_completed | tail -1)"
echo "  --- Android bellek ---"
L cat /proc/meminfo 2>/dev/null | grep -E "MemTotal|MemAvailable|SwapTotal|SwapFree" | sed 's/^/    /'

S "6. HOST bellek durumu (asil supheli)"
free -h | sed 's/^/  /'
echo "  --- basinc ---"; cat /proc/pressure/memory | sed 's/^/    /'
echo "  --- konteyner cgroup ---"
for f in memory.current memory.max memory.peak memory.events; do
  v=$(cat /sys/fs/cgroup/machine.slice/*waydroid*/$f 2>/dev/null || cat /sys/fs/cgroup/lxc.payload.waydroid/$f 2>/dev/null)
  [ -n "${v:-}" ] && echo "    $f: $v"
done
echo "  --- host OOM killer ---"
journalctl -k --since "1 hour ago" --no-pager 2>/dev/null | grep -iE "out of memory|oom-kill|killed process" | tail -10 || echo "    (kernel OOM kaydi yok)"

S "7. GAPPS ne kadar agir (VANILLA ile fark)"
echo "  calisan Android sureci: $(L sh -c 'ps -A 2>/dev/null | wc -l' | tail -1)"
L sh -c 'ps -A -o RSS,NAME 2>/dev/null | sort -rn | head -12' 2>&1 | sed 's/^/    /'
