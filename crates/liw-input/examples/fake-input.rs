//! A synthetic keyboard and mouse for the HOST, so the key mapper can be
//! driven without a hand.
//!
//! This is a development tool, not part of the product. It exists because the
//! interesting questions about game mode — does the grab happen, does W reach
//! Android, does mouse motion turn the view — can only be answered by real
//! evdev events arriving on the device the mapper opened.
//!
//! Pointing the mapper at these instead of the real keyboard and mouse also
//! means a test cannot take the desk hardware hostage: `EVIOCGRAB` on the
//! user's own mouse while they are watching is not an acceptable test.
//!
//! ```text
//! cargo run -p liw-input --example fake-input -- 30
//! ```
//!
//! The names deliberately do NOT start with `liwinux`: discovery skips those,
//! because that is how it avoids reading back the virtual touchscreen the
//! mapper itself writes to. A test device named that way is invisible to the
//! very code under test.
//!
//! Prints the two device nodes and then, after the given number of seconds,
//! sends: hotkey, W held for a second with mouse motion during it, W up,
//! hotkey. Enough to see a grab, a joystick and an aim in the log.

use std::{thread::sleep, time::Duration};

use evdev::{
    uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode, PropType,
    RelativeAxisCode,
};

/// evdev code for the key this machine's profile uses as the hotkey.
const KEY_HOTKEY: u16 = 41;

fn main() -> std::io::Result<()> {
    let wait: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30);

    let mut keys = AttributeSet::<KeyCode>::new();
    // The whole typing set, so `typing_score` rates this as a real keyboard
    // and `classify` calls it one. A device the mapper refuses to open tests
    // nothing.
    for k in [
        KeyCode::KEY_A, KeyCode::KEY_Z, KeyCode::KEY_Q, KeyCode::KEY_M,
        KeyCode::KEY_0, KeyCode::KEY_9, KeyCode::KEY_SPACE, KeyCode::KEY_ENTER,
        KeyCode::KEY_TAB, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT,
        KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTALT, KeyCode::KEY_CAPSLOCK,
        KeyCode::KEY_ESC, KeyCode::KEY_BACKSPACE, KeyCode::KEY_F1, KeyCode::KEY_F12,
        KeyCode::KEY_UP, KeyCode::KEY_DOWN, KeyCode::KEY_LEFT, KeyCode::KEY_RIGHT,
        KeyCode::KEY_W, KeyCode::KEY_S, KeyCode::KEY_D, KeyCode::new(KEY_HOTKEY),
    ] {
        keys.insert(k);
    }
    let mut kbd = VirtualDevice::builder()?
        .name("liwtest keyboard")
        .with_keys(&keys)?
        .build()?;

    let mut btns = AttributeSet::<KeyCode>::new();
    for b in [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE] {
        btns.insert(b);
    }
    let mut rels = AttributeSet::<RelativeAxisCode>::new();
    for r in [RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y,
              RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_HWHEEL] {
        rels.insert(r);
    }
    let mut props = AttributeSet::<PropType>::new();
    props.insert(PropType::POINTER);
    let mut mouse = VirtualDevice::builder()?
        .name("liwtest mouse")
        .with_keys(&btns)?
        .with_relative_axes(&rels)?
        .with_properties(&props)?
        .build()?;

    for (label, dev) in [("keyboard", &mut kbd), ("mouse", &mut mouse)] {
        for p in dev.enumerate_dev_nodes_blocking()? {
            println!("{label}={}", p?.display());
        }
    }
    println!("sending in {wait}s");

    sleep(Duration::from_secs(wait));

    let key = |c: u16, v: i32| InputEvent::new(EventType::KEY.0, c, v);
    let rel = |c: RelativeAxisCode, v: i32| InputEvent::new(EventType::RELATIVE.0, c.0, v);

    println!("hotkey");
    kbd.emit(&[key(KEY_HOTKEY, 1)])?;
    sleep(Duration::from_millis(40));
    kbd.emit(&[key(KEY_HOTKEY, 0)])?;
    sleep(Duration::from_millis(600));

    println!("W down");
    kbd.emit(&[key(KeyCode::KEY_W.0, 1)])?;
    // Motion WHILE a key is held: the joystick and the aim run through
    // different parts of the engine and both have to survive the other.
    for _ in 0..20 {
        mouse.emit(&[rel(RelativeAxisCode::REL_X, 12)])?;
        sleep(Duration::from_millis(25));
    }
    println!("W up");
    kbd.emit(&[key(KeyCode::KEY_W.0, 0)])?;
    sleep(Duration::from_millis(400));

    println!("hotkey (leaving game mode)");
    kbd.emit(&[key(KEY_HOTKEY, 1)])?;
    sleep(Duration::from_millis(40));
    kbd.emit(&[key(KEY_HOTKEY, 0)])?;
    sleep(Duration::from_millis(400));
    println!("done");
    Ok(())
}
