#!/usr/bin/env python3
"""POC: Waydroid'in dokunuş FIFO'suna DOĞRUDAN yaz ve ekran dışına çık.

İki iddiayı aynı anda sınar:

  1. Konteynerdeki `/dev/input/wl_touch_events` FIFO'suna ham `input_event`
     yazmak Android'e gerçek bir çoklu dokunuş verir.
     (uinput → libinput → KWin → wl_touch zincirini TAMAMEN atlar.)

  2. Bu yolda koordinat KIRPILMAZ. Parmak ekranın dışına çıkabilir ve
     dokunuş yine de oyuna ulaşır — çünkü:
       * FIFO'da çekirdeğin evdev katmanı yok (ABS kırpması yok),
       * TouchInputMapper::cookPointerData() kırpmaz,
       * InputDispatcher yalnızca DOWN'da pencere seçer; MOVE mandallanmış
         pencereye gider.
     Doğruysa "kenara gelince ortala" ihtiyacı KÖKTEN ortadan kalkar.

Kullanım (root şart — FIFO system:system 0660):

    sudo python3 scripts/poc/fifo-touch.py

Önce dokunuş göstergesini aç ki gözle görebilelim:

    waydroid shell -- settings put system pointer_location 1
"""
import ctypes, errno, os, struct, subprocess, sys, time

# --- evdev sabitleri ---
EV_SYN, EV_ABS = 0x00, 0x03
SYN_REPORT = 0
ABS_MT_SLOT, ABS_MT_TOUCH_MAJOR = 0x2f, 0x30
ABS_MT_POSITION_X, ABS_MT_POSITION_Y = 0x35, 0x36
ABS_MT_TRACKING_ID, ABS_MT_PRESSURE = 0x39, 0x3a

# struct input_event (x86_64): timeval(16) + type(2) + code(2) + value(4)
EV_FMT = "@llHHi"
assert struct.calcsize(EV_FMT) == 24, struct.calcsize(EV_FMT)

FIFO = "dev/input/wl_touch_events"


def container_pid():
    """LXC konteynerinin init pid'i. Android süreçlerinden geriye gidiyoruz:
    isim eşlemesi kırılgan, ama surfaceflinger konteyner dışında olamaz."""
    for name in ("surfaceflinger", "system_server"):
        out = subprocess.run(["pgrep", "-x", name], capture_output=True, text=True)
        for line in out.stdout.split():
            return int(line)
    return None


def monotonic_tv():
    t = time.clock_gettime(time.CLOCK_MONOTONIC)
    return int(t), int((t - int(t)) * 1_000_000)


def frame(fd, events):
    """Bir kare = tek write().

    Tek çağrı ŞART: POSIX yalnızca PIPE_BUF'tan (4096) kısa yazmaların
    bölünmezliğini garanti eder. Kareyi parçalarsak hwcomposer'ın kendi
    yazmaları araya girer ve EventHub bozuk kayıt okur.
    """
    sec, usec = monotonic_tv()
    buf = b"".join(struct.pack(EV_FMT, sec, usec, t, c, v) for t, c, v in events)
    buf += struct.pack(EV_FMT, sec, usec, EV_SYN, SYN_REPORT, 0)
    assert len(buf) <= 4096, "kare PIPE_BUF'u aşıyor"
    os.write(fd, buf)


def down(fd, slot, x, y):
    frame(fd, [(EV_ABS, ABS_MT_SLOT, slot), (EV_ABS, ABS_MT_TRACKING_ID, slot),
               (EV_ABS, ABS_MT_POSITION_X, x), (EV_ABS, ABS_MT_POSITION_Y, y),
               (EV_ABS, ABS_MT_PRESSURE, 50)])


def move(fd, slot, x, y):
    frame(fd, [(EV_ABS, ABS_MT_SLOT, slot), (EV_ABS, ABS_MT_TRACKING_ID, slot),
               (EV_ABS, ABS_MT_POSITION_X, x), (EV_ABS, ABS_MT_POSITION_Y, y),
               (EV_ABS, ABS_MT_PRESSURE, 50)])


