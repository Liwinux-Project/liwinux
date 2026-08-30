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


## Into gpui (2026-08-30)

### The import question, answered by looking

gpui has **no external-texture path on Linux**. Its `surface` element is
`#[cfg(target_os = "macos")]` and takes a `CVPixelBuffer`; `paint_surface`
is the same. So the choice was a patch to the pinned Zed fork, or reading
each frame back to bytes.

That was measured before anything was built on it. Reading a 1280x720 frame
back costs **0.80 to 2.02 ms**, 5 to 12 per cent of a 16.67 ms frame. Worth
paying while the rest is brought up; the first thing to delete when the fork
grows a texture path.

Two details the measurement itself taught:

* The readback must not be **waited on** between render and swap. Doing so
  left EGL with a different current draw surface, the context was lost on
  the next present, and the whole compositor exited. Ask for the copy while
  the framebuffer is bound; wait for it after the frame is on screen.
* Capture as `Argb8888`, not `Abgr8888`. DRM names channels
  most-significant-first in a little-endian word, so ARGB8888 lands in
  memory as B, G, R, A — already what gpui's `RenderImage` wants. Capturing
  the guest's own ABGR8888 would need a per-pixel swap: 55 million byte
  swaps a second at 720p60, for nothing.

### What is built

* `liw_compositor::headless` renders into an offscreen GLES texture on a
  headless EGL context (`PLATFORM_DEVICE_EXT`), no window of its own.
* `liw_compositor::embedded::spawn` runs the whole Wayland side on a thread
  and leaves the newest frame in a one-slot mailbox. One slot, not a queue:
  a UI that is behind wants the current picture, not a backlog.
* `liw-ui` has a **Play** page that draws that frame, a frame pump that
  repaints only when the serial changes, and **F11** for immersive mode —
  chrome hidden, Android alone. `liw-ui --play` opens straight into it.
* The render size follows the window. Confirmed in the log: with the real
  window open, `fitted guest=(1280, 720) view=(1120, 668) fit=0.875`.

### Where it stops

Android's composer does not come up against the headless compositor.
`android.hardware.graphics.composer@2.1::IComposer` never registers,
SurfaceFlinger waits for it forever, and boot never completes — the same
shape as the missing-`wl_seat` failure, though the seat is present now.

A control run against KWin's socket booted in 10 seconds immediately
afterwards, so this is the embedded path and not a stale machine.

The guest **does** connect: the socket is hosted by `liw-ui`, the toplevel
is created and configured, and the fit is computed against the real window.
What is missing is between that and the composer starting.

The difference from the working standalone binary is the renderer: winit's
window-backed EGL there, `EGLDevice` headless here. That is the first place
to look, and the next run should compare what each advertises to the client
rather than guessing which of the two matters.

## Narrowing the embedded failure (2026-08-30)

`liw-compositor --headless` runs the embedded code path with no window and
no gpui around it. Against it, Android does not boot; a control run against
KWin's socket ten seconds later does. So **gpui is not the cause** — the
process it lives in makes no difference.

What the headless run establishes, from its own log:

```
guest connected
toplevel created and configured size=(1280, 720)
(true, Some((1, 1)), 2, Some("shm Argb8888")) frame=Some((1280, 720, 2))
```

The compositor is working. The guest connects, creates a toplevel, takes the
configure, attaches the 1x1 placeholder the session manager always attaches,
and **two frames are rendered and read back**. Then the guest goes quiet
without disconnecting — the windowed run logs `toplevel destroyed` at this
point and this one never does.

Android's side is the same shape every time:
`android.hardware.graphics.composer@2.1::IComposer` never registers,
SurfaceFlinger is waited for, and system_server's watchdog eventually kills
it inside `DisplayManagerService.<init>`. That is all downstream of the
composer, not a second fault.

### The bisect was not clean, and saying so is the point

`--headless` swaps **two** things at once: the renderer (winit's
window-backed EGL for a headless `EGLDevice`) and the loop (`main.rs` for
`embedded.rs`). It rules out gpui. It does not say which of those two
remaining halves matters, and treating it as though it did would be exactly
the reasoning that has cost this work the most time.

