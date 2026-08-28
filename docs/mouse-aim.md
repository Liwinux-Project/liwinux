# The FPS mouse problem: root cause and definitive fix

Symptom, in the user's words: *"when the mouse reaches the edge of the screen it
teleports back to the centre; that makes the mouse sometimes not register at
all, sometimes not register for 2-3 seconds, and sometimes drift the aim."*

All three symptoms follow from **one design assumption**:

> "A finger cannot be dragged forever, so at the edge we must lift it and place
> it back at the centre."

On Waydroid that assumption is **WRONG**. Below is why, and what replaces it.

---

## 1. Today's chain and why it loses

```
evdev -> liwinux engine -> uinput -> libinput -> KWin -> wl_touch
       -> Waydroid hwcomposer -> /dev/input/wl_touch_events -> Android EventHub
```

We create our own virtual touchscreen on the host and hand it to the compositor.
The compositor forwards it to the Waydroid window, and Waydroid **already**
writes the same events into a FIFO inside the container. The last link of the
chain was always there; we simply take the long way round.

What the long way costs:

| Link | What it does |
|---|---|
| kernel evdev | clamps ABS values into the advertised range |
| libinput | normalizes the touch from device space into **screen space** — nothing stays off-screen |
| KWin | converts the touch to surface coordinates, does not deliver outside the window, and is bound to its own frame clock |
| ScreenMap | window geometry maths (polled through a KWin script) |

The result: **the finger cannot leave the screen.** That is why the engine has to
lift-recentre-press at the edge, and why all the complexity (delayed press,
handoff, non-linear scaling, idle recentring) exists — to hide that single
constraint.

---

## 2. Waydroid's input path (verified)

Waydroid patches Android's `EventHub`
(`anbox-patches/frameworks/native/0006-EventHub-Add-wayland-inputs-support.patch`).
Three **named pipes** are listened on inside the container:

| Path | EventHub device name | Class |
|---|---|---|
| `/dev/input/wl_touch_events` | `wayland_touch` | `TOUCH_MT` + `INPUT_PROP_DIRECT` |
| `/dev/input/wl_pointer_events` | `wayland_pointer` | `CURSOR` |
| `/dev/input/wl_keyboard_events` | `wayland_keyboard` | `KEYBOARD` |

Verified on this machine:

* `libinputreader.so` contains the strings `/dev/input/wl_touch_events`,
  `wayland_touch`, `waydroid.display_width` and `waydroid.display_height`.
* The FIFO is created by **hwcomposer** (`wayland-hwc.cpp`):
  `mkfifo(..., 0660)` + `chown(..., 1000, 1000)` -> owned by `system:system`.
* On a display hotplug the FIFO is **deleted and recreated** — an open fd is
  orphaned and must be reopened.

### Pipe protocol

A raw array of `struct input_event` (24 bytes per record on x86_64) with
`CLOCK_MONOTONIC` timestamps. The exact order hwcomposer produces:

```c
// press / move (identical sequence)
ABS_MT_SLOT        = slot
ABS_MT_TRACKING_ID = slot          // same for press and move
ABS_MT_POSITION_X  = x             // ANDROID SCREEN PIXELS
ABS_MT_POSITION_Y  = y
ABS_MT_PRESSURE    = 50
SYN_REPORT         = 0
// lift
ABS_MT_SLOT        = slot
ABS_MT_TRACKING_ID = -1
SYN_REPORT         = 0
```

No `BTN_TOUCH`, no `ABS_X`/`ABS_Y`. The coordinate space is directly
`waydroid.display_width` x `waydroid.display_height` (2560x1440 here); EventHub
**invents** the axis information from those properties instead of asking the
device (the `location == "wayland"` branch in `getAbsoluteAxisInfo`).

**Atomicity:** every frame must go out in ONE `write()`. hwcomposer writes to the
same pipe, and POSIX only guarantees atomicity for writes below `PIPE_BUF`
(4096 bytes). 4096 / 24 = 170 events; one of our frames is at most ~60 events,
so it is safe.

---

## 3. The key finding: nothing clamps on this path

Three independent layers of the chain do not clamp:

1. **No kernel.** The FIFO is a pipe; the evdev driver layer is not involved at
   all. `input_handle_abs_event` never runs, so there is no `ABS` range clamp.
   *(This was impossible on the uinput path.)*

2. **`TouchInputMapper::cookPointerData()` does not clamp.** From the Android 13
   source: `mAffineTransform.applyTo()` + `rotateAndScale()`, then straight into
   `AMOTION_EVENT_AXIS_X/Y`. There is no surface bounds test; the only `clamp` in
   the file is `clampResolution()`, and that is for touch **size** axes.

3. **`InputDispatcher` does not re-pick a window on MOVE.**
   `findTouchedWindowTargetsLocked()` only looks for a target on `ACTION_DOWN` /
   `ACTION_POINTER_DOWN` (the `newGesture` branch). MOVE goes to the latched
   `tempTouchState`. The one exception is windows flagged `SLIPPERY` — games do
   not set that, and the branch is gated on `pointerCount == 1` anyway.

