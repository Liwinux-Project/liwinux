//! Turning what gpui reports into what the input engine stores.
//!
//! A profile stores an **evdev code**, not a letter. That is deliberate and
//! documented in `liw_input::profile::Trigger`: the raw code is
//! layout-independent, so a binding made on a Turkish keyboard still means the
//! same physical key on a US one. gpui reports the printed character instead,
//! so something has to translate, and this is it.
//!
//! The table is not exhaustive and is not trying to be. It covers what games
//! are bound to: letters, digits, the arrows, the modifiers, space, and the
//! function keys. A key that is not here is REFUSED rather than guessed at —
//! binding the wrong physical key is worse than saying no, because it fails
//! silently and only during play.

/// evdev code for a key gpui named, or `None` when it is not one we can map.
pub fn evdev_code(name: &str) -> Option<u16> {
    // The letter and digit rows in evdev order. Writing them out beats
    // arithmetic on ASCII: the codes are NOT alphabetical, they follow the
    // physical rows of a keyboard, and 'a' is 30 rather than anything derivable.
    const ROW1: &str = "qwertyuiop";
    const ROW2: &str = "asdfghjkl";
    const ROW3: &str = "zxcvbnm";

    let n = name.to_ascii_lowercase();
    if let Some(i) = ROW1.find(&n).filter(|_| n.len() == 1) {
        return Some(16 + i as u16);
    }
    if let Some(i) = ROW2.find(&n).filter(|_| n.len() == 1) {
        return Some(30 + i as u16);
    }
    if let Some(i) = ROW3.find(&n).filter(|_| n.len() == 1) {
        return Some(44 + i as u16);
    }
    if n.len() == 1 {
        if let Some(d) = n.chars().next().unwrap().to_digit(10) {
            // 1..9 are 2..10 and 0 is 11 — the number row, not the value.
            return Some(if d == 0 { 11 } else { 1 + d as u16 });
        }
    }

    Some(match n.as_str() {
        "escape" => 1,
        "backspace" => 14,
        "tab" => 15,
        "enter" | "return" => 28,
        "ctrl" | "control" | "leftcontrol" => 29,
        "shift" | "leftshift" => 42,
        "alt" | "leftalt" => 56,
        "space" => 57,
        "capslock" => 58,
        "rightshift" => 54,
        "rightctrl" | "rightcontrol" => 97,
        "rightalt" => 100,
        "home" => 102,
        "up" => 103,
        "pageup" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pagedown" => 109,
        "insert" => 110,
        "delete" => 111,
        "minus" | "-" => 12,
        "equal" | "=" => 13,
        "leftbracket" | "[" => 26,
        "rightbracket" | "]" => 27,
        "semicolon" | ";" => 39,
        "quote" | "'" => 40,
        "comma" | "," => 51,
        "period" | "." => 52,
        "slash" | "/" => 53,
        "backslash" | "\\" => 43,
        f if f.starts_with('f') && f.len() <= 3 => {
            let n: u16 = f[1..].parse().ok()?;
            match n {
                1..=10 => 58 + n,
                11 => 87,
                12 => 88,
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// A human label for a stored code, for showing what is bound.
///
/// Only the codes this module can produce are named. Anything else is shown
/// as its number rather than a wrong letter — a profile written by hand or by
/// an older version can hold codes this table never made.
pub fn label(code: u16) -> String {
    const ROW1: &[u8] = b"qwertyuiop";
    const ROW2: &[u8] = b"asdfghjkl";
    const ROW3: &[u8] = b"zxcvbnm";
    let letter = |set: &[u8], base: u16| {
        set.get((code - base) as usize)
            .map(|c| (*c as char).to_ascii_uppercase().to_string())
    };
    match code {
        16..=25 => letter(ROW1, 16),
        30..=38 => letter(ROW2, 30),
        44..=50 => letter(ROW3, 44),
        2..=10 => Some((code - 1).to_string()),
        11 => Some("0".into()),
        1 => Some("Esc".into()),
        14 => Some("Backspace".into()),
        15 => Some("Tab".into()),
        28 => Some("Enter".into()),
        29 => Some("Ctrl".into()),
        42 => Some("Shift".into()),
        54 => Some("R.Shift".into()),
        56 => Some("Alt".into()),
        57 => Some("Space".into()),
        97 => Some("R.Ctrl".into()),
        100 => Some("R.Alt".into()),
        103 => Some("Up".into()),
        105 => Some("Left".into()),
        106 => Some("Right".into()),
        108 => Some("Down".into()),
        59..=68 => Some(format!("F{}", code - 58)),
        87 => Some("F11".into()),
        88 => Some("F12".into()),
        _ => None,
    }
    .unwrap_or_else(|| format!("#{code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codes that matter most: WASD is what nearly every profile starts
    /// from, and getting one of them wrong is a binding that silently moves
    /// the wrong way.
    #[test]
    fn wasd_maps_to_the_evdev_codes_the_engine_expects() {
        assert_eq!(evdev_code("w"), Some(17));
        assert_eq!(evdev_code("a"), Some(30));
        assert_eq!(evdev_code("s"), Some(31));
        assert_eq!(evdev_code("d"), Some(32));
    }

    /// Letters are laid out by keyboard row, not alphabetically. A table
    /// derived from ASCII would put 'a' at 16 and every binding would be
    /// wrong by a whole row.
    #[test]
    fn the_rows_are_not_alphabetical() {
        assert_eq!(evdev_code("q"), Some(16));
        assert_eq!(evdev_code("p"), Some(25));
        assert_eq!(evdev_code("z"), Some(44));
        assert_eq!(evdev_code("m"), Some(50));
    }

    /// The number row stores its POSITION, so 1 is 2 and 0 is 11 rather than
    /// the digit's value.
    #[test]
    fn digits_use_row_position_not_value() {
        assert_eq!(evdev_code("1"), Some(2));
        assert_eq!(evdev_code("9"), Some(10));
        assert_eq!(evdev_code("0"), Some(11));
    }

    #[test]
    fn the_keys_games_use_are_covered() {
        for k in ["space", "shift", "ctrl", "alt", "tab", "escape",
                  "up", "down", "left", "right", "enter"] {
            assert!(evdev_code(k).is_some(), "{k} is not mapped");
        }
    }

    #[test]
    fn function_keys_are_mapped_including_the_two_odd_ones() {
        assert_eq!(evdev_code("f1"), Some(59));
        assert_eq!(evdev_code("f10"), Some(68));
        assert_eq!(evdev_code("f11"), Some(87), "F11 breaks the run");
        assert_eq!(evdev_code("f12"), Some(88));
    }

    /// An unmappable key must be refused. Guessing a code would bind the
    /// wrong physical key, and that failure only shows up during play.
    #[test]
    fn an_unknown_key_is_refused_rather_than_guessed() {
        assert_eq!(evdev_code("f13"), None);
        assert_eq!(evdev_code("printscreen"), None);
        assert_eq!(evdev_code(""), None);
        assert_eq!(evdev_code("ä"), None);
    }

    /// Case must not matter: gpui reports what is printed on the key.
    #[test]
    fn case_is_ignored() {
        assert_eq!(evdev_code("W"), evdev_code("w"));
        assert_eq!(evdev_code("Space"), evdev_code("space"));
    }

    /// Every code the table produces must have a label, or the editor shows
    /// a number for a key the user just pressed.
    #[test]
    fn everything_mappable_is_also_nameable() {
        for k in ["q", "m", "0", "9", "space", "shift", "f1", "f11", "up"] {
            let code = evdev_code(k).unwrap();
            let l = label(code);
            assert!(!l.starts_with('#'), "{k} -> {code} has no label ({l})");
        }
    }

    /// A code from outside the table is shown as a number rather than a
    /// wrong letter — profiles can be written by hand.
    #[test]
    fn an_unknown_code_is_shown_as_itself() {
        assert_eq!(label(240), "#240");
    }

    #[test]
    fn labels_round_trip_the_letters() {
        assert_eq!(label(evdev_code("w").unwrap()), "W");
        assert_eq!(label(evdev_code("a").unwrap()), "A");
        assert_eq!(label(evdev_code("z").unwrap()), "Z");
    }
}
