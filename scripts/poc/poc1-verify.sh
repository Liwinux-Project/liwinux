#!/usr/bin/env bash
# liwinux PoC 1 dogrulama — GPU + ARM ceviri AYNI ANDA calisiyor mu?
set -u
W() { waydroid --details-to-stdout shell "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Boot"
for i in $(seq 1 24); do
  b=$(W getprop sys.boot_completed | tail -1)
  echo "  [$i] boot_completed='$b'"; [ "$b" = "1" ] && break; sleep 5
done

S "2. GPU hala saglam mi? (KRITIK-1)"
W dumpsys SurfaceFlinger | grep -iE "^GLES" | head -3

S "3. Calisan grafik prop'lari"
for p in ro.hardware.gralloc ro.hardware.egl ro.hardware.vulkan debug.hwui.renderer; do
  printf "  %-28s = %s\n" "$p" "$(W getprop $p | tail -1)"
done

S "4. ARM ceviri aktif mi? (KRITIK-2)"
for p in ro.product.cpu.abilist ro.dalvik.vm.native.bridge ro.enable.native.bridge.exec ro.dalvik.vm.isa.arm64; do
  printf "  %-32s = %s\n" "$p" "$(W getprop $p | tail -1)"
done

S "5. libhoudini dosyalari yerinde mi"
W ls -la /system/lib64/libhoudini.so /system/lib/libhoudini.so
W sh -c 'ls /system/lib64/arm64/ 2>/dev/null | head -5'

S "6. ARM64 kodu GERCEKTEN calisiyor mu? (KRITIK-3)"
echo "  -- houdini ile arm64 binary calistirma denemesi --"
W sh -c '/system/bin/houdini64 --version 2>&1 || echo "houdini64 yok/calismadi"'
echo "  -- package manager ABI gorusu --"
W sh -c 'pm get-install-location; getprop ro.product.cpu.abilist64'

S "7. Hata avi (logcat)"
timeout 10 waydroid --details-to-stdout shell logcat -d 2>/dev/null | tr -d '\r' \
  | grep -iE "houdini|native.?bridge|venus|angle|vulkan|E ANGLE|FATAL" | tail -30

S "8. Host kaynak"
nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader 2>/dev/null
free -h | head -2
