//! macOS backend: DDC/CI through IOAVService (Apple Silicon), a menu bar item (NSStatusItem),
//! Carbon global hotkeys, notifications through `osascript`, a LaunchAgent for "Open at Login",
//! and the same Unix socket + flock as Linux for the CLI.
//!
//! AppKit must run on the main thread, so the tray app's event loop runs on a second thread
//! (`run_with_app_kit`) and the UI is driven by dispatching closures to the main queue.

pub mod ddc;
#[path = "../linux/ipc.rs"]
pub mod ipc;
mod keys;
mod ui;

pub use ui::{Ui, run_with_app_kit};

use std::path::PathBuf;

pub const META_NAME: &str = "Cmd";
pub const START_WITH_LOGIN: &str = "Open at Login";
/// Appended to the Settings window's inputs hint.
pub const SHORTCUTS_HINT: &str = " Ctrl, Alt and Cmd are the Control, Option and Command keys.";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// ~/Library/Application Support/MonitorInputSwitcher
pub fn config_dir() -> PathBuf {
    home().join("Library/Application Support/MonitorInputSwitcher")
}

/// Nothing to do: a macOS program prints to the terminal it was started from.
pub fn attach_console() {}

/// Not needed: the app releases its hotkeys while the Settings window is open.
pub fn block_shortcuts_for_capture(_blocked: bool) {}

/// The character a punctuation key types, by its PC scan code. Hotkeys are registered by key
/// position on the ANSI (US) layout, so this is the US character for that position.
pub fn layout_char(scancode: u32) -> Option<char> {
    Some(match scancode {
        0x29 => '`',
        0x0C => '-',
        0x0D => '=',
        0x1A => '[',
        0x1B => ']',
        0x2B => '\\',
        0x27 => ';',
        0x28 => '\'',
        0x33 => ',',
        0x34 => '.',
        0x35 => '/',
        _ => return None,
    })
}

/// "Open at Login": a per-user LaunchAgent, ~/Library/LaunchAgents/<label>.plist.
pub mod autostart {
    use super::home;
    use std::path::PathBuf;
    use std::{fs, io};

    const LABEL: &str = "io.github.monitor-input-switcher";

    fn path() -> PathBuf {
        home()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn program() -> String {
        crate::config::self_exe().display().to_string()
    }

    fn plist() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
            xml_escape(&program())
        )
    }

    pub fn is_enabled() -> bool {
        path().exists()
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let path = path();
        if !enabled {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        }
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, plist())
    }

    /// Keeps the program path correct if the app was moved.
    pub fn refresh_path() {
        if let Ok(text) = fs::read_to_string(path())
            && text != plist()
        {
            let _ = set_enabled(true);
        }
    }
}
