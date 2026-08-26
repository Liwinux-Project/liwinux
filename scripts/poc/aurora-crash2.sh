#!/usr/bin/env bash
# Cokme istisnasinin BASLIGINI al (tur + mesaj + Caused by)
set -u
[ "$(id -u)" = 0 ] || { echo "root: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. FATAL EXCEPTION basliklari (istisna turu ve mesaji)"
L logcat -b crash -d | grep -E "FATAL EXCEPTION|^.*AndroidRuntime: (Process|java\.|kotlin\.|android\.|Caused by)" | tail -40

S "2. Ilk 45 satir ham crash (basliktan itibaren)"
L logcat -b crash -d | grep -A45 "FATAL EXCEPTION" | head -60

S "3. Hangi surec cokmus"
L logcat -b crash -d | grep -E "Process:.*PID" | tail -10

S "4. Aurora hedef SDK uyumu"
echo "  cihaz SDK      : $(L getprop ro.build.version.sdk | tail -1)"
echo "  cihaz surum    : $(L getprop ro.build.version.release | tail -1)"
L dumpsys package com.aurora.store | grep -E "targetSdk|minSdk|versionName" | head -3

S "5. Aurora'yi temiz baslat, canli yakala"
L am force-stop com.aurora.store
L logcat -c -b crash 2>/dev/null
L am start -n com.aurora.store/.MainActivity >/dev/null 2>&1
echo "  10 saniye bekleniyor (bu sirada Anonymous'a BASMA, sadece acilsin)..."
sleep 10
echo "  --- acilista cokme oldu mu ---"
L logcat -b crash -d | grep -A25 "FATAL EXCEPTION" | head -35
echo "  --- surec durumu ---"
L pidof com.aurora.store || echo "    cokmus/kapali"