def up(fd, slot):
    frame(fd, [(EV_ABS, ABS_MT_SLOT, slot), (EV_ABS, ABS_MT_TRACKING_ID, -1)])


def prop(key, default=None):
    out = subprocess.run(["waydroid", "prop", "get", key],
                         capture_output=True, text=True)
    v = out.stdout.strip()
    return v if v else default


def main():
    if os.geteuid() != 0:
        sys.exit("root gerekiyor: FIFO system:system 0660.  sudo ile çalıştır.")

    pid = container_pid()
    if not pid:
        sys.exit("Waydroid konteyneri çalışmıyor (surfaceflinger yok).")

    path = f"/proc/{pid}/root/{FIFO}"
    if not os.path.exists(path):
        sys.exit(f"{path} yok — hwcomposer FIFO'yu henüz kurmamış olabilir.")

    st = os.stat(path)
    import stat as st_mod
    print(f"FIFO   : {path}")
    print(f"tür    : {'FIFO ✅' if st_mod.S_ISFIFO(st.st_mode) else 'FIFO DEĞİL ❌'}"
          f"  mod={oct(st.st_mode & 0o777)} uid={st.st_uid} gid={st.st_gid}")

    w = int(prop("waydroid.display_width", "0"))
    h = int(prop("waydroid.display_height", "0"))
    print(f"ekran  : {w}x{h}  (waydroid.display_width/height)")
    if not w or not h:
        sys.exit("display_width/height okunamadı.")

    # O_NONBLOCK: okuyucu (EventHub) zaten açık olmalı. Değilse ENXIO alırız
    # ve bu, "Android bu FIFO'yu dinlemiyor" demektir — sessizce beklemek
    # yerine bunu söylemek istiyoruz.
    try:
        fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
    except OSError as e:
        if e.errno == errno.ENXIO:
            sys.exit("ENXIO: FIFO'nun okuyucusu yok — EventHub bu boruyu açmamış.")
        raise
    print("açıldı : O_WRONLY|O_NONBLOCK ✅\n")

    slot = 9          # yüksek slot: gerçek dokunuşlarla çakışmasın
    y = h // 2
    x0 = int(w * 0.72)          # sağ yarı: FPS oyunlarında bakış bölgesi

    # --- 1. aşama: ekran İÇİNDE sürükleme (yol çalışıyor mu) ---
    print("1) ekran içi sürükleme: x = %d → %d" % (x0, w - 40))
    down(fd, slot, x0, y)
    time.sleep(0.02)
    for x in range(x0, w - 40, 24):
        move(fd, slot, x, y)
        time.sleep(0.005)

    # --- 2. aşama: ekran DIŞINA devam (kırpma var mı) ---
    far = w * 3
    print("2) ekran DIŞI sürükleme: x = %d → %d  (ekran genişliği %d)" % (w - 40, far, w))
    for x in range(w - 40, far, 24):
        move(fd, slot, x, y)
        time.sleep(0.005)

    # --- 3. aşama: geri dön ve kaldır ---
    print("3) geri dönüş ve kaldırma")
    for x in range(far, x0, -48):
        move(fd, slot, x, y)
        time.sleep(0.005)
    up(fd, slot)
    os.close(fd)

    print("""
Ne aranacak:
  * Aşama 1 iz bırakıyorsa  → FIFO yolu çalışıyor (compositor atlandı).
  * Aşama 2'de oyun/uygulama DÖNMEYE DEVAM ediyorsa → kırpma yok:
    kenarda ortalama ihtiyacı ortadan kalkar.
  * pointer_location göstergesinde X değeri 2560'ı geçiyorsa doğrulandı.
  * Aşama 2'de hareket duruyorsa → o oyun ekran dışı koordinatı
    tolere etmiyor; sınırlı ama GENİŞ bir kutu (ör. 3 ekran) kullanılmalı.
""")


if __name__ == "__main__":
    main()