### Conclusion

> **As long as a touch GOES DOWN inside the game window, its later movements
> reach that same window even when they leave the screen.**

That removes the need to recentre at the edge **entirely**. The aim finger goes
down once and roams an unbounded plane until aim is released.

Almost every Android FPS computes look as `delta = position - previous_position`
and only uses the absolute position at `ACTION_DOWN`, to test "is this touch in
the look area". As long as the press lands in the right place, where the finger
goes afterwards is of no concern to the game.

---

## 4. Root causes of the three symptoms

### 4.1 "Sometimes it does not register at all"

**a) The aim finger drifts into the left half.** With `origin = (0.50, 0.50)` and
`recenter_margin = 0.03` in the profile, the finger roams x in [0.03, 0.97]. In
most FPS games the look-area test is `x > width/2`. If the finger lands in the
left half after a reset, the game treats it as the **movement joystick**: look
dies and the character walks sideways on top of it. Because `origin` sits exactly
on the boundary, every reset is a coin flip.

**b) Pointer pool leak.** Once the pool is full, `on_mouse` cannot get a pointer
and the mouse dies completely (`reconcile_pointers` exists to repair this, which
means it actually happened).

### 4.2 "It does not register for 2-3 seconds"

`engine.rs` had a **frame-merge bug** in the delayed press:

```rust
if let Some(t) = self.aim_down_at {
    if now_ms >= t {
        self.aim_down_at = None;
        acts.push(TouchAction::Down { id, at: origin });   // press
    }
    if self.aim_down_at.is_some() { return ...; }          // now None -> falls through
}
...
} else if self.aim_accum != (0.0, 0.0) {
    acts.extend(self.on_mouse(ax, ay));                    // move in the SAME frame
}
```

The press and the move land in the same `dispatch()` call, and therefore in the
same **`SYN_REPORT`** — exactly the bug warned about repeatedly elsewhere in the
code. Worse, `aim_accum` keeps accumulating throughout the delay (12 ms plus the
5 ms tick granularity), so the press happens directly at the accumulated
position: the game sees zero delta and that turn is lost.

On fast turns this loops — every reset feeds the next one and the user
experiences dead zones lasting seconds.

`idle_recenter()` returned `Up` and `Down` in one batch too; the same frame merge
was present there.

### 4.3 "It drifts the aim"

**a) `nonlinear = true`.** Sensitivity is scaled by `sqrt(min_d / d)` and falls
by **up to 3x** as the finger moves away from the centre. The same mouse movement
therefore turns a different angle depending on the finger's invisible position.
Muscle memory cannot be built on that — the symptom is precisely "aim drifts".

**b) `reseat_point` seats at the far end.** Turning right puts the finger on the
left edge. That buys screen width but pushes the press point into the game's
movement pad area (4.1a).

**c) Overflowing movement is discarded.** The amount past the limit is
deliberately dropped (the reasoning is in the comment) — it prevents the loop,
but a little rotation is lost on every reset.

---

## 5. The fix

### 5.1 Shorten the injection path

```
evdev -> liwinux engine -> /proc/<container>/root/dev/input/wl_touch_events
```

`uinput`, `libinput`, `KWin`, `wl_touch` and `ScreenMap` all leave the chain.

**Privilege:** the FIFO is `system:system 0660`; writing needs root.
`liwd-helper` already runs as root. The right design is for the helper to open
the FIFO and **hand the file descriptor back over D-Bus** (`zvariant::OwnedFd`).
That way:

* authorization is asked once, in polkit,
* the 200 Hz write traffic never passes through IPC — `liwd` writes to the fd
  directly,
* the helper exposes no general-purpose write interface — it hands over exactly
  one write handle.

Entering the container's mount namespace needs no `nsenter`: as root,
`/proc/<pid>/root/...` opens it directly.

**Careful:** the FIFO is recreated on hotplug; the fd must be requested again
when `waydroid.display_width` changes or a write fails.

### 5.2 Unbounded aim

For the aim finger:

* The press happens **once**, in the middle of the game's look area.
* After that, only `Move`. No lift, no recentring, no handoff, no delayed press.
* Sensitivity is **constant** (`nonlinear` off) — a 1:1 feel.
* The position is kept unbounded; there is only a wide safety box against
  overflow, and if it is ever reached, it resets silently **while the mouse is
  stopped** (invisibly).

That removes 4.1, 4.2 and 4.3 in one go: no finger to lift, no delay to wait for,
no varying sensitivity.

### 5.3 Profile corrections (SFG2)

* Put `origin` at the centre but NOT exactly on the boundary: `x = 0.5004`,
  `y = 0.50` (pixel 1281 at 2560 — one pixel right of centre).

  In unbounded mode the press happens once, so this point only decides "does the
  game count this as look"; everything after is delta. Exactly `0.50` must not be
  used: in a game writing the test as `x > width/2`, `1280 > 1280` is false, the
  finger falls into the left half and the game takes it for the movement pad —
  aim dies entirely. One pixel to the right is indistinguishable by eye and
  removes that risk.
