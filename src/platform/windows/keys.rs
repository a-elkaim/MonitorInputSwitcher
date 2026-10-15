//! Hotkeys as Windows virtual keys, for RegisterHotKey.

use crate::hotkey::Hotkey;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VkKeyScanW,
};

const NAMED_VK: &[(&str, u16)] = &[
    ("Esc", 0x1B),
    ("Tab", 0x09),
    ("Backtab", 0x09),
    ("Backspace", 0x08),
    ("Return", 0x0D),
    ("Enter", 0x0D),
    ("Ins", 0x2D),
    ("Del", 0x2E),
    ("Pause", 0x13),
    ("Print", 0x2C),
    ("SysReq", 0x2C),
    ("Clear", 0x0C),
    ("Home", 0x24),
    ("End", 0x23),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("PgUp", 0x21),
    ("PgDown", 0x22),
    ("CapsLock", 0x14),
    ("NumLock", 0x90),
    ("ScrollLock", 0x91),
    ("Menu", 0x5D),
    ("Space", 0x20),
    ("Volume Down", 0xAE),
    ("Volume Mute", 0xAD),
    ("Volume Up", 0xAF),
    ("Media Play", 0xB3),
    ("Media Stop", 0xB2),
    ("Media Previous", 0xB1),
    ("Media Next", 0xB0),
];

/// The virtual key of a hotkey's key. Letters and digit-row keys are positional virtual keys
/// (VK_A, VK_1...); other characters go through the current layout.
pub fn vk(hk: &Hotkey) -> Option<u16> {
    let key = hk.key.as_str();
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    if let Some((_, vk)) = NAMED_VK.iter().find(|(n, _)| *n == key) {
        return Some(*vk);
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if c.is_ascii_alphanumeric() {
        return Some(c.to_ascii_uppercase() as u16);
    }
    let mut buf = [0u16; 2];
    let code = c.encode_utf16(&mut buf);
    if code.len() != 1 {
        return None;
    }
    let scan = unsafe { VkKeyScanW(code[0]) };
    (scan != -1).then_some((scan as u16) & 0xFF)
}

pub fn modifiers(hk: &Hotkey) -> HOT_KEY_MODIFIERS {
    let mut m = MOD_NOREPEAT;
    if hk.ctrl {
        m |= MOD_CONTROL;
    }
    if hk.alt {
        m |= MOD_ALT;
    }
    if hk.shift {
        m |= MOD_SHIFT;
    }
    if hk.meta {
        m |= MOD_WIN;
    }
    m
}
