//! Linux (KDE Plasma) backend: DDC/CI over /dev/i2c-N, a StatusNotifierItem tray, desktop
//! notifications and kglobalaccel shortcuts over D-Bus, a Unix socket for the CLI.

pub mod ddc;
pub mod ipc;
mod kglobalaccel;
mod qtkeys;
mod ui;
mod xkb;

pub use ui::Ui;

use crate::config::{APP_ID, APP_NAME};
use std::path::PathBuf;

pub const META_NAME: &str = "Meta";
pub const START_WITH_LOGIN: &str = "Start with login";
/// Appended to the Settings window's inputs hint.
pub const SHORTCUTS_HINT: &str = " Shortcuts also appear in System Settings > Keyboard > Shortcuts, \
     where they can be changed too (including Meta combinations).";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

/// ~/.config/monitor-input-switcher
pub fn config_dir() -> PathBuf {
    config_home().join(APP_ID)
}

/// Nothing to do: a Linux program prints to the terminal it was started from.
pub fn attach_console() {}

/// Suspends all global shortcuts while the Settings window records a key combination, so the
/// keys reach the window instead of triggering a shortcut.
pub fn block_shortcuts_for_capture(blocked: bool) {
    kglobalaccel::set_blocked(blocked);
}

/// The character the key with this evdev code types without modifiers, on the first of the
/// user's keyboard layouts ("²" for the key left of "1" on a French layout).
pub fn layout_char(evdev_code: u32) -> Option<char> {
    let layouts = xkb::current_layouts();
    xkb::chars_for_key(evdev_code, false, &layouts[..layouts.len().min(1)])
        .into_iter()
        .next()
        .flatten()
}

/// "Start with login": ~/.config/autostart/monitor-input-switcher.desktop.
pub mod autostart {
    use super::*;
    use std::{fs, io};

    fn path() -> PathBuf {
        config_home()
            .join("autostart")
            .join(format!("{APP_ID}.desktop"))
    }

    /// The installed launcher if present, otherwise this executable.
    fn launch_command() -> String {
        let installed = home().join(".local/bin").join(APP_ID);
        if installed.exists() {
            return installed.display().to_string();
        }
        crate::config::self_exe().display().to_string()
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
        fs::write(
            &path,
            format!(
                "[Desktop Entry]\nType=Application\nName={APP_NAME}\n\
                 Comment=Switch the monitor input over DDC/CI from the system tray\n\
                 Exec={}\nIcon={APP_ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nX-KDE-autostart-phase=2\n",
                launch_command()
            ),
        )
    }

    /// Keeps the Exec line correct if the app was moved or reinstalled.
    pub fn refresh_path() {
        if let Ok(text) = fs::read_to_string(path())
            && !text.contains(&format!("Exec={}\n", launch_command()))
        {
            let _ = set_enabled(true);
        }
    }
}
