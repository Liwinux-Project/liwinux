//! `liw keymap` — try the input engine with a real keyboard.

use anyhow::{Context, Result};
use liw_core::input::{
    backend::TouchBackend,
    capture::{self, translate, GrabbedDevice},
    Engine, InputEvent, Profile, ScreenMap, TouchAction, TriggerKind, UinputBackend,
};
use std::path::PathBuf;

pub fn list_devices() -> Result<()> {
    let devs = capture::discover();
    if devs.is_empty() {
        println!("No input device found.");
        println!("Is the user in the 'input' group?  ->  groups | grep input");
        return Ok(());
    }
    // Mark the device ACTUALLY IN USE, not the guessed one.
    //
    // This used to show the auto-selection result as "default". While the
    // config used a completely different device, an unrelated one appeared
    // marked in the list, which made diagnosis harder.
    let cfg = liw_core::Config::load();
    let want = |p: &std::path::Path, cfgp: &Option<PathBuf>| -> bool {
        cfgp.as_ref().is_some_and(|c| {
            c == p || c.canonicalize().ok() == p.canonicalize().ok()
        })
    };
    println!("{:<20} {:<9} {:<5} {:<7} {}", "PATH", "KIND", "SCORE", "VIRTUAL", "NAME");
    for d in &devs {
        let mark = if want(&d.path, &cfg.keyboard) { "  <- CONFIGURED keyboard" }
            else if want(&d.path, &cfg.mouse) { "  <- CONFIGURED mouse" }
            else { "" };
        println!("{:<20} {:<9} {:<5} {:<7} {}{}",
            d.path.display(), format!("{:?}", d.kind), d.typing_score,
            if d.virtual_device { "yes" } else { "no" }, d.name, mark);
    }
    println!();
    println!("SCORE = likelihood of being a real typing keyboard (out of 22).");
    if cfg.keyboard.is_none() {
        println!("WARNING: no keyboard configured — auto-selection will be used.");
    }
    // The score ALONE IS NOT ENOUGH: several nodes of one keyboard can score
    // 22 while only one actually sends keys. Measuring is mandatory.
    let tops = devs.iter().filter(|d| !d.virtual_device && d.typing_score >= 22).count();
    if tops > 1 {
        println!();
        println!("{tops} devices tied on score — the score does not say which one");
        println!("ACTUALLY sends keys. To measure:  liw keymap detect --save");
    }
    Ok(())
}

