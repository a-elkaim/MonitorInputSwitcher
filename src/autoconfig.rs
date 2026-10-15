//! Builds a config from whatever monitor is connected: its EDID model and the input codes listed
//! in its DDC/CI capabilities. Used on first run (or whenever no inputs are configured) and by the
//! Settings window's "Detect inputs" button.

use crate::config::InputEntry;
use crate::ddc::{self, DisplayInfo, Monitor};

pub const TOGGLE_HOTKEY: &str = "Ctrl+Alt+Shift+S";

/// Display names for input codes, sorted by code: "DisplayPort" instead of "DisplayPort-1" when the
/// monitor has only one DisplayPort input, and so on.
pub fn input_entries(codes: &[u32]) -> Vec<(u32, String)> {
    let mut codes: Vec<u32> = codes.to_vec();
    codes.sort_unstable();
    codes.dedup();
    let names: Vec<String> = codes.iter().map(|c| ddc::input_name(*c)).collect();
    codes
        .iter()
        .zip(&names)
        .map(|(code, name)| {
            let short = name.strip_suffix("-1").filter(|base| {
                !names
                    .iter()
                    .any(|other| other != name && other.strip_suffix("-2") == Some(*base))
            });
            (*code, short.unwrap_or(name).to_string())
        })
        .collect()
}

/// "Ctrl+Alt+Shift+<n>" for the n-th input (1-9), None after that.
pub fn default_hotkey(index: usize) -> Option<String> {
    (index < 9).then(|| format!("Ctrl+Alt+Shift+{}", index + 1))
}

pub struct Detected {
    pub display: DisplayInfo,
    pub inputs: Vec<InputEntry>,
    /// Why the input list came from a fallback rather than the capabilities, if it did.
    pub note: Option<String>,
}

/// Picks the monitor to control: the only DDC/CI display, or the first one when there are several.
pub fn pick_display(displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    displays.iter().find(|d| d.valid)
}

/// Detects the monitor and its inputs. Blocks for a few seconds (capabilities are slow to read).
pub fn detect() -> Result<Detected, String> {
    let displays = ddc::list_displays()?;
    let display = pick_display(&displays).cloned().ok_or_else(|| {
        if displays.is_empty() {
            "No monitor found on the graphics adapters' I2C buses".to_string()
        } else {
            "No monitor answers DDC/CI (enable DDC/CI in the monitor's on-screen menu)".to_string()
        }
    })?;
    let mut monitor = Monitor::new(Some(display.model.clone()), Some(display.bus.clone()));
    let (codes, note) = match monitor.capabilities() {
        Ok(caps) if !ddc::caps_inputs(&caps).is_empty() => (ddc::caps_inputs(&caps), None),
        result => {
            // No list: at least offer the input the monitor is showing now.
            let why = match result {
                Ok(_) => "The monitor's capabilities list no input codes".to_string(),
                Err(e) => format!("Could not read the monitor's capabilities ({e})"),
            };
            match monitor.get_input() {
                Ok(v) => (
                    vec![v & 0xFF],
                    Some(format!("{why}; only the current input was added.")),
                ),
                Err(_) => (Vec::new(), Some(format!("{why}; add inputs in Settings."))),
            }
        }
    };
    let inputs = input_entries(&codes)
        .into_iter()
        .enumerate()
        .map(|(i, (value, name))| InputEntry::new(name, value, default_hotkey(i)))
        .collect();
    Ok(Detected {
        display,
        inputs,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_order() {
        // AW3926QW capabilities list 60(1B 0F 11 12).
        let entries = input_entries(&[0x1B, 0x0F, 0x11, 0x12]);
        assert_eq!(
            entries,
            vec![
                (0x0F, "DisplayPort".to_string()),
                (0x11, "HDMI-1".to_string()),
                (0x12, "HDMI-2".to_string()),
                (0x1B, "USB-C".to_string()),
            ]
        );
        assert_eq!(input_entries(&[0x10, 0x0F])[0].1, "DisplayPort-1");
        assert_eq!(default_hotkey(0).as_deref(), Some("Ctrl+Alt+Shift+1"));
        assert_eq!(default_hotkey(9), None);
    }
}
