//! Monitor model shared by every platform: what a detected display looks like, the configured
//! monitor (found once, cached, re-detected after a failure), the DDC/CI capabilities string, and
//! MCCS input names. The actual DDC/CI I/O lives in `platform::ddc`.
//!
//! The capabilities string lists the supported features, e.g. "vcp(... 60(0F 11 1B) ...)": the
//! input codes (VCP 0x60 values) the monitor accepts.

use crate::platform::ddc::{self, Bus};

/// Every display the platform can see, including ones whose DDC/CI channel does not answer.
pub use crate::platform::ddc::list_displays;
use serde::{Deserialize, Serialize};

pub const INPUT_VCP: u8 = 0x60;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub bus: String,   // device id: "/dev/i2c-20" (Linux) or "\\\\.\\DISPLAY1" (Windows)
    pub model: String, // EDID model name, e.g. "AW3926QW"
    pub mfg: String,
    pub serial: String,
    pub connector: String, // e.g. "card2-DP-8" (Linux DRM connector) or the monitor description (Windows)
    pub valid: bool,       // false: the EDID is readable but the monitor does not answer DDC/CI
}

impl DisplayInfo {
    pub fn label(&self) -> String {
        let extra: Vec<&str> = [self.bus.as_str(), self.connector.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        let model = if self.model.is_empty() {
            "(unknown)"
        } else {
            &self.model
        };
        format!(
            "{model}  ({}){}",
            extra.join(", "),
            if self.valid {
                ""
            } else {
                "  [DDC not working]"
            }
        )
    }
}

/// Some monitors (AW3926QW included) put other data in the high byte: DisplayPort reads as
/// 0x0F0F on Windows and 0x1B0F on Linux.
pub fn matches(reported: u32, configured: u32) -> bool {
    reported == configured || (reported & 0xFF) == configured
}

// --- capabilities ----------------------------------------------------------------------------------

/// The text inside "<key>(...)" at the top level of a capabilities string (case-insensitive).
fn caps_section<'a>(caps: &'a str, key: &str) -> Option<&'a str> {
    let lower = caps.to_ascii_lowercase();
    let needle = format!("{key}(");
    let mut search = 0;
    while let Some(pos) = lower[search..].find(&needle).map(|p| p + search) {
        // Must be a whole word: "vcp(" but not "vcpname(" or "...xvcp(".
        let before = lower[..pos].chars().last();
        if before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_') {
            let start = pos + needle.len();
            let mut depth = 1;
            for (i, c) in caps[start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&caps[start..start + i]);
                        }
                    }
                    _ => {}
                }
            }
            return Some(&caps[start..]);
        }
        search = pos + needle.len();
    }
    None
}

/// The values a VCP feature accepts according to the capabilities, e.g. 60(0F 11 1B) -> [15, 17, 27].
pub fn caps_vcp_values(caps: &str, vcp: u8) -> Option<Vec<u32>> {
    let body = caps_section(caps, "vcp")?;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_hexdigit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            let code = u8::from_str_radix(&body[start..i], 16).ok();
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let has_values = j < bytes.len() && bytes[j] == b'(';
            if has_values {
                let end = body[j..].find(')').map_or(body.len(), |e| j + e);
                if code == Some(vcp) {
                    return Some(
                        body[j + 1..end]
                            .split_whitespace()
                            .filter_map(|v| u32::from_str_radix(v, 16).ok())
                            .collect(),
                    );
                }
                i = end + 1;
            } else if code == Some(vcp) {
                return Some(Vec::new()); // supported, but no value list
            }
        } else {
            i += 1;
        }
    }
    None
}

