//! Which character a physical key produces on each of the user's keyboard layouts (libxkbcommon).

use xkbcommon::xkb;

/// [(layout, variant)] from KDE's keyboard settings, else $XKB_DEFAULT_LAYOUT, else "us".
pub fn current_layouts() -> Vec<(String, String)> {
    let path = super::home().join(".config/kxkbrc");
    if let Ok(text) = std::fs::read_to_string(path) {
        let mut in_layout = false;
        let (mut use_, mut layouts, mut variants) = (false, String::new(), String::new());
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_layout = line == "[Layout]";
                continue;
            }
            if !in_layout {
                continue;
            }
            match line.split_once('=') {
                Some(("Use", v)) => use_ = v.trim() == "true",
                Some(("LayoutList", v)) => layouts = v.trim().into(),
                Some(("VariantList", v)) => variants = v.trim().into(),
                _ => {}
            }
        }
        if use_ && !layouts.is_empty() {
            let variants: Vec<&str> = variants.split(',').collect();
            return layouts
                .split(',')
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .map(|(i, l)| {
                    (
                        l.trim().to_string(),
                        variants.get(i).unwrap_or(&"").trim().to_string(),
                    )
                })
                .collect();
        }
    }
    let env = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_else(|_| "us".into());
    env.split(',')
        .filter(|l| !l.trim().is_empty())
        .map(|l| (l.trim().to_string(), String::new()))
        .collect()
}

/// The character the key (evdev code) produces with or without Shift, per layout; None where unknown.
pub fn chars_for_key(
    evdev_code: u32,
    shift: bool,
    layouts: &[(String, String)],
) -> Vec<Option<char>> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    layouts
        .iter()
        .map(|(layout, variant)| {
            let keymap = xkb::Keymap::new_from_names(
                &ctx,
                "",
                "",
                layout,
                variant,
                None,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )?;
            let syms = keymap.key_get_syms_by_level(
                xkb::Keycode::new(evdev_code + 8),
                0,
                u32::from(shift),
            );
            char::from_u32(xkb::keysym_to_utf32(*syms.first()?)).filter(|c| *c != '\0')
        })
        .collect()
}
