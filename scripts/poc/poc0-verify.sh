#!/usr/bin/env bash
# liwinux PoC 0 — Waydroid + NVIDIA/Venus GPU verification
# Usage: sudo bash poc0-verify.sh
set -u
S() { echo; echo "=== $* ==="; }

S "1. Boot durumu"
for i in $(seq 1 24); do
  b=$(waydroid --details-to-stdout shell getprop sys.boot_completed 2>/dev/null | tr -d '\r' | tail -1)
  echo "  [$i] sys.boot_completed='$b'"
  [ "$b" = "1" ] && break
  sleep 5
done

S "2. SurfaceFlinger GPU rendering (CRITICAL)"
waydroid --details-to-stdout shell dumpsys SurfaceFlinger 2>/dev/null \
  | grep -iE "GLES|Vulkan|DisplayDevice|renderer" | head -12

S "3. Grafik property'leri"
for p in ro.hardware.gralloc ro.hardware.egl ro.hardware.vulkan \
         debug.hwui.renderer ro.hardware.hwcomposer persist.waydroid.multi_windows; do
  v=$(waydroid --details-to-stdout shell getprop "$p" 2>/dev/null | tr -d '\r' | tail -1)
  printf "  %-34s = %s\n" "$p" "$v"
done

S "4. Installed graphics drivers (guest)"
waydroid --details-to-stdout shell sh -c 'ls /vendor/lib64/hw/ 2>/dev/null | grep -iE "gralloc|hwcomposer"; ls /vendor/lib64/egl/ 2>/dev/null' 2>/dev/null | tr -d '\r' | head -20

S "5. Is ARM translation (native bridge) present?"
for p in ro.dalvik.vm.native.bridge ro.enable.native.bridge.exec; do
  v=$(waydroid --details-to-stdout shell getprop "$p" 2>/dev/null | tr -d '\r' | tail -1)
  printf "  %-34s = %s\n" "$p" "$v"
done
waydroid --details-to-stdout shell sh -c 'ls /system/lib64/libndk_translation.so /system/lib64/libhoudini.so 2>&1' 2>/dev/null | tr -d '\r'

S "6. CPU ABI listesi"
waydroid --details-to-stdout shell getprop ro.product.cpu.abilist 2>/dev/null | tr -d '\r' | tail -1

S "7. Guest GPU hatalari (logcat)"
timeout 8 waydroid --details-to-stdout shell logcat -d 2>/dev/null \
  | grep -iE "venus|angle|gralloc|vulkan|EGL|SurfaceFlinger" | tail -25

S "8. Host tarafi"
echo "  --- wd-venus son loglar ---"
journalctl --user -u wd-venus.service -n 15 --no-pager 2>/dev/null | tail -15
echo "  --- NVIDIA GPU kullanimi ---"
nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv 2>/dev/null
nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader 2>/dev/null
echo "  --- RAM ---"
free -h | head -2
cat /proc/pressure/memory 2>/dev/null