/// Runs the engine with real input and prints the touches it produces.
///
/// Nothing is injected into Android — this verifies mapping correctness without
/// touching the container at all.
pub async fn test_profile(
    profile_path: PathBuf,
    device: Option<PathBuf>,
    grab: bool,
    screen: (u32, u32),
    inject: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("could not read profile: {}", profile_path.display()))?;
    let profile = Profile::from_toml(&text).context("invalid profile")?;
    println!("Profil : {} ({})", profile.name, profile.package);
    println!("Bindings: {}", profile.bindings.len());

    let devs = capture::discover();
    // Order: explicit -d > saved calibration > rough guess.
    // The guess is a last resort and unreliable, so the user is warned.
    let cfg = liw_core::Config::load();
    let target = match device.or(cfg.keyboard) {
        Some(p) => p,
        None => {
            eprintln!("warning: no calibrated keyboard, guessing.");
            eprintln!("         for the real one:  liw keymap detect --save");
            capture::best_keyboard(&devs)
                .map(|d| d.path.clone())
                .context("no keyboard found — check with 'liw keymap devices'")?
        }
    };
    let name = devs.iter().find(|d| d.path == target)
        .map(|d| d.name.clone()).unwrap_or_default();
    println!("Cihaz  : {} ({})", target.display(), name);

    if grab {
        println!();
        println!("!! GRAB ON: keys will NOT reach the desktop.");
        println!("!! Ctrl+C therefore DOES NOT REACH THE TERMINAL.");
        println!("!! ESCAPE KEY: ESC  (three times in a row)");
        println!("!! If the program dies the kernel releases the grab anyway.");
    } else {
        println!("Grab   : off (keys also reach the desktop; use --grab to grab)");
    }
    println!("Ekran  : {}x{}", screen.0, screen.1);
    println!("---- press keys (Ctrl+C to quit) ----");

    // Injection backend. Discovering devices AFTER creating the virtual one
    // risks capturing our own device; the target device was therefore already
    // chosen above.
    let mut backend: Option<UinputBackend> = if inject {
        let mut b = UinputBackend::new(ScreenMap::default())
            .context("could not create the virtual touchscreen")?;
        // libinput/KWin may take a few hundred ms to notice the device.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        println!("Enjeksiyon: uinput  ->  {}", b.dev_nodes().join(", "));
        println!("  WARNING: touches are in SCREEN space. If Waydroid is not");
        println!("           fullscreen they land wrong (ScreenMap compensates).");
        Some(b)
    } else {
        println!("Injection: OFF (printing only; enable with --inject)");
        None
    };

    let dev = GrabbedDevice::open(&target, grab)?;
    let mut stream = dev.into_stream()?;
    let mut engine = Engine::new(profile);

    // Gesture clock. Swipes advance in intermediate steps; without this tick a
    // gesture starts but never completes. 4ms ~ 250Hz: even on a 144Hz display
    // every frame gets at least one step.
    let t0 = std::time::Instant::now();
    let mut lat = liw_core::input::LatencyStats::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(4));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Escape hatch. While grabbed, Ctrl+C does not reach the terminal, so we
    // must catch ESC in OUR OWN loop; otherwise the user's keyboard stays stuck
    // in the program. The three-times rule prevents an accidental exit from
    // pressing ESC in the game.
    const ESC: u16 = 1;
    let mut esc_streak = 0u8;

    loop {
        tokio::select! {
            ev = stream.next_event() => {
                let ev = ev.context("event stream broke")?;
                let ev_time = ev.timestamp();
                let Some(input) = translate(&ev) else { continue };

                if grab {
                    match input {
                        InputEvent::Press(TriggerKind::Key(ESC)) => {
                            esc_streak += 1;
                            if esc_streak >= 3 {
                                println!();
                                println!("ESC x3 — exiting, releasing the grab");
                                let acts = engine.set_enabled(false);
                                for act in &acts { print!("[exit] "); print_action(act, screen); }
                                if let Some(b) = backend.as_mut() { let _ = b.dispatch(&acts); }
                                break;
                            }
                            println!("  (ESC {esc_streak}/3 — press again to exit)");
                        }
                        InputEvent::Press(_) => esc_streak = 0,
                        _ => {}
                    }
                }

                // Actions produced by tick MUST NOT be dropped: the previous
                // gesture's last MOVE and UP may be there. Dropping them leaves
                // a finger stuck on screen and breaks later gestures.
                let mut acts = engine.tick(t0.elapsed().as_millis() as u64);
                acts.extend(engine.handle(input));
                for act in &acts { print_action(act, screen); }
                if let Some(b) = backend.as_mut() {
                    if let Err(e) = b.dispatch(&acts) {
                        eprintln!("  !! injection error: {e}");
                    } else if !acts.is_empty() {
                        lat.record(ev_time);
                    }
                }
            }
            _ = ticker.tick(), if engine.has_pending() => {
                let acts = engine.tick(t0.elapsed().as_millis() as u64);
                for act in &acts { print_action(act, screen); }
                if let Some(b) = backend.as_mut() {
                    if let Err(e) = b.dispatch(&acts) {
                        eprintln!("  !! injection error: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!();
                let acts = engine.set_enabled(false);
                for act in &acts { print!("[exit] "); print_action(act, screen); }
                if let Some(b) = backend.as_mut() { let _ = b.dispatch(&acts); }
                println!();
                println!("{}", lat.report("key -> touch write (liwinux layer only)"));
                println!("done — grab released");
                break;
            }
        }
    }
    Ok(())
}

fn print_action(act: &TouchAction, (w, h): (u32, u32)) {
    match act {
        TouchAction::Down { id, at } => {
            let (x, y) = at.to_px(w, h);
            println!("  DOWN  p{id}  ({x:>5},{y:>5})   norm({:.3},{:.3})", at.x, at.y);
        }
        TouchAction::Move { id, at } => {
            let (x, y) = at.to_px(w, h);
            println!("  MOVE  p{id}  ({x:>5},{y:>5})   norm({:.3},{:.3})", at.x, at.y);
        }
        TouchAction::Up { id } => println!("  UP    p{id}"),
    }
}

/// Sends a single touch — an injection test independent of the mapping.
///
/// The reason for this separation is diagnosis: if the game does not react, you
/// need to know whether the problem is in the mapping or the injection path.
pub async fn poke(
    x: f32, y: f32, hold_ms: u64, drag_to: Option<(f32, f32)>,
    map: ScreenMap, delay_s: u64, force_uinput: bool,
) -> Result<()> {
    use liw_core::input::{Norm, WlTouchBackend};

    // The default is Waydroid's touch pipe: it bypasses the compositor chain
    // and does not clamp coordinates. This command is also the VERIFICATION
    // tool for that claim — pass a coordinate outside 0..1 and watch the touch
    // (see docs/mouse-aim.md).
    //
    // If the pipe cannot be acquired we fall back to uinput; erroring would
    // make this diagnostic command depend on the helper, and it would be
    let pipe = if force_uinput { None } else {
        match liw_core::HelperClient::connect().await {
            Ok(h) => match h.open_touch_pipe().await {
                Ok(t) => Some(t),
                Err(e) => { eprintln!("warning: could not acquire the touch pipe: {e}"); None }
            },
            Err(e) => { eprintln!("warning: could not connect to liwd-helper: {e}"); None }
        }
    };
    // Off-screen coordinates are meaningful ONLY on the pipe path: on the
    // uinput path libinput clamps anyway and not clamping misleads.
    let offscreen_ok = pipe.is_some();

    let mut b: Box<dyn liw_core::input::TouchBackend> = match pipe {
        Some((f, w, h)) => {
            println!("Backend: Waydroid touch pipe ({w}x{h}) — bypassing the compositor");
            println!("Coordinates are NOT clamped: values outside 0..1 are sent too.");
            Box::new(WlTouchBackend::from_pipe(f, w, h)
                .context("could not set up the touch backend")?)
        }
        None => {
            let mut u = UinputBackend::new(map)
                .context("could not create the virtual touchscreen")?;
            println!("Backend: uinput -> libinput -> KWin -> wl_touch");
            println!("Sanal cihaz: {}", u.dev_nodes().join(", "));
            println!("Harita: origin({:.3},{:.3}) scale({:.3},{:.3}) invert({},{})",
                map.origin_x, map.origin_y, map.scale_x, map.scale_y,
                map.invert_x, map.invert_y);
            println!("Waiting for KWin/libinput to notice the device...");
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            Box::new(u)
        }
    };

    let point = |px: f32, py: f32| if offscreen_ok {
        Norm::unclamped(px, py)
    } else {
        Norm::new(px, py)
    };

    // Touches are routed BY POSITION, not by focus. When the command is run
    // from a terminal and the terminal window is over the target, the touch
    // goes THERE and the test silently gives the wrong answer.
    if delay_s > 0 {
        println!();
        println!("Bring the target window to the FRONT within {delay_s} seconds:");
        for i in (1..=delay_s).rev() {
            print!("  {i}... ");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        println!();
    }

    let from = point(x, y);
    println!("DOWN  ({x:.3}, {y:.3})");
    b.dispatch(&[TouchAction::Down { id: 0, at: from }])
        .context("could not send DOWN")?;

    if let Some((tx, ty)) = drag_to {
        // Split the drag into steps: a single jump is not recognised as a
        // oyunlar ara hareketleri bekler.
        const STEPS: u32 = 12;
        let step_ms = (hold_ms / STEPS as u64).max(4);
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let at = point(x + (tx - x) * t, y + (ty - y) * t);
            b.dispatch(&[TouchAction::Move { id: 0, at }])
                .context("could not send MOVE")?;
            tokio::time::sleep(std::time::Duration::from_millis(step_ms)).await;
        }
        println!("MOVE  ({tx:.3}, {ty:.3})  [in {STEPS} steps]");
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
    }

    b.dispatch(&[TouchAction::Up { id: 0 }]).context("could not send UP")?;
    println!("UP");
    // If the device disappears immediately the last events can be lost.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    println!("bitti");
    Ok(())
}

/// Listens to every keyboard at once and prints which device produces which code.
///
/// For diagnosis: when "I pressed a key and nothing happened", the first thing
/// to ask is whether we are listening to the right device. On multi-interface
/// keyboards (Razer, Logitech) the letter keys may be on an unexpected node.
pub async fn watch(device: Option<PathBuf>) -> Result<()> {
    let devs = capture::discover();
    let targets: Vec<_> = match device {
        Some(p) => vec![p],
        None => devs.iter()
            .filter(|d| !matches!(d.kind, capture::DeviceKind::Pointer))
            .filter(|d| !d.virtual_device)
            .map(|d| d.path.clone())
            .collect(),
    };
    if targets.is_empty() {
        println!("No device to listen to.");
        return Ok(());
    }

    println!("Dinlenen cihazlar:");
    for t in &targets {
        let n = devs.iter().find(|d| &d.path == t)
            .map(|d| d.name.as_str()).unwrap_or("?");
        println!("  {}  {}", t.display(), n);
    }
    println!();
    println!("Press keys — you will see which device produces which code.");
    println!("The 'code' column is the value to use in a profile. Ctrl+C to quit.");
    println!();
    println!("{:<20} {:<8} {:<10} {}", "DEVICE", "CODE", "STATE", "NAME");

    let mut set = tokio::task::JoinSet::new();
    for path in targets {
        let name = devs.iter().find(|d| d.path == path)
            .map(|d| d.name.clone()).unwrap_or_default();
        // NO grab: taking the user's keyboard during diagnosis is unacceptable.
        let dev = match GrabbedDevice::open(&path, false) {
            Ok(d) => d,
            Err(e) => { eprintln!("  skipped {}: {e}", path.display()); continue; }
        };
        let mut stream = match dev.into_stream() {
            Ok(s) => s,
            Err(e) => { eprintln!("  could not set up stream {}: {e}", path.display()); continue; }
        };
        let short = path.file_name().map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        set.spawn(async move {
            while let Ok(ev) = stream.next_event().await {
                if let Some(input) = translate(&ev) {
                    let (kod, durum) = match input {
                        InputEvent::Press(TriggerKind::Key(k)) => (k.to_string(), "BASILDI"),
                        InputEvent::Release(TriggerKind::Key(k)) => (k.to_string(), "released"),
                        InputEvent::Press(t) => (format!("{t:?}"), "BASILDI"),
                        InputEvent::Release(t) => (format!("{t:?}"), "released"),
                        InputEvent::MouseMove { .. } => continue,
                    };
                    println!("{:<20} {:<8} {:<10} {}", short, kod, durum, name);
                }
            }
        });
    }

    tokio::signal::ctrl_c().await.ok();
    set.abort_all();
    println!();
    println!("bitti");
    Ok(())
}

/// Identifies the keyboard by calibration: press a key, whichever device
/// produced it wins.
///
/// Measurement instead of auto-detection. On multi-interface keyboards the
/// capability list is NOT distinctive — on this machine both interfaces of the
/// same keyboard advertise the full key range but only one produces events.
pub async fn detect(save: bool, mouse_mode: bool, hotkey_mode: bool) -> Result<()> {
    let devs = capture::discover();
    let cands: Vec<_> = devs.iter()
        .filter(|d| !d.virtual_device)
        .filter(|d| if mouse_mode {
            // In mouse calibration, pointer and combo devices are candidates.
            matches!(d.kind, capture::DeviceKind::Pointer | capture::DeviceKind::Combo)
        } else {
            !matches!(d.kind, capture::DeviceKind::Pointer)
        })
        .collect();
    if cands.is_empty() {
        println!("No candidate keyboard. Are you in the 'input' group?  ->  groups");
        return Ok(());
    }
    if mouse_mode {
        println!("Listening to {} candidates. MOVE YOUR MOUSE NOW...", cands.len());
    } else if hotkey_mode {
        println!("listening on {} candidates.", cands.len());
        println!("PRESS THE KEY YOU WANT AS THE GAME-MODE HOTKEY NOW...");
        println!("(pick a key you do not use in the game)");
    } else {
        println!("Listening to {} candidates. PRESS A KEY NOW...", cands.len());
    }
    println!();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(PathBuf, String, u16)>(8);
    let mut set = tokio::task::JoinSet::new();
    for d in &cands {
        let (path, name, tx) = (d.path.clone(), d.name.clone(), tx.clone());
        let Ok(dev) = GrabbedDevice::open(&path, false) else { continue };
        let Ok(mut stream) = dev.into_stream() else { continue };
        set.spawn(async move {
            while let Ok(ev) = stream.next_event().await {
                match translate(&ev) {
                    // In mouse mode we look for MOTION: a key press does not
                    // identify a mouse, most keyboards have keys too.
                    Some(InputEvent::MouseMove { dx, dy }) if mouse_mode
                        && (dx.abs() + dy.abs()) > 2.0 =>
                    {
                        let _ = tx.send((path.clone(), name.clone(), 0)).await;
                        return;
                    }
                    Some(InputEvent::Press(TriggerKind::Key(k))) if !mouse_mode => {
                        let _ = tx.send((path.clone(), name.clone(), k)).await;
                        return;
                    }
                    _ => {}
                }
            }
        });
    }
    drop(tx);

    let found = tokio::select! {
        v = rx.recv() => v,
        _ = tokio::time::sleep(std::time::Duration::from_secs(20)) => {
            println!("Timed out — no key detected."); None
        }
        _ = tokio::signal::ctrl_c() => { println!("cancelled"); None }
    };
    set.abort_all();

    let Some((path, name, code)) = found else { return Ok(()) };
    if mouse_mode {
        println!("Fare   : {}  ({})", path.display(), name);
    } else if hotkey_mode {
        println!("Hotkey code: {code}   (device: {})", path.display());
    } else {
        println!("Klavye : {}  ({})", path.display(), name);
        println!("First key code: {code}");
    }

    if save {
        // Write a STABLE path instead of eventN. The numbers change across
        // reboots: in reality event23 in the config was the keyboard and after
        // a reboot became an audio device, and the keymapper silently stopped
        // working.
        let path = match liw_core::input::capture::stable_path(&path) {
            Some(stable) => {
                println!("Stable path: {}", stable.display());
                stable
            }
            None => {
                println!("WARNING: no stable link under /dev/input/by-id for this \
                          device; eventN may change across reboots.");
                path
            }
        };
        let mut cfg = liw_core::Config::load();
        if mouse_mode { cfg.mouse = Some(path); }
        else if hotkey_mode { cfg.hotkey_game_mode = Some(code); }
        else { cfg.keyboard = Some(path); }
        let p = cfg.save().context("could not save the configuration")?;
        println!("Kaydedildi: {}", p.display());
        println!("The keymapper will now use this device.");
    } else {
        println!("(to save: liw keymap detect --save)");
    }
    Ok(())
}

/// Toggles the Android touch indicator (for calibration).
pub async fn overlay(on: bool) -> Result<()> {
    let h = liw_core::HelperClient::connect().await
        .context("could not connect to liwd-helper — is it running? \
                  (sudo systemctl status liwd-helper)")?;
    h.set_pointer_location(on).await.context("could not set the indicator")?;
    println!("touch indicator: {}", if on { "ON" } else { "off" });
    if on {
        println!("Every touch is now marked on screen — you can see where it lands.");
    }
    Ok(())
}

/// Sweeps along the X axis to find which coordinates reach the window.
///
/// To measure the coordinate mapping instead of guessing it. The user reports
/// which points appear on screen and WHERE.
pub async fn sweep(axis: char, count: u32, gap_ms: u64) -> Result<()> {
    use liw_core::input::Norm;

    let mut b = UinputBackend::new(ScreenMap::default())
        .context("could not create the virtual touchscreen")?;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    println!("Sweep: {} axis, {count} points, {gap_ms}ms apart", axis);
    println!("Watch the Android screen — note which numbers appear.");
    println!();

    for i in 0..count {
        let t = i as f32 / (count - 1).max(1) as f32;
        let at = if axis == 'y' { Norm::new(0.5, t) } else { Norm::new(t, 0.5) };
        println!("  #{:<2}  {} = {:.2}   ({:.3}, {:.3})", i, axis, t, at.x, at.y);
        b.dispatch(&[TouchAction::Down { id: 0, at }])?;
        tokio::time::sleep(std::time::Duration::from_millis(160)).await;
        b.dispatch(&[TouchAction::Up { id: 0 }])?;
        tokio::time::sleep(std::time::Duration::from_millis(gap_ms)).await;
    }
    println!();
    println!("Tell me the first and last visible number — I will compute the mapping.");
    Ok(())
}

/// Watches the foreground game, loads its profile automatically and maps keys.
///
/// The engine lives in `liw_core::input::Runner`; `liwd` uses the same one. The
/// only difference here is who supplies the foreground and where output goes.
pub async fn run(grab: bool, poll_ms: u64) -> Result<()> {
    use liw_core::input::{Runner, RunnerConfig, RunnerEvent, ScreenMap, Store};

    let helper = liw_core::HelperClient::connect().await
        .context("could not connect to liwd-helper — is it running? \
                  (systemctl status liwd-helper)")?;

    let devs = capture::discover();
    let cfg = liw_core::Config::load();
    let device = cfg.keyboard.clone()
        .or_else(|| capture::best_keyboard(&devs).map(|d| d.path.clone()))
        .context("no keyboard — calibrate it with 'liw keymap detect --save'")?;
    let dev_name = devs.iter().find(|d| d.path == device)
        .map(|d| d.name.clone()).unwrap_or_default();

    let store = Store::discover();
    println!("Klavye  : {} ({})", device.display(), dev_name);
    println!("Profiles: {} loaded", store.len());
    for p in &store.problems {
        eprintln!("  warning: {} — {}", p.path.display(), p.error);
    }
    println!("Grab    : {}", if grab { "will be taken while a profile is active" } else { "off" });
    println!("Yoklama : {poll_ms}ms");
    println!("Ctrl+C to quit. While grabbed, exit with: ESC x3");
    println!();

    // Touch pipe: if acquired the compositor chain is bypassed and aim runs in
    // unbounded mode. If not, the uinput path — it works, but aim resets at the
    // edge (`docs/mouse-aim.md`).
    let pipe = match helper.open_touch_pipe().await {
        Ok((f, w, h)) => {
            println!("Touch   : Waydroid pipe ({w}x{h}) — unbounded aim ENABLED");
            Some((f, (w, h)))
        }
        Err(e) => {
            eprintln!("Touch   : uinput (could not acquire the pipe: {e})");
            eprintln!("          Aim runs in BOUNDED mode: the finger resets at the edge.");
            None
        }
    };

    let mut runner = Runner::new(
        RunnerConfig {
            device, mouse: cfg.mouse.clone(), grab,
            hotkey: cfg.hotkey_game_mode, screen_map: ScreenMap::default(),
            screen_px: pipe.as_ref().map(|(_, px)| *px).unwrap_or((2560, 1440)),
        }, store);
    if let Some((f, px)) = pipe {
        runner = runner.with_touch_pipe(f, px);
    }

    // In debug mode there is NO host focus gate: the KWin notification goes to
    // liwd and this process cannot receive it. The gate is therefore treated as
    // permanently open and the user is warned — backgrounding the game means
    // keys inject touches into the desktop.
    eprintln!("WARNING: this mode does NOT enforce the host focus gate.");
    eprintln!("         If you background the game, keys touch the desktop.");
    eprintln!("         For everyday use: liw keymap start");
    eprintln!();
    let (_focus_tx, focus_rx) = tokio::sync::watch::channel(true);
    let (fg_tx, fg_rx) = tokio::sync::mpsc::channel::<String>(4);
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<RunnerEvent>(16);
    let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);

    // Foreground polling in a separate task: the input path must not block.
    let poll = tokio::spawn(async move {
        let mut t = tokio::time::interval(std::time::Duration::from_millis(poll_ms));
        loop {
            t.tick().await;
            match helper.foreground_package().await {
                Ok(p) if !p.is_empty() => { let _ = fg_tx.try_send(p); }
                Ok(_) => {}
                Err(e) => eprintln!("could not query the foreground: {e}"),
            }
        }
    });

    let printer = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            match e {
                RunnerEvent::ProfileActivated { package, profile } =>
                    println!("[{package}] profile ACTIVE: {profile}"),
                RunnerEvent::ProfileCleared { package } =>
                    println!("[{package}] no profile — mapping off"),
                RunnerEvent::OverlayPaused { package } => println!(
                    "\n  ⏸  {package} came over the game — mapping paused.\n\
                     \x20    The mouse is free; it returns by itself when the layer closes.\n"),
                RunnerEvent::Grabbed => println!("  grab taken"),
                RunnerEvent::Ungrabbed => println!("  grab released"),
                RunnerEvent::GameModeOn => println!("  GAME MODE ON (grab + mapping)"),
                RunnerEvent::GameModeOff => println!("  game mode off (mouse free)"),
                RunnerEvent::FocusGained => println!("  Waydroid odakta"),
                RunnerEvent::FocusLost => println!("  Waydroid is not focused"),
                RunnerEvent::EscapeRequested => println!("ESC x3 — exiting"),
            }
        }
    });

    let sd = sd_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        println!();
        println!("exiting");
        let _ = sd.send(true);
    });

    let lat = runner.run(fg_rx, focus_rx, sd_rx, Some(ev_tx)).await
        .context("the keymapper stopped with an error")?;
    poll.abort();
    printer.abort();

    println!();
    println!("=== LATENCY (liwinux layer only) ===");
    println!("{}", lat.report("key -> touch write"));
    println!();
    println!("COVERS: from the kernel evdev timestamp to the uinput write.");
    println!("EXCLUDES: libinput, KWin, wl_touch, Waydroid, the Android input");
    println!("pipeline and the game's own reaction time.");
    println!("bitti");
    Ok(())
}

