//! Hotkey strings ("Ctrl+Alt+Shift+1"): parsing, canonical key names and formatting. How a hotkey
//! becomes a global shortcut is up to the platform (KDE key codes on Linux, virtual keys on Windows).

use std::fmt;

/// The digit-row keys, in physical order. Like Windows' VK_0..VK_9 they are positional:
/// "Shift+1" means this physical key on every layout (on AZERTY it types "1" only with Shift).
pub const DIGIT_ROW: [char; 10] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];

/// Canonical names of the non-character keys a hotkey may use.
pub const NAMED_KEYS: &[&str] = &[
    "Esc",
    "Tab",
    "Backtab",
    "Backspace",
    "Return",
    "Enter",
    "Ins",
    "Del",
    "Pause",
    "Print",
    "SysReq",
    "Clear",
    "Home",
    "End",
    "Left",
    "Up",
    "Right",
    "Down",
    "PgUp",
    "PgDown",
    "CapsLock",
    "NumLock",
    "ScrollLock",
    "Menu",
    "Space",
    "Volume Down",
    "Volume Mute",
    "Volume Up",
    "Media Play",
    "Media Stop",
    "Media Previous",
    "Media Next",
];

/// Other spellings (Windows Forms, egui) of the names above.
const ALIASES: &[(&str, &str)] = &[
    ("escape", "Esc"),
    ("delete", "Del"),
    ("insert", "Ins"),
    ("pageup", "PgUp"),
    ("prior", "PgUp"),
    ("pagedown", "PgDown"),
    ("next", "PgDown"),
    ("back", "Backspace"),
    ("arrowup", "Up"),
    ("arrowdown", "Down"),
    ("arrowleft", "Left"),
    ("arrowright", "Right"),
    ("printscreen", "Print"),
    ("capital", "CapsLock"),
    ("scroll", "ScrollLock"),
    ("apps", "Menu"),
];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Windows/Super key ("Meta" on Linux, "Win" on Windows).
    pub meta: bool,
    /// "1", "S", "F5", "PgUp", ...
    pub key: String,
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (name, on) in [
            ("Ctrl", self.ctrl),
            ("Alt", self.alt),
            ("Shift", self.shift),
            (crate::platform::META_NAME, self.meta),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key)
    }
}

/// Canonical key name, or None if unknown.
pub fn canonical_key(raw: &str) -> Option<String> {
    let mut chars = raw.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        return Some(c.to_uppercase().collect());
    }
    let low = raw.to_lowercase().replace(' ', "");
    // Windows Forms "D1", egui "Num1"
    for prefix in ["d", "num"] {
        if let Some(d) = low.strip_prefix(prefix)
            && d.len() == 1
            && d.chars().all(|c| c.is_ascii_digit())
        {
            return Some(d.into());
        }
    }
    if let Some(n) = low.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=35).contains(&n).then(|| format!("F{n}"));
    }
    if let Some((_, name)) = ALIASES.iter().find(|(a, _)| *a == low) {
        return Some(name.to_string());
    }
    NAMED_KEYS
        .iter()
        .find(|n| n.to_lowercase().replace(' ', "") == low)
        .map(|n| n.to_string())
}

impl Hotkey {
    /// Parses "Ctrl+Alt+Shift+1" ("Win", "Meta" and "Super" all mean the Windows key).
    /// None if empty or invalid.
    pub fn parse(text: &str) -> Option<Hotkey> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut hk = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
            key: String::new(),
        };
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if text.ends_with("++") || text == "+" {
            parts.retain(|p| !p.is_empty());
            parts.push("+");
        }
        for raw in parts.into_iter().filter(|p| !p.is_empty()) {
            match raw.to_lowercase().as_str() {
                "ctrl" | "control" => hk.ctrl = true,
                "alt" => hk.alt = true,
                "shift" => hk.shift = true,
                "meta" | "win" | "windows" | "super" => hk.meta = true,
                _ if hk.key.is_empty() => hk.key = canonical_key(raw)?,
                _ => return None,
            }
        }
        (!hk.key.is_empty()).then_some(hk)
    }

    pub fn parse_opt(text: &Option<String>) -> Option<Hotkey> {
        text.as_deref().and_then(Hotkey::parse)
    }

    /// The digit-row position (0 for "1" ... 9 for "0"), if the key is a digit.
    #[cfg_attr(windows, allow(dead_code))] // used by the KDE backend
    pub fn digit_index(&self) -> Option<usize> {
        let mut chars = self.key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => DIGIT_ROW.iter().position(|d| *d == c),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::META_NAME;

    #[test]
    fn round_trip() {
        for s in [
            "Ctrl+Alt+Shift+1",
            "Ctrl+Alt+Shift+S",
            "Ctrl+PgUp",
            "Alt+Space",
        ] {
            assert_eq!(Hotkey::parse(s).unwrap().to_string(), s);
        }
        let meta = format!("{META_NAME}+F5");
        assert_eq!(Hotkey::parse(&meta).unwrap().to_string(), meta);
        assert_eq!(
            Hotkey::parse("ctrl+win+d1").unwrap().to_string(),
            format!("Ctrl+{META_NAME}+1")
        );
        assert_eq!(Hotkey::parse("Ctrl+PageUp").unwrap().key, "PgUp");
        assert_eq!(
            Hotkey::parse("Ctrl+Alt+Shift+0").unwrap().digit_index(),
            Some(9)
        );
        assert!(Hotkey::parse("").is_none());
        assert!(Hotkey::parse("Ctrl+Bogus").is_none());
    }
}
