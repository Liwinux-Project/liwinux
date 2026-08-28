#!/usr/bin/env python3
"""POC: write DIRECTLY to Waydroid's touch FIFO and leave the screen.

It tests two claims at once:

  1. Konteynerdeki `/dev/input/wl_touch_events` FIFO'suna ham `input_event`
     writing gives Android a genuine multi-touch.
     (uinput → libinput → KWin → wl_touch zincirini TAMAMEN atlar.)

  2. Coordinates are NOT CLAMPED on this path. The finger can leave the
     screen and the touch still reaches the game — because:
       * the FIFO has no kernel evdev layer (no ABS clamping),
       * TouchInputMapper::cookPointerData() does not clamp,
       * InputDispatcher only picks a window on DOWN; MOVE goes to the latched
         pencereye gider.
     If true, the need to "recentre at the edge" disappears at the root.

Usage (root required — the FIFO is system:system 0660):

    sudo python3 scripts/poc/fifo-touch.py

Turn on the touch indicator first so it is visible:

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
    """Init pid of the LXC container. We work backwards from Android processes:
    name matching is fragile, but surfaceflinger cannot exist outside it."""
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

    A single call is MANDATORY: POSIX only guarantees atomicity for writes
    shorter than PIPE_BUF (4096). Splitting the frame lets hwcomposer's own
    writes slip in and EventHub reads a corrupt record.
    """
    sec, usec = monotonic_tv()
    buf = b"".join(struct.pack(EV_FMT, sec, usec, t, c, v) for t, c, v in events)
    buf += struct.pack(EV_FMT, sec, usec, EV_SYN, SYN_REPORT, 0)
    assert len(buf) <= 4096, "frame exceeds PIPE_BUF"
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
        sys.exit("root required: the FIFO is system:system 0660.  Run with sudo.")

    pid = container_pid()
    if not pid:
        sys.exit("The Waydroid container is not running (no surfaceflinger).")

    path = f"/proc/{pid}/root/{FIFO}"
    if not os.path.exists(path):
        sys.exit(f"{path} does not exist — hwcomposer may not have created the FIFO yet.")

    st = os.stat(path)
    import stat as st_mod
    print(f"FIFO   : {path}")
    print(f"type   : {'FIFO ✅' if st_mod.S_ISFIFO(st.st_mode) else 'NOT A FIFO ❌'}"
          f"  mod={oct(st.st_mode & 0o777)} uid={st.st_uid} gid={st.st_gid}")

    w = int(prop("waydroid.display_width", "0"))
    h = int(prop("waydroid.display_height", "0"))
    print(f"ekran  : {w}x{h}  (waydroid.display_width/height)")
    if not w or not h:
        sys.exit("could not read display_width/height.")

    # O_NONBLOCK: the reader (EventHub) should already be open. If not we get
    # ve bu, "Android bu FIFO'yu dinlemiyor" demektir — sessizce beklemek
    # ENXIO, and we would rather say that than hang.
    try:
        fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
    except OSError as e:
        if e.errno == errno.ENXIO:
            sys.exit("ENXIO: the FIFO has no reader — EventHub has not opened this pipe.")
        raise
    print("opened : O_WRONLY|O_NONBLOCK ✅\n")

    slot = 9          # a high slot, so it does not collide with real touches
    y = h // 2
    x0 = int(w * 0.72)          # right half: the look area in FPS games

    # --- stage 1: drag INSIDE the screen (does the path work?) ---
    print("1) on-screen drag: x = %d -> %d" % (x0, w - 40))
    down(fd, slot, x0, y)
    time.sleep(0.02)
    for x in range(x0, w - 40, 24):
        move(fd, slot, x, y)
        time.sleep(0.005)

    # --- stage 2: continue OFF-SCREEN (is there clamping?) ---
    far = w * 3
    print("2) off-screen drag: x = %d -> %d  (screen width %d)" % (w - 40, far, w))
    for x in range(w - 40, far, 24):
        move(fd, slot, x, y)
        time.sleep(0.005)

    # --- stage 3: return and lift ---
    print("3) return and lift")
    for x in range(far, x0, -48):
        move(fd, slot, x, y)
        time.sleep(0.005)
    up(fd, slot)
    os.close(fd)

    print("""
Ne aranacak:
  * If stage 1 leaves a trace  -> the FIFO path works (compositor bypassed).
  * If the game/app KEEPS TURNING in stage 2 -> there is no clamping:
    the need to recentre at the edge disappears.
  * If X exceeds 2560 in the pointer_location overlay, it is verified.
  * If movement stops in stage 2 -> that game does not tolerate off-screen
    coordinates; use a bounded but WIDE box (e.g. 3 screens).
""")


if __name__ == "__main__":
    main()
