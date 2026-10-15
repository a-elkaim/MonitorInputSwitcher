//! Windows backend: DDC/CI through dxva2, a Shell_NotifyIcon tray with balloon notifications,
//! RegisterHotKey shortcuts, a named pipe for the CLI.

pub mod ddc;
pub mod ipc;
mod keys;
mod ui;

pub use ui::Ui;

use std::path::PathBuf;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

pub const META_NAME: &str = "Win";
pub const START_WITH_LOGIN: &str = "Start with Windows";
/// Appended to the Settings window's inputs hint.
pub const SHORTCUTS_HINT: &str = "";

/// %APPDATA%\MonitorInputSwitcher
pub fn config_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MonitorInputSwitcher")
}

/// The exe is a GUI program (no console window); CLI commands print to the console they were
/// started from.
pub fn attach_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Not needed: the app releases its hotkeys while the Settings window is open.
pub fn block_shortcuts_for_capture(_blocked: bool) {}

/// The character the key with this scan code types without modifiers on the current keyboard
/// layout ("²" for the key left of "1" on a French layout; dead keys such as "^" included).
pub fn layout_char(scancode: u32) -> Option<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MAPVK_VK_TO_CHAR, MAPVK_VSC_TO_VK, MapVirtualKeyW,
    };
    let vk = unsafe { MapVirtualKeyW(scancode, MAPVK_VSC_TO_VK) };
    if vk == 0 {
        return None;
    }
    // The top bit marks a dead key.
    let c = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) } & 0x7FFF_FFFF;
    char::from_u32(c).filter(|c| *c != '\0' && !c.is_control())
}

/// "Start with Windows": the per-user Run key (no admin rights needed).
pub mod autostart {
    use std::io;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
        REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
        RegQueryValueExW, RegSetValueExW,
    };
    use windows::core::{PCWSTR, w};

    const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const VALUE: PCWSTR = w!("MonitorInputSwitcher");

    fn command() -> String {
        format!("\"{}\"", crate::config::self_exe().display())
    }

    fn read() -> Option<String> {
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_READ, &mut key) }
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let rc = unsafe {
            RegQueryValueExW(
                key,
                VALUE,
                None,
                Some(&mut kind),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut len),
            )
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        if rc != ERROR_SUCCESS || kind != REG_SZ {
            return None;
        }
        let chars = (len as usize / 2).min(buf.len());
        let end = buf[..chars].iter().position(|c| *c == 0).unwrap_or(chars);
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    pub fn is_enabled() -> bool {
        read().is_some()
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let mut key = HKEY::default();
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc.0 as i32));
        }
        let rc = if enabled {
            let mut data: Vec<u16> = command().encode_utf16().collect();
            data.push(0);
            let bytes =
                unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };
            unsafe { RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes)) }
        } else {
            match unsafe { RegDeleteValueW(key, VALUE) } {
                rc if rc.0 == 2 => ERROR_SUCCESS, // not found: already off
                rc => rc,
            }
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        if rc != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(rc.0 as i32));
        }
        Ok(())
    }

    /// Keeps the registered path correct if the exe was moved.
    pub fn refresh_path() {
        if let Some(current) = read()
            && current != command()
        {
            let _ = set_enabled(true);
        }
    }
}
