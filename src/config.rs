//! JSON config (the same schema on Linux and Windows) and the "Start with login" autostart entry.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

pub const APP_ID: &str = "monitor-input-switcher";
pub const APP_NAME: &str = "Monitor Input Switcher";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct InputEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: u32,
    #[serde(default)]
    pub hotkey: Option<String>,
    /// Whether "Next input" cycles through this input (untick inputs with nothing plugged in).
    /// Missing in older configs, which means included.
    #[serde(default = "included")]
    pub include_in_next: bool,
}

fn included() -> bool {
    true
}

impl InputEntry {
    pub fn new(name: impl Into<String>, value: u32, hotkey: Option<String>) -> Self {
        InputEntry {
            name: name.into(),
            value,
            hotkey,
            include_in_next: true,
        }
    }
}

/// The input "Next input" goes to: the first included input after the current one, in list order
/// and wrapping around. Inputs left out of the cycle are skipped, but switching away from one works.
pub fn next_input(inputs: &[InputEntry], current: Option<u32>) -> Result<&InputEntry, String> {
    if inputs.is_empty() {
        return Err("No inputs configured".into());
    }
    let start = current.and_then(|c| inputs.iter().position(|e| crate::ddc::matches(c, e.value)));
    let n = inputs.len();
    let index = match start {
        Some(s) => (1..=n)
            .map(|k| (s + k) % n)
            .find(|&i| inputs[i].include_in_next),
        None => (0..n).find(|&i| inputs[i].include_in_next),
    };
    index
        .map(|i| &inputs[i])
        .ok_or_else(|| "No input is included in \"Next input\" (see Settings)".into())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppConfig {
    pub monitor_name: Option<String>,
    pub monitor_device: Option<String>,
    pub inputs: Vec<InputEntry>,
    pub toggle_hotkey: Option<String>,
    /// A notification after each switch.
    pub show_notifications: bool,
    /// A notification when the app starts, so it's clear it is running in the tray.
    pub show_startup_notification: bool,
}

/// Monitor-agnostic defaults: no monitor and no inputs yet. The app fills them in from the
/// connected monitor on first run (see autoconfig.rs).
impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            monitor_name: None,
            monitor_device: None,
            inputs: Vec::new(),
            toggle_hotkey: Some(crate::autoconfig::TOGGLE_HOTKEY.into()),
            show_notifications: true,
            show_startup_notification: true,
        }
    }
}

/// This executable. After an upgrade replaced the file, /proc/self/exe reads "<path> (deleted)":
/// the new binary at the same path is what we want then.
pub fn self_exe() -> PathBuf {
    match std::env::current_exe() {
        Ok(p) => {
            let s = p.display().to_string();
            s.strip_suffix(" (deleted)").map(PathBuf::from).unwrap_or(p)
        }
        Err(_) => PathBuf::from(APP_ID),
    }
}

/// `~/.config/monitor-input-switcher/config.json` on Linux,
/// `%APPDATA%\MonitorInputSwitcher\config.json` on Windows.
pub fn config_path() -> PathBuf {
    crate::platform::config_dir().join("config.json")
}

pub use crate::platform::autostart;

pub enum Loaded {
    Ok(AppConfig),
    FirstRun(AppConfig),
    /// The file exists but could not be read; defaults are used.
    Error(AppConfig, String),
}

impl AppConfig {
    pub fn load() -> Loaded {
        let path = config_path();
        match fs::read_to_string(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Loaded::FirstRun(AppConfig::default()),
            Err(e) => Loaded::Error(
                AppConfig::default(),
                format!("Could not read {}: {e}", path.display()),
            ),
            Ok(text) => match serde_json::from_str(&text) {
                Ok(cfg) => Loaded::Ok(cfg),
                Err(e) => Loaded::Error(
                    AppConfig::default(),
                    format!("Could not read {}: {e}. Defaults are used.", path.display()),
                ),
            },
        }
    }

    /// The config, or defaults if it is missing or unreadable.
    pub fn load_or_default() -> AppConfig {
        match Self::load() {
            Loaded::Ok(c) | Loaded::FirstRun(c) | Loaded::Error(c, _) => c,
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        text.push('\n');
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Vec<InputEntry> {
        let mut v = vec![
            InputEntry::new("DisplayPort", 15, None),
            InputEntry::new("HDMI-1", 17, None),
            InputEntry::new("HDMI-2", 18, None),
            InputEntry::new("USB-C", 27, None),
        ];
        v[1].include_in_next = false;
        v[2].include_in_next = false;
        v
    }

    #[test]
    fn next_skips_excluded_inputs() {
        let v = inputs();
        assert_eq!(next_input(&v, Some(0x1B0F)).unwrap().name, "USB-C"); // DP -> USB-C, skipping HDMI
        assert_eq!(next_input(&v, Some(27)).unwrap().name, "DisplayPort"); // wraps around
        assert_eq!(next_input(&v, Some(17)).unwrap().name, "USB-C"); // leaving an excluded input works
        assert_eq!(next_input(&v, None).unwrap().name, "DisplayPort"); // unknown current: first included
        let mut none = v.clone();
        none.iter_mut().for_each(|e| e.include_in_next = false);
        assert!(next_input(&none, Some(15)).is_err());
        assert!(next_input(&[], None).is_err());
    }

    #[test]
    fn old_configs_include_every_input() {
        let cfg: AppConfig =
            serde_json::from_str(r#"{"Inputs":[{"Name":"DP","Value":15,"Hotkey":null}]}"#).unwrap();
        assert!(cfg.inputs[0].include_in_next);
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""IncludeInNext":true"#));
    }
}
