//! JSON config (the same schema on Linux and Windows) and the "Start with login" autostart entry.

use crate::hotkey::Hotkey;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

    fn to_json(&self) -> io::Result<String> {
        let mut text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        text.push('\n');
        Ok(text)
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, self.to_json()?)?;
        fs::rename(&tmp, &path)
    }

    /// Writes the settings to a file another machine can import (same format as config.json).
    pub fn export(&self, path: &Path) -> Result<(), String> {
        self.to_json()
            .and_then(|text| fs::write(path, text))
            .map_err(|e| format!("Could not write {}: {e}", path.display()))
    }

    /// Reads settings exported by `export` (or a copy of config.json), and checks them.
    pub fn import(path: &Path) -> Result<AppConfig, String> {
        let invalid = |e: String| format!("{} is not a valid settings file: {e}", path.display());
        let read = |e: io::Error| format!("Could not read {}: {e}", path.display());
        // A settings file is a few hundred bytes; don't load a big file picked by mistake.
        if fs::metadata(path).map_err(read)?.len() > 1024 * 1024 {
            return Err(invalid("it is larger than 1 MB".into()));
        }
        let text = fs::read_to_string(path).map_err(read)?;
        Self::from_export(&text).map_err(invalid)
    }

    fn from_export(text: &str) -> Result<AppConfig, String> {
        // Windows PowerShell and older Notepad start UTF-8 files with a byte order mark.
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        // Every field has a default, so any JSON object would parse: require the fields that
        // carry the settings rather than silently filling them in.
        let inputs = value
            .get("Inputs")
            .and_then(|v| v.as_array())
            .ok_or("no \"Inputs\" list")?;
        for (i, input) in inputs.iter().enumerate() {
            for field in ["Name", "Value"] {
                if input.get(field).is_none() {
                    return Err(format!("input {} has no \"{field}\"", i + 1));
                }
            }
        }
        let mut config: AppConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let blank = |h: &mut Option<String>| {
            if h.as_deref().is_some_and(|t| t.trim().is_empty()) {
                *h = None;
            }
        };
        config.inputs.iter_mut().for_each(|e| blank(&mut e.hotkey));
        blank(&mut config.toggle_hotkey);
        config.check()?;
        Ok(config)
    }

    /// Checks what a working config needs: at least one input, each with a name and an input code
    /// in range, and valid shortcuts with no duplicates.
    pub fn check(&self) -> Result<(), String> {
        if self.inputs.is_empty() {
            return Err("Add at least one input.".into());
        }
        let mut shortcuts = Vec::new();
        for (i, e) in self.inputs.iter().enumerate() {
            let name = e.name.trim();
            if name.is_empty() {
                return Err(format!("Input {} has no name.", i + 1));
            }
            if !(1..=0xFFFF).contains(&e.value) {
                return Err(format!(
                    "{name}: input code {} is out of range (1 to 65535).",
                    e.value
                ));
            }
            shortcuts.push((name.to_string(), &e.hotkey));
        }
        shortcuts.push(("Next input".into(), &self.toggle_hotkey));
        let mut seen: Vec<Hotkey> = Vec::new();
        for (owner, text) in shortcuts {
            let Some(text) = text.as_deref().filter(|t| !t.trim().is_empty()) else {
                continue;
            };
            let hotkey =
                Hotkey::parse(text).ok_or_else(|| format!("{owner}: shortcut {text:?} is not valid."))?;
            if seen.contains(&hotkey) {
                return Err(format!("The shortcut {hotkey} is assigned more than once."));
            }
            seen.push(hotkey);
        }
        Ok(())
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

    #[test]
    fn export_round_trip() {
        let mut cfg = AppConfig::default();
        cfg.monitor_name = Some("AW3926QW".into());
        cfg.inputs = inputs();
        let text = cfg.to_json().unwrap();
        assert_eq!(AppConfig::from_export(&text).unwrap(), cfg);
        assert_eq!(AppConfig::from_export(&format!("\u{FEFF}{text}")).unwrap(), cfg);
    }

    #[test]
    fn import_rejects_bad_files() {
        let bad = [
            "",
            "not json",
            "[1, 2]",
            "{}",
            r#"{"Inputs": "DP"}"#,
            r#"{"Inputs": []}"#,
            r#"{"Inputs": [{}]}"#,
            r#"{"Inputs": [{"Name": "DP"}]}"#,
            r#"{"Inputs": [{"Value": 15}]}"#,
            r#"{"Inputs": [{"Name": " ", "Value": 15}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": "15"}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": -1}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 0}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 70000}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": "Ctrl+Banana"}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": "Ctrl+1"},
                           {"Name": "HDMI", "Value": 17, "Hotkey": "ctrl+1"}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": "Ctrl+1"}], "ToggleHotkey": "Ctrl+1"}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15}], "ShowNotifications": "yes"}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15}"#,
        ];
        for text in bad {
            assert!(AppConfig::from_export(text).is_err(), "accepted: {text}");
        }
        // Minimal and lenient: other fields default, unknown fields and blank shortcuts are ignored.
        let ok = AppConfig::from_export(
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": ""}], "Extra": 1}"#,
        )
        .unwrap();
        assert_eq!(ok.inputs[0].name, "DP");
        assert_eq!(ok.inputs[0].hotkey, None);
        assert!(ok.show_notifications);
    }
}
