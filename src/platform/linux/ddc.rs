//! DDC/CI over the kernel's i2c-dev interface (/dev/i2c-N), without ddcutil.
//!
//! A switch is one 7-byte I2C write to the monitor (address 0x37) and takes ~25 ms; reading the
//! input takes ~90 ms (the monitor needs 40 ms between request and reply). Calls block, so run them
//! off the UI thread, one at a time.
//!
//! Protocol (VESA DDC/CI 1.1):
//! - Set VCP:  51 84 03 <vcp> <hi> <lo> <chk>        chk = 0x6E ^ xor(bytes)
//! - Get VCP:  51 82 01 <vcp> <chk>                  then wait 40 ms and read 11 bytes:
//!   6E 88 02 <result> <vcp> <type> <max hi> <max lo> <cur hi> <cur lo> <chk>   chk = 0x50 ^ xor(bytes)
//! - Capabilities: 51 83 F3 <offset hi> <offset lo> <chk>, then read fragments
//!   6E <0x80|len> E3 <offset hi> <offset lo> <data...> <chk> until an empty one. The text lists the
//!   supported features, e.g. "vcp(... 60(0F 11 1B) ...)" = the accepted input codes.

use crate::ddc::{DisplayInfo, INPUT_VCP};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

const DDC_ADDR: u16 = 0x37;
const EDID_ADDR: u16 = 0x50;
const I2C_SLAVE: libc::c_ulong = 0x0703;
/// The monitor needs this long between a request and reading its reply...
const REPLY_DELAY: Duration = Duration::from_millis(40);
/// ...and between two commands.
const COMMAND_GAP: Duration = Duration::from_millis(50);
const TRIES: usize = 3;
/// Capabilities fragments need a little longer than a VCP reply.
const CAPS_DELAY: Duration = Duration::from_millis(50);
const CAPS_MAX_LEN: usize = 4096;

// --- i2c-dev ---------------------------------------------------------------------------------------

fn open_bus(bus: u32, addr: u16) -> Result<File, String> {
    let path = format!("/dev/i2c-{bus}");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| open_error(&path, e))?;
    if unsafe { libc::ioctl(file.as_raw_fd(), I2C_SLAVE, libc::c_ulong::from(addr)) } < 0 {
        return Err(format!(
            "{path}: cannot address the monitor: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(file)
}

fn open_error(path: &str, e: io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound if !Path::new("/sys/module/i2c_dev").exists() => {
            "The i2c-dev kernel module is not loaded (sudo modprobe i2c-dev; see the README)".into()
        }
        io::ErrorKind::PermissionDenied => format!(
            "No permission to open {path}. Install the udev rule from the README (or join the i2c group), \
             then log in again."
        ),
        _ => format!("{path}: {e}"),
    }
}

fn io_error(e: io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::ENXIO) | Some(libc::EREMOTEIO) | Some(libc::EIO) | Some(libc::ETIMEDOUT) => {
            "The monitor does not answer DDC/CI (is DDC/CI enabled in its on-screen menu?)".into()
        }
        _ => format!("I2C error: {e}"),
    }
}

fn xor(bytes: &[u8], init: u8) -> u8 {
    bytes.iter().fold(init, |acc, b| acc ^ b)
}

fn set_vcp_packet(vcp: u8, value: u16) -> [u8; 7] {
    let [hi, lo] = value.to_be_bytes();
    let msg = [0x51, 0x84, 0x03, vcp, hi, lo];
    let mut out = [0u8; 7];
    out[..6].copy_from_slice(&msg);
    out[6] = xor(&msg, 0x6E);
    out
}

fn get_vcp_packet(vcp: u8) -> [u8; 5] {
    let msg = [0x51, 0x82, 0x01, vcp];
    [msg[0], msg[1], msg[2], msg[3], xor(&msg, 0x6E)]
}

fn caps_request_packet(offset: u16) -> [u8; 6] {
    let [hi, lo] = offset.to_be_bytes();
    let msg = [0x51, 0x83, 0xF3, hi, lo];
    [msg[0], msg[1], msg[2], msg[3], msg[4], xor(&msg, 0x6E)]
}

