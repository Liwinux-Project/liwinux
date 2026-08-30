//! A synthetic mouse for the HOST, so a click can be tested without a hand.
//!
//! This is a development tool, not part of the product. It exists because
//! "the mouse does nothing in the game window" cannot be investigated by
//! reading code: the question is whether a real click reaches the handler,
//! and answering it needs a real click.
//!
//! It creates a uinput pointer with absolute axes, moves it to a screen
//! position and clicks. Absolute rather than relative on purpose — a relative
//! mouse would land wherever the cursor happened to be, and a test that
//! cannot say where it clicked proves nothing.
//!
//! ```text
//! cargo run -p liw-input --example host-click -- 800 400 [screen_w screen_h]
//! ```

use std::{thread::sleep, time::Duration};

use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent,
    KeyCode, PropType, UinputAbsSetup,
};

fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 {
        eprintln!("usage: host-click X Y [SCREEN_W SCREEN_H]");
        std::process::exit(2);
    }
    let (x, y): (i32, i32) = (a[0].parse().unwrap(), a[1].parse().unwrap());
    let sw: i32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(3840);
    let sh: i32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(2160);

    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_LEFT);
    // BTN_TOOL_FINGER is not set and BTN_TOUCH is not either: this is a
    // POINTER, and claiming touch capabilities would have the compositor
    // treat it as a touchscreen and route the events somewhere else.
    let mut props = AttributeSet::<PropType>::new();
    props.insert(PropType::POINTER);

    let mut dev = VirtualDevice::builder()?
        .name("liwinux test pointer")
        .with_keys(&keys)?
        .with_properties(&props)?
        .with_absolute_axis(&UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_X,
            AbsInfo::new(0, 0, sw, 0, 0, 1),
        ))?
        .with_absolute_axis(&UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_Y,
            AbsInfo::new(0, 0, sh, 0, 0, 1),
        ))?
        .build()?;

    // The compositor needs a moment to notice a new device; events sent
    // before it has are simply lost, which reads as "the click did nothing".
    sleep(Duration::from_millis(900));

    let abs = |code: AbsoluteAxisCode, v: i32| {
        InputEvent::new(EventType::ABSOLUTE.0, code.0, v)
    };
    let btn = |v: i32| InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, v);

    dev.emit(&[abs(AbsoluteAxisCode::ABS_X, x), abs(AbsoluteAxisCode::ABS_Y, y)])?;
    sleep(Duration::from_millis(250));
    dev.emit(&[btn(1)])?;
    sleep(Duration::from_millis(120));
    dev.emit(&[btn(0)])?;
    sleep(Duration::from_millis(250));

    println!("clicked at {x},{y} on a {sw}x{sh} screen");
    Ok(())
}
