#!/usr/bin/env bash
# liwinux PoC 2 dogrulama — GPU + ARM + microG + AG
set -u
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r' | grep -v "^\[.*% lxc-info\|^\[.*RUNNING$"; }
S() { echo; echo "=== $* ==="; }

S "1. Boot"
for i in $(seq 1 24); do b=$(W getprop sys.boot_completed | tail -1); echo "  [$i] '$b'"; [ "$b" = "1" ] && break; sleep 5; done

S "2. GPU (regresyon kontrolu)"
W dumpsys SurfaceFlinger | grep -iE "^GLES" | head -2

S "3. ARM ceviri (regresyon kontrolu)"
W sh -c '/system/bin/houdini64 --version 2>&1 | grep -i version'
W getprop ro.product.cpu.abilist

S "4. microG / Play Store kurulu mu"
W sh -c 'pm list packages 2>/dev/null | grep -iE "gms|gsf|vending|aurora|microg"'
echo "  toplam paket: $(W sh -c 'pm list packages 2>/dev/null | wc -l' | tail -1)"

S "5. AG ERISIMI (Aurora/Play icin sart)"
W sh -c 'ip addr show 2>/dev/null | grep -E "inet " | grep -v 127.0.0.1'
echo "  --- DNS + baglanti testi ---"
W sh -c 'ping -c 2 -W 3 8.8.8.8 2>&1 | tail -3'
W sh -c 'ping -c 2 -W 3 google.com 2>&1 | tail -3'

S "6. microG durumu (logcat)"
waydroid --details-to-stdout shell -- logcat -d 2>/dev/null | tr -d '\r' \
  | grep -iE "microg|gmscore|GmsCore|signature.spoof|FATAL" | tail -20

S "7. Play sertifikasyonu icin cihaz ID"
W sh -c 'sqlite3 /data/data/com.google.android.gsf/databases/gservices.db "select * from main where name = \"android_id\"" 2>/dev/null || echo "(sqlite3 yok veya GSF henuz calismadi)"'

S "8. Kaynak"
nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader 2>/dev/null
free -h | head -2