/// Parses one capabilities reply fragment into (offset, data).
fn parse_caps_fragment(r: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let len = usize::from(r.get(1).copied().unwrap_or(0) & 0x7F); // opcode + offset + data
    if len < 3 || r.len() < len + 3 || r[2] != 0xE3 {
        return Err(format!(
            "Unexpected capabilities reply: {}",
            hex(&r[..r.len().min(8)])
        ));
    }
    if xor(&r[..len + 2], 0x50) != r[len + 2] {
        return Err("Capabilities reply checksum mismatch".into());
    }
    Ok((u16::from_be_bytes([r[3], r[4]]), r[5..len + 2].to_vec()))
}

/// Parses a "VCP feature reply"; returns the current value (high byte included).
fn parse_vcp_reply(vcp: u8, r: &[u8]) -> Result<u32, String> {
    if r.len() < 11 || r[1] & 0x7F == 0 {
        // 6E 80 BE is the "null message": the monitor is busy.
        return Err("The monitor sent an empty reply".into());
    }
    if r[1] != 0x88 || r[2] != 0x02 || r[4] != vcp {
        return Err(format!("Unexpected DDC reply: {}", hex(r)));
    }
    if xor(&r[..10], 0x50) != r[10] {
        return Err(format!("DDC reply checksum mismatch: {}", hex(r)));
    }
    if r[3] != 0 {
        return Err(format!("The monitor does not support VCP 0x{vcp:02X}"));
    }
    Ok(u32::from(u16::from_be_bytes([r[8], r[9]])))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One DDC/CI channel, keeping the inter-command gap the monitor needs.
pub struct Bus {
    bus: u32,
    last: Option<Instant>,
}

impl Bus {
    fn wait_gap(&self) {
        if let Some(elapsed) = self.last.map(|t| t.elapsed())
            && elapsed < COMMAND_GAP
        {
            sleep(COMMAND_GAP - elapsed);
        }
    }

    pub fn get_vcp(&mut self, vcp: u8) -> Result<u32, String> {
        let mut last_err = String::new();
        for _ in 0..TRIES {
            self.wait_gap();
            let result = (|| {
                let mut f = open_bus(self.bus, DDC_ADDR)?;
                f.write_all(&get_vcp_packet(vcp)).map_err(io_error)?;
                sleep(REPLY_DELAY);
                let mut reply = [0u8; 11];
                f.read_exact(&mut reply).map_err(io_error)?;
                parse_vcp_reply(vcp, &reply)
            })();
            self.last = Some(Instant::now());
            match result {
                Ok(v) => return Ok(v),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// The monitor's capabilities string (~1-2 s: it arrives in 32-byte fragments).
    pub fn capabilities(&mut self) -> Result<String, String> {
        let mut text: Vec<u8> = Vec::new();
        loop {
            let offset = u16::try_from(text.len()).map_err(|_| "Capabilities string too long")?;
            let mut last_err = String::new();
            let mut fragment = None;
            for _ in 0..TRIES {
                self.wait_gap();
                let result = (|| {
                    let mut f = open_bus(self.bus, DDC_ADDR)?;
                    f.write_all(&caps_request_packet(offset))
                        .map_err(io_error)?;
                    sleep(CAPS_DELAY);
                    let mut reply = [0u8; 39];
                    f.read_exact(&mut reply).map_err(io_error)?;
                    parse_caps_fragment(&reply)
                })();
                self.last = Some(Instant::now());
                match result {
                    Ok((got, data)) if got == offset => {
                        fragment = Some(data);
                        break;
                    }
                    Ok((got, _)) => {
                        last_err = format!("Capabilities fragment at {got}, expected {offset}")
                    }
                    Err(e) => last_err = e,
                }
            }
            let data = fragment.ok_or(last_err)?;
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

    pub fn set_vcp(&mut self, vcp: u8, value: u16) -> Result<(), String> {
        let mut last_err = String::new();
        for _ in 0..TRIES {
            self.wait_gap();
            let result = open_bus(self.bus, DDC_ADDR)
                .and_then(|mut f| f.write_all(&set_vcp_packet(vcp, value)).map_err(io_error));
            self.last = Some(Instant::now());
            match result {
                Ok(()) => return Ok(()),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }
}

// --- detection -------------------------------------------------------------------------------------

/// I2C buses that belong to a graphics adapter (PCI class 0x03xxxx): the same buses ddcutil probes.
/// Other buses (SMBus, touchpads, sensors) are never touched, nor buses of a DRM connector that
/// reports "disconnected".
fn display_buses() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/sys/bus/i2c/devices") else {
        return Vec::new();
    };
    let mut buses: Vec<u32> = entries
        .flatten()
        .filter_map(|e| {
            let n: u32 = e.file_name().to_str()?.strip_prefix("i2c-")?.parse().ok()?;
            let mut dir = std::fs::canonicalize(e.path()).ok()?;
            if let Ok(status) = std::fs::read_to_string(dir.with_file_name("status"))
                && status.trim() == "disconnected"
            {
                return None;
            }
            while dir.pop() {
                if let Ok(class) = std::fs::read_to_string(dir.join("class")) {
                    return class.trim().starts_with("0x03").then_some(n);
                }
            }
            None
        })
        .collect();
    buses.sort_unstable();
    buses
}

fn read_edid(bus: u32) -> Option<Vec<u8>> {
    let mut f = open_bus(bus, EDID_ADDR).ok()?;
    f.write_all(&[0]).ok()?;
    let mut edid = vec![0u8; 128];
    f.read_exact(&mut edid).ok()?;
    (edid[..8] == [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]).then_some(edid)
}

struct EdidInfo {
    mfg: String,
    model: String,
    serial: String,
}

fn parse_edid(edid: &[u8]) -> EdidInfo {
    let id = u16::from_be_bytes([edid[8], edid[9]]);
    let mfg: String = [10, 5, 0]
        .iter()
        .map(|shift| char::from(b'@' + ((id >> shift) & 0x1F) as u8))
        .collect();
    let (mut model, mut serial) = (String::new(), String::new());
    for block in edid[54..126].chunks(18) {
        if block[..3] == [0, 0, 0] {
            let text = String::from_utf8_lossy(&block[5..])
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            match block[3] {
                0xFC => model = text,
                0xFF => serial = text,
                _ => {}
            }
        }
    }
    if serial.is_empty() {
        let n = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
        if n != 0 {
            serial = n.to_string();
        }
    }
    EdidInfo { mfg, model, serial }
}

/// The DRM connector ("card2-DP-8") whose EDID matches, for display only.
fn connector_for(edid: &[u8]) -> String {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return String::new();
    };
    entries
        .flatten()
        .find(|e| {
            std::fs::read(e.path().join("edid"))
                .is_ok_and(|d| d.len() >= 128 && d[..128] == edid[..128])
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether a DRM connector ("card1-eDP-1") is a laptop's built-in panel.
fn is_builtin(connector: &str) -> bool {
    let kind = connector.split_once('-').map_or(connector, |(_, rest)| rest);
    ["eDP-", "LVDS-", "DSI-"].iter().any(|p| kind.starts_with(p))
}

/// All displays with a readable EDID on a graphics adapter's I2C bus, including ones whose DDC/CI
/// channel does not respond (valid = false), except a laptop's built-in screen (it has no inputs to
/// switch).
pub fn list_displays() -> Result<Vec<DisplayInfo>, String> {
    let buses = display_buses();
    if buses.is_empty() {
        return Err(if Path::new("/sys/module/i2c_dev").exists() {
            "No display I2C buses found".into()
        } else {
            "The i2c-dev kernel module is not loaded (sudo modprobe i2c-dev; see the README)".into()
        });
    }
    // Empty buses take 100-600 ms to time out, so probe them all at once.
    let results: Vec<Result<Option<DisplayInfo>, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = buses
            .iter()
            .map(|&bus| scope.spawn(move || probe(bus)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Ok(None)))
            .collect()
    });
    let mut denied = None;
    let mut displays = Vec::new();
    for r in results {
        match r {
            Ok(Some(d)) => displays.push(d),
            Ok(None) => {}
            Err(e) => denied = Some(e),
        }
    }
    match (displays.is_empty(), denied) {
        (true, Some(e)) => Err(e),
        _ => Ok(displays),
    }
}

/// The display on one bus, if it has a readable EDID. Err only for permission problems.
fn probe(bus: u32) -> Result<Option<DisplayInfo>, String> {
    if let Err(e) = open_bus(bus, EDID_ADDR) {
        return if e.starts_with("No permission") {
            Err(e)
        } else {
            Ok(None)
        };
    }
    let Some(edid) = read_edid(bus) else {
        return Ok(None);
    };
    let connector = connector_for(&edid);
    if is_builtin(&connector) {
        return Ok(None);
    }
    let info = parse_edid(&edid);
    let valid = Bus { bus, last: None }.get_vcp(INPUT_VCP).is_ok();
    Ok(Some(DisplayInfo {
        bus: format!("/dev/i2c-{bus}"),
        model: info.model,
        mfg: info.mfg,
        serial: info.serial,
        connector,
        valid,
    }))
}

fn bus_number(device: &str) -> Option<u32> {
    device.strip_prefix("/dev/i2c-")?.parse().ok()
}

/// Whether a configured `MonitorDevice` is a Linux one ("/dev/i2c-N").
pub fn is_device(device: &str) -> bool {
    bus_number(device).is_some()
}

pub fn open(device: &str) -> Result<Bus, String> {
    let bus = bus_number(device).ok_or_else(|| format!("Unexpected bus name {device}"))?;
    Ok(Bus { bus, last: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddc::matches;

    #[test]
    fn builtin_connectors() {
        assert!(is_builtin("card1-eDP-1"));
        assert!(is_builtin("card0-LVDS-1"));
        assert!(is_builtin("card0-DSI-1"));
        assert!(!is_builtin("card2-DP-8"));
        assert!(!is_builtin("card1-HDMI-A-1"));
        assert!(!is_builtin(""));
    }

    #[test]
    fn packets() {
        // Checksum = 0x6E (monitor address) XOR every byte.
        assert_eq!(
            set_vcp_packet(0x60, 27),
            [0x51, 0x84, 0x03, 0x60, 0x00, 0x1B, 0xC3]
        );
        assert_eq!(get_vcp_packet(0x60), [0x51, 0x82, 0x01, 0x60, 0xDC]);
    }

    #[test]
    fn parses_reply() {
        // Real AW3926QW replies: DisplayPort (low byte 0x0F) and USB-C (0x1B), high byte 0x1B.
        let dp = [
            0x6e, 0x88, 0x02, 0x00, 0x60, 0x00, 0x00, 0x0e, 0x1b, 0x0f, 0xce,
        ];
        let usbc = [
            0x6e, 0x88, 0x02, 0x00, 0x60, 0x00, 0x00, 0x0e, 0x1b, 0x1b, 0xda,
        ];
        assert_eq!(parse_vcp_reply(0x60, &dp), Ok(0x1B0F));
        assert_eq!(parse_vcp_reply(0x60, &usbc), Ok(0x1B1B));
        assert!(matches(0x1B0F, 15) && !matches(0x1B0F, 27));
        assert!(matches(0x1B1B, 27));

        let mut bad = dp;
        bad[10] ^= 1;
        assert!(parse_vcp_reply(0x60, &bad).is_err());
        assert!(parse_vcp_reply(0x60, &[0x6e, 0x80, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        let mut unsupported = dp;
        unsupported[3] = 1;
        unsupported[10] = xor(&unsupported[..10], 0x50);
        assert!(
            parse_vcp_reply(0x60, &unsupported)
                .unwrap_err()
                .contains("does not support")
        );
    }

    #[test]
    fn parses_edid() {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        edid[8..10].copy_from_slice(&[0x10, 0xAC]); // "DEL"
        let mut block = [0u8; 18];
        block[3] = 0xFC;
        block[5..18].copy_from_slice(b"AW3926QW\n    ");
        edid[54..72].copy_from_slice(&block);
        block[3] = 0xFF;
        block[5..18].copy_from_slice(b"1HWQ7K4\n     ");
        edid[72..90].copy_from_slice(&block);
        let info = parse_edid(&edid);
        assert_eq!(
            (info.mfg.as_str(), info.model.as_str(), info.serial.as_str()),
            ("DEL", "AW3926QW", "1HWQ7K4")
        );
    }

    #[test]
    fn caps_fragments() {
        assert_eq!(
            caps_request_packet(0),
            [0x51, 0x83, 0xF3, 0, 0, 0x6E ^ 0x51 ^ 0x83 ^ 0xF3]
        );
        let mut frag = vec![0x6E, 0x80 | 7, 0xE3, 0x00, 0x20, b'(', b'p', b'r', b'o'];
        frag.push(xor(&frag, 0x50));
        assert_eq!(parse_caps_fragment(&frag), Ok((0x20, b"(pro".to_vec())));
        let mut empty = vec![0x6E, 0x83, 0xE3, 0x00, 0x40];
        empty.push(xor(&empty, 0x50));
        assert_eq!(parse_caps_fragment(&empty), Ok((0x40, vec![])));
    }

    #[test]
    fn device_ids() {
        assert!(!is_device(r"\\.\DISPLAY1"));
        assert_eq!(bus_number("/dev/i2c-20"), Some(20));
    }
}
