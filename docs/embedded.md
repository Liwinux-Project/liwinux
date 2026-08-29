# Embedding Android in the liwinux window

Goal: the game rendered *inside* our window with our own chrome around it,
the way GameLoop looks — rather than Waydroid sitting in a separate
top-level window beside the launcher.

## The mechanism, and it is simpler than expected

Waydroid's hwcomposer is an ordinary Wayland **client**. Which compositor it
connects to is decided when the session starts, from the environment
(`waydroid/tools/config/__init__.py`):

```python
"xdg_runtime_dir": str(os.environ.get('XDG_RUNTIME_DIR')),
"wayland_display": str(os.environ.get('WAYLAND_DISPLAY')),
```

`session_manager.py` even accepts an absolute path as `wayland_display`. So
pointing Android at a compositor of ours needs no patching, no system
properties and no container changes — only a different environment when the
session starts.

## Proven end to end, without writing a compositor

Measured on this machine:

```bash
kwin_wayland --width 1280 --height 720 --socket wayland-liw &
WAYLAND_DISPLAY=wayland-liw waydroid session start &
WAYLAND_DISPLAY=wayland-liw waydroid app launch com.ForgeGames.SpecialForcesGroup2
```

Android booted against the nested compositor and Special Forces Group 2
rendered inside that window — its menu, its artwork, all of it. Graphics
worked with no changes: the hwcomposer talks dmabuf to whichever compositor
it finds.

That settles the risky part. Embedding is not a question of whether Waydroid
can be redirected; it can, trivially. It is a question of what does the
compositing.

## What is left

`liw-ui` would have to *be* that compositor:

1. **A nested Wayland compositor** (`smithay`, 0.7) advertising
   `wl_compositor`, `xdg_shell`, `wl_seat`, `wl_output` and
   `linux-dmabuf`, and accepting Waydroid's surface.
2. **Importing its dmabuf as a texture gpui can draw.** This is the hard
   join: gpui renders through Blade, smithay has its own renderer, and
   handing a texture between them almost certainly needs a patch to the
   pinned Zed fork.
3. **Input.** Less than it looks: game input already bypasses the
   compositor entirely by writing to Waydroid's touch pipe, so `wl_seat`
   is only needed for menus and for the pointer outside game mode.

## The performance question, stated honestly

Today: `Waydroid -> KWin -> display`, and when the game is fullscreen and
alone KWin can hand the buffer straight to the display with no compositing
at all.

Embedded: `Waydroid -> us -> KWin -> display` — one more composite pass. With
a zero-copy dmabuf import that is one textured quad per frame, likely under a
millisecond on this GPU.

But the comparison is not "embedded versus today". **Direct scanout is
already impossible the moment the window has chrome around it** — a game with
a sidebar is not a fullscreen-alone window. So the honest trade is:

> Chrome costs the scanout path. Once that is paid, embedding costs about one
> composite on top.

Which means the decision is not "embed or not"; it is "chrome or not". If the
answer is chrome, embedding is the right way to get it.

Whatever is built, measure it with `liw trace` before and after, and keep the
frame-time delta where the numbers say it should be — not where the design
document hoped.

## First run of `liw-compositor` (2026-08-29)

Step 1 exists and Waydroid talks to it. What the run established:

* **The redirection is real.** With `WAYLAND_DISPLAY=wayland-liw`, the
  generated `config_session` bound *our* socket into the container:

  ```
  lxc.mount.entry = /run/user/1000/wayland-liw run/xdg/wayland-0 ...
  ```

  No patching, exactly as the mechanism section predicted.

* **The client connects and takes a configure.** A toplevel was created and
  accepted the 1280x720 fullscreen configure we sent it.

* **The advertised dmabuf format was wrong, and the client said so.** The
  first attempt offered ARGB8888/XRGB8888 — the guess a Linux compositor
  makes — and got:

  ```
  zwp_linux_buffer_params_v1: Format DrmFourcc(AB24)/34324241 is not supported.
  ```

  AB24 is ABGR8888: Android's `HAL_PIXEL_FORMAT_RGBA_8888`. Offering both
  channel orders removed the protocol error. Worth keeping as a lesson —
  the client will name the format it wants if you let it fail.

### What blocks the next run

`liwd` restarts the session out from under the experiment. Its supervisor
sees "Android boot did not complete" while the guest is still coming up
against the nested compositor, restarts it, and the restart uses liwd's own
environment — so the container goes back to `wayland-0` and the test is over
before a frame arrives:

```
WARN  liwd: session unhealthy strike=1 failures=["Android boot did not complete"]
INFO  liw_core::session: starting session (detached)
```

So testing the embedded path needs liwd stopped, or taught to start the
session with the compositor's socket. That is the next thing to fix, and it
is a liwd change rather than a compositor one.

## Second run: Android renders into it (2026-08-29)

With `liwd` stopped so it could not restart the session, and one thing
fixed, the whole path works:

```
connected=true size=Some((2560, 1440)) commits=111 buffer=Some("dmabuf DrmFourcc(AB24)")
```

Real dmabuf frames, in the format hwcomposer asked for, arriving at a
**median gap of 16.62 ms** — 60 FPS. Android boots in about 10 seconds
against the nested compositor.

### The fix, and the guess it disproved

`wl_seat`. The first version left it out on the reasoning that game input
already bypasses the compositor through the touch pipe, so a seat bought
nothing. That was wrong, and not subtly:

* with no seat, `android.hardware.graphics.composer@2.1::IComposer` never
  registers at all. SurfaceFlinger waits for it in a one-second retry loop
  forever and Android never finishes booting. Nothing in the log mentions
  Wayland — the composer dies before it gets that far, so the symptom
  points nowhere near the cause.
* a control run against KWin's socket, everything else identical, booted in
  15 seconds. That is what ruled out the environment and left the seat.

The seat carries keyboard, pointer and touch capabilities and delivers no
events. Being there is what the client checks for.

### What the run also showed

* **The surface is 2560x1440, not the 1280x720 we advertised.** Android
  sizes itself from `waydroid.display_width/height`, which come from the
  host display, not from our `wl_output` mode. Controlling the embedded
  size means setting those properties, not just configuring the output.
* **Nothing is drawn.** The compositor accepts the buffers and drops them.
  Importing them as a gpui texture is the next step and the hard one.

### Still unproven

Whether a frame can cross into gpui at all. Everything up to the buffer
arriving is now measured; everything after it is still the design document.
