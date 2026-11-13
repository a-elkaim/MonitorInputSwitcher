//! Hotkeys as macOS virtual key codes (Carbon kVK_*, positions on the ANSI layout) and modifier
//! masks, for RegisterEventHotKey, plus the key equivalents the menu shows next to each item.

use crate::hotkey::Hotkey;

// Carbon modifier masks (Events.h).
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

const CHAR_KEYS: &[(char, u32)] = &[
    ('A', 0x00),
    ('S', 0x01),
    ('D', 0x02),
    ('F', 0x03),
    ('H', 0x04),
    ('G', 0x05),
    ('Z', 0x06),
    ('X', 0x07),
    ('C', 0x08),
    ('V', 0x09),
    ('B', 0x0B),
    ('Q', 0x0C),
    ('W', 0x0D),
    ('E', 0x0E),
    ('R', 0x0F),
    ('Y', 0x10),
    ('T', 0x11),
    ('1', 0x12),
    ('2', 0x13),
    ('3', 0x14),
    ('4', 0x15),
    ('6', 0x16),
    ('5', 0x17),
    ('=', 0x18),
    ('9', 0x19),
    ('7', 0x1A),
    ('-', 0x1B),
    ('8', 0x1C),
    ('0', 0x1D),
    (']', 0x1E),
    ('O', 0x1F),
    ('U', 0x20),
    ('[', 0x21),
    ('I', 0x22),
    ('P', 0x23),
    ('L', 0x25),
    ('J', 0x26),
    ('\'', 0x27),
    ('K', 0x28),
    (';', 0x29),
    ('\\', 0x2A),
    (',', 0x2B),
    ('/', 0x2C),
    ('N', 0x2D),
    ('M', 0x2E),
    ('.', 0x2F),
    ('`', 0x32),
];

const NAMED_KEYS: &[(&str, u32)] = &[
    ("Return", 0x24),
    ("Enter", 0x4C),
    ("Tab", 0x30),
    ("Space", 0x31),
    ("Backspace", 0x33),
    ("Esc", 0x35),
    ("Clear", 0x47),
    ("Ins", 0x72),
    ("Home", 0x73),
    ("PgUp", 0x74),
    ("Del", 0x75),
    ("End", 0x77),
    ("PgDown", 0x79),
    ("Left", 0x7B),
    ("Right", 0x7C),
    ("Down", 0x7D),
    ("Up", 0x7E),
    ("Volume Up", 0x48),
    ("Volume Down", 0x49),
    ("Volume Mute", 0x4A),
];

const F_KEYS: [u32; 20] = [
    0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71, 0x6A,
    0x40, 0x4F, 0x50, 0x5A,
];

/// The virtual key code of a hotkey's key, by position on the ANSI layout.
pub fn key_code(hk: &Hotkey) -> Option<u32> {
    let key = hk.key.as_str();
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<usize>().ok()) {
        return F_KEYS.get(n.checked_sub(1)?).copied();
    }
    if let Some((_, code)) = NAMED_KEYS.iter().find(|(n, _)| *n == key) {
        return Some(*code);
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let c = c.to_ascii_uppercase();
    CHAR_KEYS.iter().find(|(k, _)| *k == c).map(|(_, code)| *code)
}

pub fn modifiers(hk: &Hotkey) -> u32 {
    let mut m = 0;
    if hk.ctrl {
        m |= CONTROL_KEY;
    }
    if hk.alt {
        m |= OPTION_KEY;
    }
    if hk.shift {
        m |= SHIFT_KEY;
    }
    if hk.meta {
        m |= CMD_KEY;
    }
    m
}

/// The NSMenuItem key equivalent that displays this key, if AppKit can show it.
pub fn key_equivalent(hk: &Hotkey) -> Option<String> {
    let key = hk.key.as_str();
    let special = |code: u32| char::from_u32(code).map(String::from);
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=35).contains(&n)
    {
        return special(0xF704 + n - 1); // NSF1FunctionKey...
    }
    let named = match key {
        "Up" => special(0xF700),
        "Down" => special(0xF701),
        "Left" => special(0xF702),
        "Right" => special(0xF703),
        "Del" => special(0xF728),
        "Home" => special(0xF729),
        "End" => special(0xF72B),
        "PgUp" => special(0xF72C),
        "PgDown" => special(0xF72D),
        "Space" => Some(" ".into()),
        "Tab" => Some("\t".into()),
        "Return" | "Enter" => Some("\r".into()),
        "Esc" => Some("\u{1b}".into()),
        "Backspace" => Some("\u{8}".into()),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_lowercase().collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        let hk = Hotkey::parse("Ctrl+Alt+Shift+1").unwrap();
        assert_eq!(key_code(&hk), Some(0x12));
        assert_eq!(modifiers(&hk), CONTROL_KEY | OPTION_KEY | SHIFT_KEY);
        assert_eq!(key_code(&Hotkey::parse("Cmd+S").unwrap()), Some(0x01));
        assert_eq!(key_code(&Hotkey::parse("F1").unwrap()), Some(0x7A));
        assert_eq!(key_code(&Hotkey::parse("F20").unwrap()), Some(0x5A));
        assert_eq!(key_code(&Hotkey::parse("F21").unwrap()), None);
        assert_eq!(key_code(&Hotkey::parse("Ctrl+PgUp").unwrap()), Some(0x74));
        assert_eq!(key_code(&Hotkey::parse("Ctrl+²").unwrap()), None);
        assert_eq!(
            key_equivalent(&Hotkey::parse("Ctrl+S").unwrap()).as_deref(),
            Some("s")
        );
    }
}
