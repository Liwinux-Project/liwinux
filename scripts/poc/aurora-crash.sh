#!/usr/bin/env bash
# Aurora Store cokmesinin nedenini bul
set -u
[ "$(id -u)" = 0 ] || { echo "root gerekiyor: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. CRASH tamponu (en onemli)"
L logcat -b crash -d -t 200 | tail -60

S "2. Aurora ile ilgili son loglar"
L logcat -d -t 400 | grep -iE "aurora|AndroidRuntime|FATAL|DEAD_OBJECT|ANR " | tail -40

S "3. Native cokme (tombstone)"
L logcat -b crash -d | grep -iE "signal|backtrace|tombstone|SIGSEGV|SIGABRT" | tail -20
L ls /data/tombstones/

S "4. Aurora hala calisiyor mu"
L pidof com.aurora.store || echo "  (surec yok - cokmus)"

S "5. Aurora surum + ABI"
L dumpsys package com.aurora.store | grep -iE "versionName|primaryCpuAbi|legacyNativeLibraryDir|codePath|targetSdk|flags" | head -10

S "6. Bellek baskisi cokme aninda oldu mu"
L dumpsys meminfo 2>/dev/null | head -12
echo "  --- lowmemorykiller ---"
L logcat -d -t 300 | grep -iE "lmkd|low.?memory|Killing|am_kill" | tail -10

S "7. Ag durumu (anonymous token icin sart)"
L dumpsys connectivity | grep -oE "VALIDATED" | head -2
L ping -c 2 -W 3 auroraoss.com 2>&1 | tail -3
L ping -c 2 -W 3 android.clients.google.com 2>&1 | tail -3
