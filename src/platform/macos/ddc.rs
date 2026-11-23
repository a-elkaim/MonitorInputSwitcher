//! DDC/CI on Apple Silicon, through IOAVService: the private IOKit API that MonitorControl, m1ddc
//! and BetterDisplay use. It sends raw I2C to the monitor, so the packets are the shared DDC/CI
//! ones (see `ddcci`), with the 0x51 sub-address passed as the I2C register.
//!
//! In the IORegistry, each external display is a framebuffer (`AppleCLCD2`, or
//! `IOMobileFramebufferShim` on M1) whose `DisplayAttributes` hold the EDID model name, followed
//! in iteration order by its `DCPAVServiceProxy` (`"Location" = "External"`). The built-in screen's
//! proxy says `"Embedded"` and is skipped. Displays are identified by the framebuffer's parent node
//! name ("dispext0"), which follows the physical port.
//!
//! Intel Macs use a different API (IOFramebuffer I2C) and aren't supported.

use crate::ddc::{DisplayInfo, INPUT_VCP};
use crate::platform::ddcci::{
    caps_request_packet, get_vcp_packet, parse_caps_fragment, parse_vcp_reply, set_vcp_packet,
};
use std::ffi::{CStr, CString, c_char, c_void};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// The monitor needs this long between a request and reading its reply...
const REPLY_DELAY: Duration = Duration::from_millis(40);
/// ...and between two commands.
const COMMAND_GAP: Duration = Duration::from_millis(50);
const TRIES: usize = 3;
const CAPS_DELAY: Duration = Duration::from_millis(50);
const CAPS_MAX_LEN: usize = 4096;
const DDC_ADDR: u32 = 0x37;
const DDC_SUBADDR: u32 = 0x51;

// --- IOKit / CoreFoundation ------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type IoObject = u32;
type KernReturn = i32;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
}

const UTF8: u32 = 0x0800_0100;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryGetRootEntry(main_port: u32) -> IoObject;
    fn IORegistryEntryCreateIterator(
        entry: IoObject,
        plane: *const c_char,
        options: u32,
        iterator: *mut IoObject,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IOObjectGetClass(object: IoObject, class_name: *mut c_char) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFTypeRef,
        alloc: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetParentEntry(
        entry: IoObject,
        plane: *const c_char,
        parent: *mut IoObject,
    ) -> KernReturn;
    fn IORegistryEntryGetName(entry: IoObject, name: *mut c_char) -> KernReturn;
}

#[cfg(target_arch = "aarch64")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOAVServiceCreateWithService(alloc: CFTypeRef, service: IoObject) -> CFTypeRef;
    fn IOAVServiceReadI2C(
        service: CFTypeRef,
        chip: u32,
        offset: u32,
        buf: *mut u8,
        len: u32,
    ) -> KernReturn;
    fn IOAVServiceWriteI2C(
        service: CFTypeRef,
        chip: u32,
        offset: u32,
        buf: *const u8,
        len: u32,
    ) -> KernReturn;
}

const IO_SERVICE_PLANE: &CStr = c"IOService";
const ITERATE_RECURSIVELY: u32 = 1;

/// An owned CoreFoundation object.
struct Cf(CFTypeRef);

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

impl Cf {
    fn new(r: CFTypeRef) -> Option<Cf> {
        if r.is_null() { None } else { Some(Cf(r)) }
    }

    fn string(s: &str) -> Cf {
        let c = CString::new(s).unwrap_or_default();
        Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
    }
}

