//! Hotkeys as Qt key codes, which is what KDE's kglobalaccel speaks.
//!
//! KWin matches global shortcuts on the character a key produces, and drops Shift when Shift was
//! needed to produce a non-letter. "Ctrl+Alt+Shift+1" therefore reaches kglobalaccel as
//! "Ctrl+Alt+!" on a US layout and as "Ctrl+Alt+1" on AZERTY, so every variant is registered.

use super::xkb;
use crate::hotkey::{DIGIT_ROW, Hotkey};

pub const SHIFT: i32 = 0x0200_0000;
pub const CTRL: i32 = 0x0400_0000;
pub const ALT: i32 = 0x0800_0000;
pub const META: i32 = 0x1000_0000;
const KEYPAD: i32 = 0x2000_0000;
const MOD_MASK: i32 = SHIFT | CTRL | ALT | META | KEYPAD;

/// evdev code of a digit-row key ("1" = KEY_1 = 2 ... "0" = KEY_0 = 11).
fn digit_evdev(index: usize) -> u32 {
    index as u32 + 2
}

const QT_CODES: &[(&str, i32)] = &[
    ("Esc", 0x0100_0000),
    ("Tab", 0x0100_0001),
    ("Backtab", 0x0100_0002),
    ("Backspace", 0x0100_0003),
    ("Return", 0x0100_0004),
    ("Enter", 0x0100_0005),
    ("Ins", 0x0100_0006),
    ("Del", 0x0100_0007),
    ("Pause", 0x0100_0008),
    ("Print", 0x0100_0009),
    ("SysReq", 0x0100_000a),
    ("Clear", 0x0100_000b),
    ("Home", 0x0100_0010),
    ("End", 0x0100_0011),
    ("Left", 0x0100_0012),
    ("Up", 0x0100_0013),
    ("Right", 0x0100_0014),
    ("Down", 0x0100_0015),
    ("PgUp", 0x0100_0016),
    ("PgDown", 0x0100_0017),
    ("CapsLock", 0x0100_0024),
    ("NumLock", 0x0100_0025),
    ("ScrollLock", 0x0100_0026),
    ("Menu", 0x0100_0055),
    ("Space", 0x20),
    ("Volume Down", 0x0100_0070),
    ("Volume Mute", 0x0100_0071),
    ("Volume Up", 0x0100_0072),
    ("Media Play", 0x0100_0080),
    ("Media Stop", 0x0100_0081),
    ("Media Previous", 0x0100_0082),
    ("Media Next", 0x0100_0083),
];

fn qt_key(name: &str) -> i32 {
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<i32>().ok()) {
        return 0x0100_002f + n;
    }
    if let Some((_, code)) = QT_CODES.iter().find(|(n, _)| *n == name) {
        return *code;
    }
    qt_key_for_char(name.chars().next().unwrap_or('\0'))
}

fn qt_key_for_char(c: char) -> i32 {
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => u as i32,
        _ => c as i32,
    }
}

fn key_name(code: i32) -> Option<String> {
    if (0x0100_0030..=0x0100_0052).contains(&code) {
        return Some(format!("F{}", code - 0x0100_002f));
    }
    if let Some((n, _)) = QT_CODES.iter().find(|(_, c)| *c == code) {
        return Some(n.to_string());
    }
    char::from_u32(code as u32)
        .filter(|c| !c.is_control())
        .map(|c| c.to_string())
}

fn mods(hk: &Hotkey) -> i32 {
    (if hk.ctrl { CTRL } else { 0 })
        | (if hk.alt { ALT } else { 0 })
        | (if hk.shift { SHIFT } else { 0 })
        | (if hk.meta { META } else { 0 })
}

/// Qt key combinations KWin may report for this hotkey, the literal form first.
pub fn variants(hk: &Hotkey) -> Vec<i32> {
    let mut out = vec![mods(hk) | qt_key(&hk.key)];
    if let Some(index) = hk.digit_index() {
        for c in xkb::chars_for_key(digit_evdev(index), hk.shift, &xkb::current_layouts())
            .into_iter()
            .flatten()
        {
            let mut m = mods(hk);
            if hk.shift && !c.is_alphabetic() {
                m &= !SHIFT;
            }
            let combo = m | qt_key_for_char(c);
            if !out.contains(&combo) {
                out.push(combo);
            }
        }
    }
    out
}

/// Inverse of `variants` for a key KDE reports ("Ctrl+Alt+!" becomes Ctrl+Alt+Shift+1).
pub fn from_qt(combined: i32) -> Option<Hotkey> {
    let m = combined & MOD_MASK;
    let name = key_name(combined & !MOD_MASK)?;
    let mut hk = Hotkey {
        ctrl: m & CTRL != 0,
        alt: m & ALT != 0,
        shift: m & SHIFT != 0,
        meta: m & META != 0,
        key: name,
    };
    let mut chars = hk.key.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && (!c.is_alphanumeric() || !c.is_ascii())
    {
        let layouts = xkb::current_layouts();
        let lc = c.to_lowercase().next();
        for (index, digit) in DIGIT_ROW.iter().enumerate() {
            for shift in [true, false] {
                let produced = xkb::chars_for_key(digit_evdev(index), shift, &layouts);
                if produced
                    .iter()
                    .flatten()
                    .any(|p| p.to_lowercase().next() == lc)
                {
                    hk.key = digit.to_string();
                    hk.shift |= shift;
                    return Some(hk);
                }
            }
        }
    }
    Some(hk)
}

/// For the tray menu (dbusmenu): [["Control", "Alt", "Shift", "1"]].
pub fn dbusmenu_shortcut(hk: &Hotkey) -> Vec<Vec<String>> {
    let mut keys: Vec<String> = [
        ("Control", hk.ctrl),
        ("Alt", hk.alt),
        ("Shift", hk.shift),
        ("Super", hk.meta),
    ]
    .into_iter()
    .filter(|(_, on)| *on)
    .map(|(n, _)| n.to_string())
    .collect();
    keys.push(hk.key.clone());
    vec![keys]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_codes() {
        let hk = Hotkey::parse("Ctrl+Alt+Shift+S").unwrap();
        assert_eq!(variants(&hk), vec![CTRL | ALT | SHIFT | 'S' as i32]);
        assert_eq!(from_qt(CTRL | ALT | SHIFT | 'S' as i32), Some(hk));
        assert_eq!(qt_key("F1"), 0x0100_0030);
        assert_eq!(from_qt(META | 0x0100_0030).unwrap().to_string(), "Meta+F1");
    }

    #[test]
    fn digit_variants_per_layout() {
        let fr_us = [
            ("fr".to_string(), String::new()),
            ("us".to_string(), String::new()),
        ];
        assert_eq!(
            xkb::chars_for_key(2, true, &fr_us),
            vec![Some('1'), Some('!')]
        );
        assert_eq!(
            xkb::chars_for_key(2, false, &fr_us),
            vec![Some('&'), Some('1')]
        );
        // The key left of "1" (KEY_GRAVE), which the Settings window records by its character.
        assert_eq!(
            xkb::chars_for_key(0x29, false, &fr_us),
            vec![Some('²'), Some('`')]
        );
        let hk = Hotkey::parse("Ctrl+Alt+²").unwrap();
        assert_eq!(variants(&hk), vec![CTRL | ALT | 0xB2]);
        assert_eq!(from_qt(CTRL | ALT | 0xB2), Some(hk));
    }
}
