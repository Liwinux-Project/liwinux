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

## Third run: the game is on screen (2026-08-29)

`liw-compositor` now opens a window and paints the guest into it. Special
Forces Group 2 was captured running inside a window titled
"liwinux — Android", at **60 paints per second** while the guest committed
at 120/s.

The renderer is GLES over EGL through smithay's winit backend, on the
RTX 3060. The guest's dmabuf becomes a GL texture with no copy. That is the
import the design document called the hard join — solved into GLES rather
than into gpui, but the buffer does cross, and the same import is what a
gpui path would need.

### What each mistake cost, since none of them announced itself

* **No `on_commit_buffer_handler`** and the surface tree yields no elements:
  every counter still climbs, a dmabuf is still named, and the window stays
  blank.
* **Reading the buffer after that handler** reports `buffer=None` forever,
  because the handler takes the pending state as it hands it to the
  renderer. A whole run was logged as having no buffer while frames were
  arriving perfectly well.
* **No paint throttle** meant 180 fps against a launcher that had stopped
  committing at 133 — a full GPU pass per frame to redraw an unchanged
  picture. Painting only when the commit counter moves fixed it.
* **The scale argument to `render_elements_from_surface_tree` only moves the
  element.** Its SIZE comes from the OUTPUT's scale, read by the damage
  tracker at render time. Passing 0.5 there and expecting a half-size
  picture left the game's buttons running off the right edge for two runs.
* **Fitting to the 1x1 placeholder** the session manager attaches gives a
  scale of 1280, which is then advertised to the client as its output scale.
  Not a rounding error — nonsense on the wire, and Android stopped booting.
* **Updating the output every pass** sent the client hundreds of wl_output
  changes a second, because the size it compared against is only refreshed
  when a frame is painted.

### Scale-to-fit, verified

Repeated after `systemctl restart waydroid-container` cleared the stale
state. Android boots against the compositor in 10 seconds, and the fit is
computed from what the guest actually sends:

```
fitted guest=(2560, 1440) window=(1280, 720) fit=0.5
```

The whole Android screen now lands inside the window. Before the fix the
game's menu showed two buttons running off the right edge; after it, all
four buttons, the player panel and the version string are on screen.

Sustained over five-second windows:

```
guest 120.3 commits/s   painted 60.2 /s
guest 120.0 commits/s   painted 60.0 /s
guest 119.9 commits/s   painted 60.0 /s
```

The guest commits at twice the frame rate — two commits per frame — and the
throttle paints once per new frame. 60 FPS.

### A warning about test method

Ten session restarts in a row left Waydroid unable to boot **against KWin's
own socket**. The failure looked exactly like a compositor bug and was not
one; a control run against the normal socket is what separated them. Any
session of this kind should re-run that control before blaming the code,
and `systemctl restart waydroid-container` is the reset.

### Still unproven

Whether a frame can cross into gpui. Everything up to and including drawing
it in a window is now measured; the gpui join is still the design document.
