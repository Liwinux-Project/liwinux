#!/usr/bin/env bash
# liwinux PoC 1 - corrected verification (with the "--" separator)
set -u
W() { waydroid --details-to-stdout shell -- "$@" 2>&1 | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Is libhoudini visible inside the guest"
W ls -la /system/lib64/libhoudini.so /system/bin/houdini64
W sh -c 'echo "arm64 lib adedi: $(ls /system/lib64/arm64/ 2>/dev/null | wc -l)"'

S "2. binfmt_misc kaydi (ARM ELF exec yolu)"
W sh -c 'cat /proc/sys/fs/binfmt_misc/status 2>&1; echo "--- kayitlar ---"; ls /proc/sys/fs/binfmt_misc/ 2>&1'

S "3. houdini64 calisiyor mu"
W sh -c '/system/bin/houdini64 --version 2>&1 | head -5; echo "exit=$?"'

S "4. ART native bridge yuklendi mi (zygote)"
W sh -c 'grep -l houdini /proc/*/maps 2>/dev/null | head -5; echo "--- zygote64 maps ---"; grep -i houdini /proc/$(pidof zygote64 2>/dev/null | cut -d" " -f1)/maps 2>/dev/null | head -3'

S "5. logcat: houdini / native bridge"
waydroid --details-to-stdout shell -- logcat -d 2>&1 | tr -d '\r' \
  | grep -iE "houdini|nativebridge|native_bridge|NativeBridge" | tail -20

S "6. logcat: any error on the GPU side"
waydroid --details-to-stdout shell -- logcat -d 2>&1 | tr -d '\r' \
  | grep -iE " E |FATAL" | grep -iE "venus|angle|vulkan|gralloc|SurfaceFlinger" | tail -15

S "7. Kurulu paketler / ABI"
W sh -c 'getprop ro.product.cpu.abilist; pm list packages 2>/dev/null | wc -l'
