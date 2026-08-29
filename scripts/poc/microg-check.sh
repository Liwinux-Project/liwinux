#!/usr/bin/env bash
# Diagnose microG signature spoofing and Play Store sign-in
set -u
[ "$(id -u)" = 0 ] || { echo "root: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Is the signature-spoofing permission DEFINED AT ALL (the root issue)"
L pm list permissions -f 2>/dev/null | grep -i "FAKE_PACKAGE_SIGNATURE" \
  && echo "  -> the permission IS defined" || echo "  -> the permission is NOT defined (the image does not support it)"

S "2. Has it been granted to GmsCore"
L dumpsys package com.google.android.gms | grep -iE "FAKE_PACKAGE_SIGNATURE|granted=true" | grep -i fake \
  || echo "  (none)"

S "3. Has the Play Store (Phonesky) signature been patched"
L dumpsys package com.android.vending | grep -iE "versionName|signatures|pkgFlags" | head -5
echo "  --- PatchPhonesky files ---"
ls /var/lib/waydroid/overlay/system/priv-app/ 2>/dev/null | grep -i phonesky | sed 's/^/    /'
ls /var/lib/waydroid/overlay/system/priv-app/PatchPhonesky/ 2>/dev/null | sed 's/^/    /'

S "4. Has microG checkin (device registration) happened"
L content query --uri content://com.google.android.gsf.gservices --where "name='android_id'" 2>&1 | head -3
echo "  --- checkin logs ---"
L logcat -d -t 500 | grep -iE "checkin|CheckinService|android_id" | tail -10

S "5. SIGN-IN ATTEMPT LOGS (the real evidence)"
L logcat -d -t 800 | grep -iE "GmsAuth|AuthManager|SignIn|sign.in|Phonesky|Finsky.*auth|AccountManager|microg.*auth|UnsupportedOperation|SecurityException" | tail -30

S "6. Is a Google account registered"
L dumpsys account 2>/dev/null | grep -iE "Account \{|com.google" | head -8 || echo "  (no account)"

S "7. Image type (VANILLA vs GAPPS)"
echo "  ro.build.flavor    : $(L getprop ro.build.flavor | tail -1)"
echo "  ro.lineage.build   : $(L getprop ro.lineage.build.version | tail -1)"
ls /var/lib/waydroid/images/ 2>/dev/null | sed 's/^/    /'
grep -iE "vendor_type|images_path|system_type" /var/lib/waydroid/waydroid.cfg 2>/dev/null | sed 's/^/    /'