// --- keymapper control through liwd ---

const BUS: &str = "id.liwinux.Manager1";
const OBJ: &str = "/id/liwinux/Manager1";

async fn daemon() -> Result<zbus::Proxy<'static>> {
    let conn = zbus::Connection::session().await
        .context("could not connect to the session bus")?;
    let p = zbus::Proxy::new(&conn, BUS, OBJ, BUS).await
        .context("could not set up the liwd proxy")?;
    p.introspect().await
        .context("liwd is not running — systemctl --user status liwd")?;
    Ok(p)
}

/// Starts the keymapper inside liwd: it keeps running after the terminal closes.
pub async fn daemon_start(grab: bool) -> Result<()> {
    let p = daemon().await?;
    p.call::<_, _, ()>("StartKeymapper", &(grab,)).await
        .context("StartKeymapper failed")?;
    println!("keymapper started (inside liwd){}",
        if grab { ", profil etkinken kilitlenecek" } else { "" });
    println!("For status: liw keymap status");
    Ok(())
}

pub async fn daemon_stop() -> Result<()> {
    let p = daemon().await?;
    p.call::<_, _, ()>("StopKeymapper", &()).await
        .context("StopKeymapper failed")?;
    println!("keymapper durduruldu");
    Ok(())
}

pub async fn daemon_status() -> Result<()> {
    let p = daemon().await?;
    let json: String = p.call("KeymapperStatus", &()).await
        .context("KeymapperStatus failed")?;
    let st: liw_core::input::RunnerState = serde_json::from_str(&json)
        .context("could not parse the state")?;

    println!("Running      : {}", if st.running { "yes" } else { "no" });
    println!("Foreground   : {}", st.foreground.as_deref().unwrap_or("-"));
    println!("Active profile : {}", st.active_profile.as_deref().unwrap_or("none"));
    println!("Game mode    : {}", if st.game_mode { "ON" } else { "off (mouse free)" });
    println!("Host focus   : {}", if st.host_focused { "Waydroid" } else { "another window" });
    println!("Grab         : {}", if st.grabbed { "on" } else { "off" });
    if st.latency_p50_us > 0 || st.latency_p99_us > 0 {
        println!("Latency      : p50 {:.2} ms   p99 {:.2} ms  (liwinux layer only)",
            st.latency_p50_us as f64 / 1000.0, st.latency_p99_us as f64 / 1000.0);
    }
    if !st.running {
        println!();
        println!("To start: liw keymap start");
    }
    Ok(())
}
