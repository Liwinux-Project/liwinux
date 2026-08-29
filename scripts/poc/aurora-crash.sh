#!/usr/bin/env bash
# Find out why Aurora Store crashes
set -u
[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. CRASH tamponu (en onemli)"
L logcat -b crash -d -t 200 | tail -60

S "2. Recent logs mentioning Aurora"
L logcat -d -t 400 | grep -iE "aurora|AndroidRuntime|FATAL|DEAD_OBJECT|ANR " | tail -40

S "3. Native crash (tombstone)"
L logcat -b crash -d | grep -iE "signal|backtrace|tombstone|SIGSEGV|SIGABRT" | tail -20
L ls /data/tombstones/

S "4. Is Aurora still running"
L pidof com.aurora.store || echo "  (no process - it crashed)"

S "5. Aurora version and ABI"
L dumpsys package com.aurora.store | grep -iE "versionName|primaryCpuAbi|legacyNativeLibraryDir|codePath|targetSdk|flags" | head -10

S "6. Was there memory pressure at the moment it crashed"
L dumpsys meminfo 2>/dev/null | head -12
echo "  --- lowmemorykiller ---"
L logcat -d -t 300 | grep -iE "lmkd|low.?memory|Killing|am_kill" | tail -10

S "7. Network state (required for the anonymous token)"
L dumpsys connectivity | grep -oE "VALIDATED" | head -2
L ping -c 2 -W 3 auroraoss.com 2>&1 | tail -3
L ping -c 2 -W 3 android.clients.google.com 2>&1 | tail -3
