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