* `nonlinear = false`, `handoff = false`; the reset fields become meaningless.
* Raising the in-game look sensitivity and lowering `sensitivity` feels best; not
  required in unbounded mode, but it keeps the numeric range small.
* `persist.waydroid.fake_touch` **must stay on** (the first draft got this
  wrong). Reading the source made it clear:

  `0016-Fake-touch-inputs-for-select-apps.patch` only does this in `ViewRootImpl`
  during `deliverInputEvent`:

  ```java
  if (mFakeClickAsTouch && q.mEvent instanceof MotionEvent) {
      int action = ev.getAction();
      if (action == ACTION_MOVE || ACTION_DOWN || ACTION_UP)
          ev.setSource(4098);   // SOURCE_TOUCHSCREEN
  }
  ```

  It **produces no new touches**; it relabels the source of an existing event.
  What we write to the pipe already arrives from the `wayland_touch` device and
  is therefore already `SOURCE_TOUCHSCREEN`; `setSource(4098)` is a no-op on us.
  There is no conflict.

  In game mode the mouse is grabbed anyway, so no mouse events reach Waydroid at
  all. With game mode off, fake_touch is needed: menu items can only be clicked
  with the mouse through it.

  **Toggling it together with game mode does not work.** `mFakeClickAsTouch` is a
  `final` field read in the `ViewRootImpl` **constructor**; the value is latched
  when the window is created. It would not affect a running game, only windows
  opened afterwards — inconsistent and hidden behaviour.

### 5.4 Verification

`scripts/poc/fifo-touch.py` tests both claims (run as root). If stage 1 leaves a
trace the FIFO path works; if the turn continues in stage 2 there is no clamping
and unbounded aim is viable.

Fallback if a game does not tolerate off-screen coordinates: a **wide** box
(e.g. 3 screens) instead of unbounded — the reset rate still drops 3x and resets
are bound to the "only while the mouse is stopped" rule from 5.2.

---

## 6. Comparison with existing projects

| Project | Injection | Edge problem |
|---|---|---|
| **XtMapper** | `app_process` shell service -> `InputManager.injectInputEvent` | Same problem; *mitigates* it with non-linear scaling and a delayed reset. The `nonlinear`/`reset_delay_ms` in this project came from there. |
| **waydroid-helper** | scrcpy protocol (TCP + `app_process` server) | Has an aim widget; subject to the same absolute-touch constraint. |
| **scrcpy** | `InputManager` injection | No aim mode; 1:1 absolute mapping. |
| **liwinux (new)** | writes directly to Waydroid's FIFO | Because nothing clamps, the problem **does not arise** — no mitigation needed. |

The difference: all the others inject at the Android **framework** level, where
coordinates have already been fitted into screen space. Waydroid's FIFO sits
**below** `InputReader`; none of the layers that would clamp are involved. This
is an advantage specific to Waydroid.

---

## 7. Implementation status

Done:

* `crates/liw-core/src/input/wl_touch.rs` — the backend writing straight to the
  pipe.
* `Norm::unclamped` / `is_offscreen` — a type that can carry off-screen
  coordinates.
* `unbounded` (default on) and `safety_span` (default 32 screens) in
  `Binding::Aim`.
* `Engine::set_offscreen_ok` — unbounded mode engages only on a non-clamping
  backend; on the uinput path the engine falls back to bounded mode by itself.
* `liwd-helper` -> `OpenTouchPipe`: opens the pipe and hands the write handle
  over D-Bus (polkit: `id.liwinux.helper.touch-pipe`).
* `liwd` and `liw keymap run` request the pipe and fall back to uinput LOUDLY if
  they cannot get it.
* `liw keymap poke` uses the pipe by default and accepts coordinates outside
  0..1 — the verification tool for the claim.
* The SFG2 profile was rewritten.

### Two frame-merge bugs were fixed as well

These were independent of unbounded mode; they were wrong on the bounded path
too:

1. `tick()` applied the accumulated movement in the same frame as the delayed
   press -> press + move in one `SYN_REPORT`, delta lost. The press now stands
   alone in its own frame.
2. `idle_recenter()` returned `Up` and `Down` in one batch -> the same merge. The
   lift is now immediate and the press comes on the next frame.

### Still unverified

The pipe path has **not been measured end to end**: writing to the pipe requires
root and the installed `liwd-helper` was still an older build. The evidence so
far is source and binary inspection:

* the strings `/dev/input/wl_touch_events`, `wayland_touch` and
  `waydroid.display_width/height` inside `libinputreader.so` (on this machine),
* Waydroid's `EventHub` patch and the writer side in `wayland-hwc.cpp`,
* the Android 13 `TouchInputMapper.cpp` / `InputDispatcher.cpp` sources.

Next step: update the helper, then

```
liw keymap poke 0.72 0.5 --to 3.0,0.5 --hold 900
```

`--to 3.0` is three times the screen width. If the touch trace does not stop at
the screen edge and the game keeps turning, the claim is verified.
