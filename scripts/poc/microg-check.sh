#!/usr/bin/env bash
# microG signature spoofing ve Play Store giris teshisi
set -u
[ "$(id -u)" = 0 ] || { echo "root: sudo bash $0"; exit 1; }
L() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }
S() { echo; echo "=== $* ==="; }

S "1. Signature spoofing izni SISTEMDE TANIMLI MI (kok sorun)"
L pm list permissions -f 2>/dev/null | grep -i "FAKE_PACKAGE_SIGNATURE" \
  && echo "  -> izin TANIMLI" || echo "  -> izin TANIMSIZ (imaj destegi yok)"

S "2. GmsCore'a verilmis mi"
L dumpsys package com.google.android.gms | grep -iE "FAKE_PACKAGE_SIGNATURE|granted=true" | grep -i fake \
  || echo "  (yok)"

S "3. Play Store (Phonesky) imzasi yamalanmis mi"
L dumpsys package com.android.vending | grep -iE "versionName|signatures|pkgFlags" | head -5
echo "  --- PatchPhonesky dosyalari ---"
ls /var/lib/waydroid/overlay/system/priv-app/ 2>/dev/null | grep -i phonesky | sed 's/^/    /'
ls /var/lib/waydroid/overlay/system/priv-app/PatchPhonesky/ 2>/dev/null | sed 's/^/    /'

S "4. microG checkin (cihaz kaydi) yapilmis mi"
L content query --uri content://com.google.android.gsf.gservices --where "name='android_id'" 2>&1 | head -3
echo "  --- checkin loglari ---"
L logcat -d -t 500 | grep -iE "checkin|CheckinService|android_id" | tail -10

S "5. GIRIS DENEMESI LOGLARI (asil kanit)"
L logcat -d -t 800 | grep -iE "GmsAuth|AuthManager|SignIn|sign.in|Phonesky|Finsky.*auth|AccountManager|microg.*auth|UnsupportedOperation|SecurityException" | tail -30

S "6. Kayitli Google hesabi var mi"
L dumpsys account 2>/dev/null | grep -iE "Account \{|com.google" | head -8 || echo "  (hesap yok)"

S "7. Imaj turu (VANILLA vs GAPPS)"
echo "  ro.build.flavor    : $(L getprop ro.build.flavor | tail -1)"
echo "  ro.lineage.build   : $(L getprop ro.lineage.build.version | tail -1)"
ls /var/lib/waydroid/images/ 2>/dev/null | sed 's/^/    /'
grep -iE "vendor_type|images_path|system_type" /var/lib/waydroid/waydroid.cfg 2>/dev/null | sed 's/^/    /'