The next run has to change one. Either drive `embedded.rs`'s loop with a
winit-backed renderer, or drive `main.rs`'s loop against an offscreen
target. Whichever is easier — the answer is the same either way.

### What is known not to be the difference

* **The seat.** Both runs log `add_keyboard` and `Loaded Keymap
  name="Turkish"`; the globals advertised are built by the same
  `Compositor::new` in both.
* **The EGL context.** The headless one comes up on `PLATFORM_DEVICE_EXT`
  against the RTX 3060 with `EGL_EXT_image_dma_buf_import` present, and it
  renders and reads back frames successfully.
* **A stale machine.** Ruled out by a control run immediately afterwards,
  which is now standard practice for this work.

## Android inside the gpui window (2026-08-30)

Done. `liw-ui --play` hosts the Wayland socket, Waydroid connects to it, and
Special Forces Group 2 renders inside the liwinux window with the nav strip
above it.

```
fitted guest=(2560, 1440) view=(1120, 668) fit=0.4375
```

The view size is the real window minus the nav strip, and the guest is
fitted into it. Through the standalone headless path, measured on the same
build: guest 120 commits/s, frames delivered at 60/s.

### The failure that was not a bug

Three runs of the embedded path failed with Android never booting, and the
last commit blamed the untested half of a two-variable bisect. That was
wrong. The same binary, unchanged except for one log line, booted Android
in ten seconds on the next run — and then delivered frames at 60/s.

So the embedded path was working the whole time and the failures were
**intermittent**. A control run against KWin's socket had passed before one
of them, which is exactly why it was believed to be the code: the control
that had caught the stale-machine problem twice before did not catch this.

The honest reading is that a control run proves the machine was healthy at
the moment it ran, not that it stayed healthy, and not that a failure a
minute earlier had the same cause. An intermittent failure needs repetition
to characterise, and three failures with no repeat of the success is not
enough evidence to name a culprit — which is what naming the renderer was.

### What is unverified

**F11.** The action is bound and the handler only fires on the Play page,
but pressing it needs a keyboard and every check here was done from a
script. The resize path is verified, because the fit follows the real
window size in the log above.

## The rail and the key mapper (2026-08-30)

A thin strip beside the picture, in the shape GameLoop uses: controls next to
the game rather than on top of it, so nothing a player needs is hidden behind
the thing they are playing. Three buttons — open the panel, map keys, fill the
window — and the panel opens to their detail.

### Mapping

Click where a button is on the game, then press the key that should press it.
Nothing is typed and no coordinates are entered; the thing being aimed at is
the running game rather than a screenshot of it.

What is stored is an **evdev code and a normalised position**, so a binding
survives a resized window and a different keyboard layout. That is the
existing model in `liw_input::profile::Trigger`, and honouring it meant a
name-to-code table: gpui reports the printed character, the profile wants the
physical key. The table covers letters, digits, arrows, modifiers, space and
the function keys, and **refuses** anything else rather than guessing —
binding the wrong physical key fails silently and only during play.

### One piece of arithmetic, not two

`liw_compositor::fit` decides both what the compositor renders and where a
click lands on the guest. It was moved there specifically so there is one
copy: two would drift, and the symptom would be taps landing somewhere other
than where they were placed — which reads as a broken input engine rather
than a broken editor.

The same applies to the rail's width. `sidebar::width()` is what the picture's
size is computed from, so a second opinion about how wide the rail is cannot
put every binding a few pixels off.

### Verified, and not

The rail renders and the layout arithmetic is right: the log shows
`view=(844, 668)` with the panel open and `view=(1076, 668)` with it closed,
against a 1120-wide window — 276 and 44 pixels, which is what
`sidebar::width` returns for each.

The mapper's own logic is covered by tests: placing, refusing a click in the
letterboxing, binding, refusing an unmappable key, the second binding of the
same key getting its own name, removing, and which bindings get markers.

**The click-and-press flow has not been exercised in the running UI.** Every
check here ran from a script, and pressing a key or clicking a marker needs
hands. The pieces are tested individually and the layout is right; that they
compose is inference, not measurement, until someone uses it.