pub fn caps_model(caps: &str) -> Option<String> {
    caps_section(caps, "model")
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Input codes the monitor accepts for VCP 0x60, from its capabilities.
pub fn caps_inputs(caps: &str) -> Vec<u32> {
    caps_vcp_values(caps, INPUT_VCP).unwrap_or_default()
}

/// MCCS name of an input code (VCP 0x60). 0x1B (USB-C) is not in MCCS but is used by Dell and others.
pub fn input_name(value: u32) -> String {
    let name = match value & 0xFF {
        0x01 => "VGA-1",
        0x02 => "VGA-2",
        0x03 => "DVI-1",
        0x04 => "DVI-2",
        0x05 => "Composite-1",
        0x06 => "Composite-2",
        0x07 => "S-Video-1",
        0x08 => "S-Video-2",
        0x09 => "Tuner-1",
        0x0A => "Tuner-2",
        0x0B => "Tuner-3",
        0x0C => "Component-1",
        0x0D => "Component-2",
        0x0E => "Component-3",
        0x0F => "DisplayPort-1",
        0x10 => "DisplayPort-2",
        0x11 => "HDMI-1",
        0x12 => "HDMI-2",
        0x1B => "USB-C",
        _ => return format!("Input 0x{value:02X}"),
    };
    name.to_string()
}

// --- monitor ---------------------------------------------------------------------------------------

/// The configured monitor, resolved to a DDC channel once and cached; re-detected after a failure.
pub struct Monitor {
    pub name: Option<String>,
    /// Platform device id ("/dev/i2c-20" on Linux, "\\.\DISPLAY1" on Windows); one from the
    /// other OS is ignored.
    pub device: Option<String>,
    channel: Option<Bus>,
}

impl Monitor {
    pub fn new(name: Option<String>, device: Option<String>) -> Self {
        let device = device.filter(|d| ddc::is_device(d));
        Monitor {
            name,
            device,
            channel: None,
        }
    }

    pub fn is(&self, name: &Option<String>, device: &Option<String>) -> bool {
        self.name == *name && self.device == device.clone().filter(|d| ddc::is_device(d))
    }

    fn resolve(&self) -> Result<Bus, String> {
        let displays = ddc::list_displays()?;
        let name = self.name.clone().unwrap_or_default().trim().to_lowercase();
        let by_name = |d: &&DisplayInfo| {
            !name.is_empty()
                && (d.model.to_lowercase() == name || d.model.to_lowercase().contains(&name))
        };
        let valid: Vec<&DisplayInfo> = displays.iter().filter(|d| d.valid).collect();

        let mut candidates: Vec<&DisplayInfo> = valid.iter().copied().filter(by_name).collect();
        if let Some(device) = &self.device {
            let pool = if candidates.is_empty() {
                &valid
            } else {
                &candidates
            };
            let narrowed: Vec<&DisplayInfo> =
                pool.iter().copied().filter(|d| &d.bus == device).collect();
            if !narrowed.is_empty() {
                candidates = narrowed;
            }
        }
        // A single DDC-capable display: use it, unless the named monitor is present but broken.
        if candidates.is_empty() && valid.len() == 1 && !displays.iter().any(|d| by_name(&d)) {
            candidates = valid.clone();
        }
        // No monitor configured at all: the first one that answers DDC/CI.
        if candidates.is_empty() && name.is_empty() && self.device.is_none() {
            candidates = valid.iter().take(1).copied().collect();
        }
        if let Some(d) = candidates.first() {
            return ddc::open(&d.bus);
        }
        if let Some(b) = displays.iter().filter(|d| !d.valid).find(|d| by_name(d)) {
            return Err(format!(
                "{} found on {}, but it does not answer DDC/CI. Check that DDC/CI is enabled in the monitor menu \
                 (see the NVIDIA notes in the README).",
                b.model, b.bus
            ));
        }
        if valid.is_empty() {
            return Err("No DDC/CI-capable display found".into());
        }
        let found: Vec<&str> = valid
            .iter()
            .map(|d| {
                if d.model.is_empty() {
                    d.bus.as_str()
                } else {
                    d.model.as_str()
                }
            })
            .collect();
        Err(format!(
            "Monitor {} not found (detected: {})",
            self.name
                .as_deref()
                .or(self.device.as_deref())
                .unwrap_or("?"),
            found.join(", ")
        ))
    }

    /// Finds the monitor now (detection takes a moment), so the first switch is fast.
    pub fn warm_up(&mut self) {
        if self.channel.is_none() {
            self.channel = self.resolve().ok();
        }
    }

    fn with_channel<T>(&mut self, op: impl Fn(&mut Bus) -> Result<T, String>) -> Result<T, String> {
        let fresh = self.channel.is_none();
        if fresh {
            self.channel = Some(self.resolve()?);
        }
        match op(self.channel.as_mut().unwrap()) {
            Ok(v) => Ok(v),
            Err(_) if !fresh => {
                // The bus can change after hotplug or a display change; detect again once.
                self.channel = None;
                self.channel = Some(self.resolve()?);
                op(self.channel.as_mut().unwrap())
            }
            Err(e) => Err(e),
        }
    }

    pub fn get_input(&mut self) -> Result<u32, String> {
        self.with_channel(|c| c.get_vcp(INPUT_VCP))
    }

    pub fn capabilities(&mut self) -> Result<String, String> {
        self.with_channel(Bus::capabilities)
    }

    pub fn set_input(&mut self, value: u32) -> Result<(), String> {
        let value = u16::try_from(value)
            .map_err(|_| format!("Input value {value} is out of range (0-65535)"))?;
        self.with_channel(|c| c.set_vcp(INPUT_VCP, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_parsing() {
        let caps = "(prot(monitor)type(LCD)model(AW3926QW)cmds(01 02 03 07 0C E3 F3)vcp(02 04 05 08 10 12 14(05 08 0B) \
                    16 18 1A 52 60( 0F 1B 11) AC AE B2 B6 C6 C8 C9 D6(01 04 05) DC(00 03 05) DF E0 E1 E2(00 1D))\
                    mswhql(1)asset_eep(40)mccs_ver(2.1))";
        assert_eq!(caps_inputs(caps), vec![0x0F, 0x1B, 0x11]);
        assert_eq!(caps_vcp_values(caps, 0x14), Some(vec![5, 8, 0x0B]));
        assert_eq!(caps_vcp_values(caps, 0x10), Some(vec![]));
        assert_eq!(caps_vcp_values(caps, 0x99), None);
        assert_eq!(caps_model(caps).as_deref(), Some("AW3926QW"));
        // "vcpname(...)" must not be mistaken for "vcp(...)".
        assert_eq!(
            caps_inputs("(vcpname(60(Source))vcp(60(11 12)))"),
            vec![0x11, 0x12]
        );
        assert_eq!(input_name(0x0F), "DisplayPort-1");
        assert_eq!(input_name(0x1B1B), "USB-C");
        assert_eq!(input_name(0x42), "Input 0x42");
    }

    #[test]
    fn high_byte_quirk() {
        assert!(matches(0x1B0F, 15) && !matches(0x1B0F, 27));
        assert!(matches(0x0F0F, 15));
        assert!(matches(27, 27));
    }
}