/// A CFString's text.
fn cf_to_string(s: CFTypeRef) -> Option<String> {
    if s.is_null() || unsafe { CFGetTypeID(s) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    unsafe { CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, UTF8) }
        .then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

/// A value of a CFDictionary (borrowed from it).
fn dict_get(dict: CFTypeRef, key: &str) -> Option<CFTypeRef> {
    if dict.is_null() || unsafe { CFGetTypeID(dict) != CFDictionaryGetTypeID() } {
        return None;
    }
    let key = Cf::string(key);
    let v = unsafe { CFDictionaryGetValue(dict, key.0) };
    (!v.is_null()).then_some(v)
}

/// An owned IORegistry entry.
struct Entry(IoObject);

impl Drop for Entry {
    fn drop(&mut self) {
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

impl Entry {
    fn class(&self) -> String {
        let mut buf = [0 as c_char; 128];
        unsafe { IOObjectGetClass(self.0, buf.as_mut_ptr()) };
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn property(&self, key: &str) -> Option<Cf> {
        let key = Cf::string(key);
        Cf::new(unsafe { IORegistryEntryCreateCFProperty(self.0, key.0, std::ptr::null(), 0) })
    }

    fn parent_name(&self) -> Option<String> {
        let mut parent = 0;
        if unsafe { IORegistryEntryGetParentEntry(self.0, IO_SERVICE_PLANE.as_ptr(), &mut parent) }
            != 0
        {
            return None;
        }
        let parent = Entry(parent);
        let mut buf = [0 as c_char; 128];
        (unsafe { IORegistryEntryGetName(parent.0, buf.as_mut_ptr()) } == 0).then(|| {
            unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }
}

// --- discovery -------------------------------------------------------------------------------------

struct Framebuffer {
    id: String,
    model: String,
    mfg: String,
    serial: String,
}

impl Framebuffer {
    fn read(entry: &Entry) -> Framebuffer {
        let attrs = entry.property("DisplayAttributes");
        let product = attrs
            .as_ref()
            .and_then(|a| dict_get(a.0, "ProductAttributes"));
        let text = |key: &str| {
            product
                .and_then(|p| dict_get(p, key))
                .and_then(cf_to_string)
                .unwrap_or_default()
        };
        Framebuffer {
            id: entry.parent_name().unwrap_or_default(),
            model: text("ProductName"),
            mfg: text("ManufacturerID"),
            serial: text("AlphanumericSerialNumber"),
        }
    }
}

struct Found {
    info: DisplayInfo,
    service: Option<Cf>,
}

/// Every external display with its AV service, in IORegistry order.
fn discover() -> Vec<Found> {
    let mut iter = 0;
    let root = unsafe { IORegistryGetRootEntry(0) };
    if unsafe {
        IORegistryEntryCreateIterator(
            root,
            IO_SERVICE_PLANE.as_ptr(),
            ITERATE_RECURSIVELY,
            &mut iter,
        )
    } != 0
    {
        return Vec::new();
    }
    let iter = Entry(iter);
    let mut out = Vec::new();
    let mut framebuffer: Option<Framebuffer> = None;
    loop {
        let raw = unsafe { IOIteratorNext(iter.0) };
        if raw == 0 {
            break;
        }
        let entry = Entry(raw);
        match entry.class().as_str() {
            "AppleCLCD2" | "IOMobileFramebufferShim" => framebuffer = Some(Framebuffer::read(&entry)),
            "DCPAVServiceProxy" => {
                let external = entry
                    .property("Location")
                    .and_then(|l| cf_to_string(l.0))
                    .is_some_and(|l| l == "External");
                let Some(fb) = framebuffer.take() else {
                    continue;
                };
                if !external {
                    continue; // the built-in screen: no inputs to switch
                }
                let service = create_service(&entry);
                let id = if fb.id.is_empty() {
                    format!("display{}", out.len() + 1)
                } else {
                    fb.id
                };
                out.push(Found {
                    info: DisplayInfo {
                        bus: id,
                        model: fb.model,
                        mfg: fb.mfg,
                        serial: fb.serial,
                        connector: String::new(),
                        valid: false,
                    },
                    service,
                });
            }
            _ => {}
        }
    }
    out
}

#[cfg(target_arch = "aarch64")]
fn create_service(entry: &Entry) -> Option<Cf> {
    Cf::new(unsafe { IOAVServiceCreateWithService(std::ptr::null(), entry.0) })
}

#[cfg(not(target_arch = "aarch64"))]
fn create_service(_entry: &Entry) -> Option<Cf> {
    None
}

// --- I2C -------------------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
fn write_i2c(service: &Cf, packet: &[u8]) -> Result<(), String> {
    // The packet starts with the 0x51 sub-address, which IOAVService takes as the register.
    let data = &packet[1..];
    let rc = unsafe {
        IOAVServiceWriteI2C(service.0, DDC_ADDR, DDC_SUBADDR, data.as_ptr(), data.len() as u32)
    };
    if rc != 0 {
        return Err(format!(
            "The monitor does not answer DDC/CI (is DDC/CI enabled in its on-screen menu?) (IOKit error 0x{rc:08X})"
        ));
    }
    Ok(())
}

#[cfg(target_arch = "aarch64")]
fn read_i2c(service: &Cf, buf: &mut [u8]) -> Result<(), String> {
    let rc = unsafe {
        IOAVServiceReadI2C(service.0, DDC_ADDR, DDC_SUBADDR, buf.as_mut_ptr(), buf.len() as u32)
    };
    if rc != 0 {
        return Err(format!(
            "No reply from the monitor over DDC/CI (IOKit error 0x{rc:08X})"
        ));
    }
    Ok(())
}

#[cfg(not(target_arch = "aarch64"))]
fn write_i2c(_service: &Cf, _packet: &[u8]) -> Result<(), String> {
    Err(INTEL.into())
}

#[cfg(not(target_arch = "aarch64"))]
fn read_i2c(_service: &Cf, _buf: &mut [u8]) -> Result<(), String> {
    Err(INTEL.into())
}

#[cfg(not(target_arch = "aarch64"))]
const INTEL: &str = "DDC/CI is only supported on Apple Silicon Macs";

/// One display's DDC/CI channel, keeping the inter-command gap the monitor needs.
pub struct Bus {
    service: Cf,
    last: Option<Instant>,
}

// The service is a CF object, usable from any thread; the app uses it from one thread at a time.
unsafe impl Send for Bus {}

impl Bus {
    fn wait_gap(&self) {
        if let Some(elapsed) = self.last.map(|t| t.elapsed())
            && elapsed < COMMAND_GAP
        {
            sleep(COMMAND_GAP - elapsed);
        }
    }

    /// Runs `op` up to TRIES times, spacing the attempts.
    fn retry<T>(&mut self, mut op: impl FnMut(&Cf) -> Result<T, String>) -> Result<T, String> {
        let mut last_err = String::new();
        for _ in 0..TRIES {
            self.wait_gap();
            let result = op(&self.service);
            self.last = Some(Instant::now());
            match result {
                Ok(v) => return Ok(v),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    pub fn get_vcp(&mut self, vcp: u8) -> Result<u32, String> {
        self.retry(|s| {
            write_i2c(s, &get_vcp_packet(vcp))?;
            sleep(REPLY_DELAY);
            let mut reply = [0u8; 11];
            read_i2c(s, &mut reply)?;
            parse_vcp_reply(vcp, &reply)
        })
    }

    pub fn set_vcp(&mut self, vcp: u8, value: u16) -> Result<(), String> {
        self.retry(|s| write_i2c(s, &set_vcp_packet(vcp, value)))
    }

    /// The monitor's capabilities string (~1-3 s: it arrives in 32-byte fragments).
    pub fn capabilities(&mut self) -> Result<String, String> {
        let mut text: Vec<u8> = Vec::new();
        loop {
            let offset = u16::try_from(text.len()).map_err(|_| "Capabilities string too long")?;
            let data = self.retry(|s| {
                write_i2c(s, &caps_request_packet(offset))?;
                sleep(CAPS_DELAY);
                let mut reply = [0u8; 39];
                read_i2c(s, &mut reply)?;
                match parse_caps_fragment(&reply)? {
                    (got, data) if got == offset => Ok(data),
                    (got, _) => Err(format!("Capabilities fragment at {got}, expected {offset}")),
                }
            })?;
            if data.is_empty() || text.len() + data.len() > CAPS_MAX_LEN {
                break;
            }
            text.extend_from_slice(&data);
        }
        while text.last() == Some(&0) {
            text.pop();
        }
        if text.is_empty() {
            return Err("The monitor returned no capabilities".into());
        }
        Ok(String::from_utf8_lossy(&text).trim().to_string())
    }
}

/// All external displays, including ones whose DDC/CI channel does not respond (valid = false).
/// The built-in screen is left out: it has no inputs to switch.
pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    let displays: Vec<DisplayInfo> = discover()
        .into_iter()
        .map(|f| {
            let mut info = f.info;
            info.valid = f.service.is_some_and(|service| {
                Bus {
                    service,
                    last: None,
                }
                .get_vcp(INPUT_VCP)
                .is_ok()
            });
            info
        })
        .collect();
    if displays.is_empty() {
        return Err(if cfg!(target_arch = "aarch64") {
            "No external monitor found (the built-in screen is ignored)".into()
        } else {
            "DDC/CI is only supported on Apple Silicon Macs".into()
        });
    }
    Ok(displays)
}

/// Whether a configured `MonitorDevice` is a macOS one ("dispext0").
pub fn is_device(device: &str) -> bool {
    device.starts_with("disp")
}

pub fn open(device: &str) -> Result<Bus, String> {
    let found = discover()
        .into_iter()
        .find(|f| f.info.bus == device)
        .ok_or_else(|| format!("Monitor {device} is not connected"))?;
    let service = found
        .service
        .ok_or_else(|| format!("No DDC/CI channel for {device}"))?;
    Ok(Bus {
        service,
        last: None,
    })
}
