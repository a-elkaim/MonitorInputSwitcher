//! DDC/CI through the Windows Monitor Configuration API (dxva2.dll).
//!
//! Monitors are identified by their GDI device name ("\\.\DISPLAY1"), with "#2", "#3"... for the
//! rare extra physical monitors behind one display. The model name comes from the EDID through
//! QueryDisplayConfig ("AW3926QW"), falling back to the physical monitor description.

use crate::ddc::{DisplayInfo, INPUT_VCP};
use std::collections::HashMap;
use windows::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DestroyPhysicalMonitor,
    DisplayConfigGetDeviceInfo, GetCapabilitiesStringLength, GetDisplayConfigBufferSizes,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    SetVCPFeature,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::core::BOOL;

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end]).trim().to_string()
}

fn last_error(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

/// A physical monitor handle. Destroyed on drop.
pub struct Bus {
    handle: HANDLE,
}

// Physical monitor handles may be used from any thread; the app uses each from one thread at a time.
unsafe impl Send for Bus {}

impl Drop for Bus {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyPhysicalMonitor(self.handle);
        }
    }
}

impl Bus {
    pub fn get_vcp(&mut self, vcp: u8) -> Result<u32, String> {
        let (mut current, mut max) = (0u32, 0u32);
        let ok = unsafe {
            GetVCPFeatureAndVCPFeatureReply(self.handle, vcp, None, &mut current, Some(&mut max))
        };
        if ok == 0 {
            return Err(last_error("The monitor did not answer DDC/CI"));
        }
        Ok(current)
    }

    pub fn set_vcp(&mut self, vcp: u8, value: u16) -> Result<(), String> {
        if unsafe { SetVCPFeature(self.handle, vcp, u32::from(value)) } == 0 {
            return Err(last_error(&format!(
                "The monitor rejected the change (value {value})"
            )));
        }
        Ok(())
    }

    /// The monitor's capabilities string (takes a second or two).
    pub fn capabilities(&mut self) -> Result<String, String> {
        let mut len = 0u32;
        if unsafe { GetCapabilitiesStringLength(self.handle, &mut len) } == 0 || len == 0 {
            return Err(last_error("Could not read the monitor's capabilities"));
        }
        let mut buf = vec![0u8; len as usize];
        if unsafe { CapabilitiesRequestAndCapabilitiesReply(self.handle, &mut buf) } == 0 {
            return Err(last_error("Could not read the monitor's capabilities"));
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Ok(String::from_utf8_lossy(&buf[..end]).trim().to_string())
    }
}

struct Physical {
    id: String,
    description: String,
    bus: Bus,
}

/// Every physical monitor, with an open handle.
fn enumerate() -> Vec<Physical> {
    unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(h);
        BOOL(1)
    }
    let mut hmonitors: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut hmonitors as *mut Vec<HMONITOR> as isize),
        );
    }

    let mut out = Vec::new();
    for hmon in hmonitors {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let device = if unsafe { GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO) }
            .as_bool()
        {
            wide_to_string(&info.szDevice)
        } else {
            String::new()
        };
        let mut count = 0u32;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut count) }.is_err()
            || count == 0
        {
            continue;
        }
        let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(hmon, &mut physical) }.is_err() {
            continue;
        }
        for (i, pm) in physical.iter().enumerate() {
            // PHYSICAL_MONITOR is packed: copy the fields out before borrowing them.
            let description = pm.szPhysicalMonitorDescription;
            let handle = pm.hPhysicalMonitor;
            out.push(Physical {
                id: if i == 0 {
                    device.clone()
                } else {
                    format!("{device}#{}", i + 1)
                },
                description: wide_to_string(&description),
                bus: Bus { handle },
            });
        }
    }
    out
}

/// Whether a display connection is a laptop's built-in panel.
fn is_builtin(tech: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> bool {
    [
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
    ]
    .contains(&tech)
}

/// GDI device name ("\\.\DISPLAY1") -> (EDID friendly name ("AW3926QW"), built-in panel). A
/// display mirrored to several screens counts as built-in only if all of them are, and is named
/// after an external one.
fn friendly_names() -> HashMap<String, (String, bool)> {
    let mut names = HashMap::new();
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        } != ERROR_SUCCESS
        {
            return names;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let err = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if err == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if err != ERROR_SUCCESS {
            return names;
        }
        for path in &paths[..path_count as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
                continue;
            }
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
                continue;
            }
            let entry = (
                wide_to_string(&target.monitorFriendlyDeviceName),
                is_builtin(path.targetInfo.outputTechnology),
            );
            names
                .entry(wide_to_string(&source.viewGdiDeviceName))
                .and_modify(|e| {
                    if e.1 && !entry.1 {
                        *e = entry.clone();
                    }
                })
                .or_insert(entry);
        }
        return names;
    }
    names
}

/// All monitors Windows reports, including ones whose DDC/CI channel does not respond, except a
/// laptop's built-in screen (it has no inputs to switch).
pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    let names = friendly_names();
    let mut builtin = false;
    let displays: Vec<DisplayInfo> = enumerate()
        .into_iter()
        .filter_map(|mut p| {
            let gdi = p.id.split('#').next().unwrap_or_default().to_string();
            let target = names.get(&gdi);
            if target.is_some_and(|t| t.1) {
                builtin = true;
                return None;
            }
            let model = target
                .map(|t| t.0.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| p.description.clone());
            let valid = p.bus.get_vcp(INPUT_VCP).is_ok();
            Some(DisplayInfo {
                bus: p.id,
                model,
                mfg: String::new(),
                serial: String::new(),
                connector: p.description,
                valid,
            })
        })
        .collect();
    if displays.is_empty() {
        return Err(if builtin {
            "No external monitor found (the built-in screen is ignored)".into()
        } else {
            "No monitor found".into()
        });
    }
    Ok(displays)
}

/// Whether a configured `MonitorDevice` is a Windows one ("\\.\DISPLAY1").
pub fn is_device(device: &str) -> bool {
    device.starts_with(r"\\.\DISPLAY")
}

pub fn open(device: &str) -> Result<Bus, String> {
    enumerate()
        .into_iter()
        .find(|p| p.id.eq_ignore_ascii_case(device))
        .map(|p| p.bus)
        .ok_or_else(|| format!("Monitor {device} is not connected"))
}
