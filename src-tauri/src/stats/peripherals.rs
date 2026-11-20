//! Battery levels of wireless peripherals: headsets, mice, keyboards, controllers.
//!
//! Sources, all read-only:
//! - **Bluetooth**: Windows keeps the level it reads from a device (hands-free profile
//!   for classic devices, the Battery Service for LE ones) in a device property, the
//!   value Settings › Bluetooth & devices shows. Nothing is sent to the device.
//! - **Logitech** (Unifying, Bolt and Lightspeed receivers, or a device on its own
//!   cable or Bluetooth): HID++, Logitech's published protocol. HID++ 2.0 devices
//!   (mice, keyboards, G headsets) through their battery features; older HID++ 1.0
//!   devices on a Unifying receiver through their battery registers. Only queries are
//!   sent.
//! - **Game controllers** Windows reads a battery for (Xbox controllers on the wireless
//!   adapter, Bluetooth or cable): Windows.Gaming.Input.
//! - **PlayStation** (DualSense, DualSense Edge, DualShock 4): the battery byte of the
//!   input reports the controller sends anyway. Over Bluetooth only once something
//!   (Steam, a game) switched it to full reports: asking for that ourselves would take
//!   the controller away from games that read the simple ones.
//! - **Any other USB device** whose HID description declares a battery strength (Generic
//!   Device Controls page): wireless dongles of several brands.
//!
//! Peripherals come and go, so the last reading of each one is kept (and saved), and
//! shown as disconnected while it is away.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Devices::Properties::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::*;
use windows::Win32::System::Threading::CreateEventW;
use windows::core::{GUID, PCWSTR};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Headset,
    Mouse,
    Keyboard,
    Gamepad,
    Phone,
    #[default]
    Other,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    #[default]
    Bluetooth,
    Logitech,
    /// Windows.Gaming.Input: Xbox controllers and others Windows reads a battery for.
    Xbox,
    PlayStation,
    /// A USB device's own HID battery report.
    Usb,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Peripheral {
    /// Stable across runs: "bt:<container>" or "lg:<unit id>".
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub via: Via,
    /// 0-100.
    pub percent: u8,
    pub charging: bool,
    pub connected: bool,
    /// Unix time (ms) of the last reading taken while connected.
    pub seen: u64,
    /// Picked in Settings to show on the taskbar (set by the sampler, not saved).
    #[serde(skip_deserializing)]
    pub shown: bool,
}

impl Peripheral {
    /// The few letters under the strip item: the brand or first word, plus a short last
    /// word that tells models apart ("MX Master 3S" -> "MX 3S", "QCY MeloBuds Pro" -> "QCY").
    pub fn short_name(&self) -> String {
        let words: Vec<&str> = self.name.split_whitespace().collect();
        let mut s = words.first().copied().unwrap_or("").to_string();
        if let (true, Some(last)) = (s.chars().count() <= 2 && words.len() > 1, words.last()) {
            if last.chars().count() <= 4 {
                s = format!("{s} {last}");
            }
        }
        s.to_uppercase().chars().take(9).collect()
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// What a product name says the device is, for sources that do not say.
fn kind_from_name(name: &str) -> Kind {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["headset", "headphone", "buds", "earphone", "arctis", "cloud", "audio"]) {
        Kind::Headset
    } else if has(&["keyboard", "keys"]) {
        Kind::Keyboard
    } else if has(&["mouse", "mice", "trackball"]) {
        Kind::Mouse
    } else if has(&["controller", "gamepad", "joystick"]) {
        Kind::Gamepad
    } else {
        Kind::Other
    }
}

/// A short, stable id from a long one (FNV-1a).
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Percent of a lithium battery from its voltage, on a typical discharge curve.
fn percent_from_mv(mv: u16) -> u8 {
    const CURVE: [(u16, u8); 11] = [(4150, 100), (4050, 90), (3980, 80), (3920, 70), (3860, 60), (3810, 50), (3780, 40), (3750, 30), (3710, 20), (3670, 10), (3500, 0)];
    if mv >= CURVE[0].0 {
        return 100;
    }
    for w in CURVE.windows(2) {
        let ((hv, hp), (lv, lp)) = (w[0], w[1]);
        if mv >= lv {
            return lp + ((mv - lv) as u32 * (hp - lp) as u32 / (hv - lv) as u32) as u8;
        }
    }
    0
}

// ---- Bluetooth --------------------------------------------------------------------------

/// The battery level Windows read from a Bluetooth device, a byte 0-100.
const BT_BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };
/// Whether the device is connected right now (a boolean).
const CONNECTED: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29), pid: 15 };
/// The Bluetooth class of device (major/minor class), for classic devices: pid 10 on the
/// device's own node, 4 on its profile nodes (hands-free, audio).
const BT_CLASS: [DEVPROPKEY; 2] = [
    DEVPROPKEY { fmtid: GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a), pid: 10 },
    DEVPROPKEY { fmtid: GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a), pid: 4 },
];

/// One property of a device node, raw.
fn property(set: HDEVINFO, dev: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    let mut ty = DEVPROPTYPE::default();
    let mut size = 0u32;
    unsafe {
        let _ = SetupDiGetDevicePropertyW(set, dev, key, &mut ty, None, Some(&mut size), 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        SetupDiGetDevicePropertyW(set, dev, key, &mut ty, Some(&mut buf), None, 0).ok()?;
        Some((ty, buf))
    }
}

fn string_property(set: HDEVINFO, dev: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let (ty, buf) = property(set, dev, key)?;
    if ty != DEVPROP_TYPE_STRING {
        return None;
    }
    let wide: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&wide))
}

fn guid_property(set: HDEVINFO, dev: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let (ty, buf) = property(set, dev, key)?;
    (ty == DEVPROP_TYPE_GUID && buf.len() == 16).then(|| {
        let g = GUID::from_u128(u128::from_be_bytes({
            // GUID's in-memory layout: three little-endian fields, then 8 bytes.
            let mut b = [0u8; 16];
            b[..4].copy_from_slice(&[buf[3], buf[2], buf[1], buf[0]]);
            b[4..6].copy_from_slice(&[buf[5], buf[4]]);
            b[6..8].copy_from_slice(&[buf[7], buf[6]]);
            b[8..].copy_from_slice(&buf[8..16]);
            b
        }));
        format!("{g:?}").to_lowercase()
    })
}

/// Strips the profile Windows appends to the node's name ("QCY MeloBuds Pro Hands-Free AG").
fn device_name(node: &str) -> String {
    let mut name = node.trim().to_string();
    for suffix in [" Hands-Free AG", " Hands-Free", " Avrcp Transport", " Stereo", " LE"] {
        if let Some(n) = name.strip_suffix(suffix) {
            name = n.trim().to_string();
        }
    }
    name
}

/// What the Bluetooth class of device says it is.
fn kind_from_class(cod: u32) -> Kind {
    let (major, minor) = ((cod >> 8) & 0x1f, (cod >> 2) & 0x3f);
    match major {
        0x02 => Kind::Phone,
        0x04 => Kind::Headset,
        0x05 => match minor & 0x30 {
            0x10 => Kind::Keyboard,
            0x20 => Kind::Mouse,
            _ if minor & 0x0f == 0x02 => Kind::Gamepad,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    }
}

/// Every Bluetooth device Windows has a battery level for, one per physical device
/// (a headset is several device nodes in one container).
pub fn bluetooth() -> Vec<Peripheral> {
    let mut found: HashMap<String, Peripheral> = HashMap::new();
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT) else { return Vec::new() };
        let mut i = 0;
        loop {
            let mut dev = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut dev).is_err() {
                break;
            }
            i += 1;
            let mut id = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(set, &dev, Some(&mut id), None).is_err() {
                continue;
            }
            let id = String::from_utf16_lossy(&id[..id.iter().position(|&c| c == 0).unwrap_or(0)]).to_uppercase();
            if !["BTHENUM\\", "BTHLE\\", "BTHLEDEVICE\\"].iter().any(|p| id.starts_with(p)) {
                continue;
            }
            let Some((ty, level)) = property(set, &dev, &BT_BATTERY) else { continue };
            if ty != DEVPROP_TYPE_BYTE || level.is_empty() || level[0] > 100 {
                continue;
            }
            let container = guid_property(set, &dev, &DEVPKEY_Device_ContainerId).unwrap_or_else(|| id.clone());
            let connected = property(set, &dev, &CONNECTED).is_some_and(|(t, v)| t == DEVPROP_TYPE_BOOLEAN && v.first().is_some_and(|b| *b != 0));
            let name = string_property(set, &dev, &DEVPKEY_Device_FriendlyName).or_else(|| string_property(set, &dev, &DEVPKEY_Device_DeviceDesc)).unwrap_or_default();
            let kind = BT_CLASS
                .iter()
                .find_map(|k| property(set, &dev, k))
                .filter(|(t, v)| *t == DEVPROP_TYPE_UINT32 && v.len() == 4)
                .map(|(_, v)| kind_from_class(u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
                .unwrap_or_default();
            let p = Peripheral {
                id: format!("bt:{container}"),
                name: device_name(&name),
                kind,
                via: Via::Bluetooth,
                percent: level[0],
                charging: false,
                connected,
                seen: 0,
                shown: false,
            };
            // Several nodes of one device: any connected one makes it connected.
            match found.get_mut(&p.id) {
                Some(e) => {
                    e.connected |= p.connected;
                    if e.kind == Kind::Other {
                        e.kind = p.kind;
                    }
                }
                None => {
                    found.insert(p.id.clone(), p);
                }
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    found.into_values().collect()
}

// ---- HID collections ----------------------------------------------------------------------

const LOGITECH: u16 = 0x046d;
const SONY: u16 = 0x054c;
/// Generic Device Controls page, Battery Strength usage.
const BATTERY_PAGE: u16 = 0x06;
const BATTERY_STRENGTH: u16 = 0x20;

/// Where a USB device's HID description puts its battery strength.
#[derive(Clone, Copy, Debug)]
struct BatteryValue {
    feature: bool,
    report: u8,
    link: u16,
    min: i32,
    max: i32,
    bits: u16,
}

/// One HID top-level collection, as Windows lists it.
struct Collection {
    /// NUL-terminated device path.
    path: Vec<u16>,
    vid: u16,
    pid: u16,
    page: u16,
    usage: u16,
    in_len: usize,
    feature_len: usize,
    /// Instance id of the parent node: the collections of one device share it.
    parent: String,
    bluetooth: bool,
    battery: Option<BatteryValue>,
}

/// The battery strength value of a collection, if its description has one.
unsafe fn battery_value(pp: PHIDP_PREPARSED_DATA, caps: &HIDP_CAPS) -> Option<BatteryValue> {
    for (ty, count, feature) in [(HidP_Feature, caps.NumberFeatureValueCaps, true), (HidP_Input, caps.NumberInputValueCaps, false)] {
        if count == 0 {
            continue;
        }
        let mut n = count;
        let mut vc = vec![HIDP_VALUE_CAPS::default(); n as usize];
        if unsafe { HidP_GetValueCaps(ty, vc.as_mut_ptr(), &mut n, pp) } != HIDP_STATUS_SUCCESS {
            continue;
        }
        for v in &vc[..n as usize] {
            let hit = v.UsagePage == BATTERY_PAGE
                && unsafe { if v.IsRange { (v.Anonymous.Range.UsageMin..=v.Anonymous.Range.UsageMax).contains(&BATTERY_STRENGTH) } else { v.Anonymous.NotRange.Usage == BATTERY_STRENGTH } };
            if hit {
                return Some(BatteryValue { feature, report: v.ReportID, link: v.LinkCollection, min: v.LogicalMin, max: v.LogicalMax, bits: v.BitSize });
            }
        }
    }
    None
}

/// Every HID collection present, with what is needed to pick the ones worth opening.
fn hid_collections() -> Vec<Collection> {
    let mut out = Vec::new();
    unsafe {
        let guid = HidD_GetHidGuid();
        let Ok(set) = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) else { return out };
        let mut i = 0;
        loop {
            let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut ifd).is_err() {
                break;
            }
            i += 1;
            let mut size = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &ifd, None, 0, Some(&mut size), None);
            if size == 0 {
                continue;
            }
            let mut raw = vec![0u64; (size as usize).div_ceil(8)];
            let detail = raw.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut dev = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, Some(detail), size, None, Some(&mut dev)).is_err() {
                continue;
            }
            let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let len = (0..).take_while(|&k| *p.add(k) != 0).count();
            let path: Vec<u16> = std::slice::from_raw_parts(p, len).iter().copied().chain([0]).collect();
            let lower = String::from_utf16_lossy(&path).to_lowercase();
            let mut parent = 0u32;
            let mut pid_buf = [0u16; 512];
            let parent_id = if CM_Get_Parent(&mut parent, dev.DevInst, 0) == CR_SUCCESS && CM_Get_Device_IDW(parent, &mut pid_buf, 0) == CR_SUCCESS {
                String::from_utf16_lossy(&pid_buf[..pid_buf.iter().position(|&c| c == 0).unwrap_or(0)])
            } else {
                String::new()
            };
            // Open without access: enough for the attributes and the description.
            let Ok(h) = CreateFileW(PCWSTR(path.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) else { continue };
            let mut attrs = HIDD_ATTRIBUTES { Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
            let mut pp = PHIDP_PREPARSED_DATA::default();
            let mut caps = HIDP_CAPS::default();
            let mut battery = None;
            let ok = HidD_GetAttributes(h, &mut attrs) && HidD_GetPreparsedData(h, &mut pp) && {
                let got = HidP_GetCaps(pp, &mut caps).is_ok();
                if got {
                    battery = battery_value(pp, &caps);
                }
                let _ = HidD_FreePreparsedData(pp);
                got
            };
            let _ = CloseHandle(h);
            if !ok {
                continue;
            }
            out.push(Collection {
                path,
                vid: attrs.VendorID,
                pid: attrs.ProductID,
                page: caps.UsagePage,
                usage: caps.Usage,
                in_len: caps.InputReportByteLength as usize,
                feature_len: caps.FeatureReportByteLength as usize,
                parent: parent_id,
                // Classic Bluetooth HID and HID over GATT services.
                bluetooth: lower.contains("00001124-0000-1000-8000-00805f9b34fb") || lower.contains("00001812-0000-1000-8000-00805f9b34fb"),
                battery,
            });
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// A HID collection opened for reading (and writing when allowed), with overlapped I/O
/// so that a silent device cannot hang the poller.
struct Hid {
    handle: HANDLE,
    event: HANDLE,
    in_len: usize,
}

impl Drop for Hid {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Hid {
    fn open(c: &Collection) -> Option<Hid> {
        unsafe {
            let open = |access: u32| CreateFileW(PCWSTR(c.path.as_ptr()), access, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, None);
            let handle = open((GENERIC_READ | GENERIC_WRITE).0).or_else(|_| open(GENERIC_READ.0)).ok()?;
            let Ok(event) = CreateEventW(None, true, false, PCWSTR::null()) else {
                let _ = CloseHandle(handle);
                return None;
            };
            Some(Hid { handle, event, in_len: c.in_len.max(LONG_LEN) })
        }
    }

    /// Runs one overlapped transfer, waiting at most `ms`.
    fn transfer(&self, write: bool, buf: &mut [u8], ms: u32) -> Option<usize> {
        unsafe {
            let mut ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
            let started = if write { WriteFile(self.handle, Some(buf), None, Some(&mut ov)) } else { ReadFile(self.handle, Some(buf), None, Some(&mut ov)) };
            if let Err(e) = started {
                if e.code() != ERROR_IO_PENDING.to_hresult() {
                    return None;
                }
            }
            let mut n = 0u32;
            if GetOverlappedResultEx(self.handle, &ov, &mut n, ms, false).is_err() {
                let _ = CancelIoEx(self.handle, Some(&ov));
                let _ = GetOverlappedResult(self.handle, &ov, &mut n, true);
                return None;
            }
            Some(n as usize)
        }
    }

    /// Reads reports until `pick` accepts one or `ms` runs out.
    fn read_until<T>(&self, ms: u64, mut pick: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        let mut buf = vec![0u8; self.in_len];
        while std::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as u32;
            let n = self.transfer(false, &mut buf, left.max(1))?;
            if let Some(t) = pick(&buf[..n]) {
                return Some(t);
            }
        }
        None
    }
}

// ---- Logitech HID++ ----------------------------------------------------------------------

/// Short HID++ report: id, device index, sub id, address, 3 bytes.
const SHORT: u8 = 0x10;
const SHORT_LEN: usize = 7;
/// Long HID++ report: id, device index, feature index, function/software id, 16 bytes.
const LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
/// Our software id: replies carry it back; notifications carry 0.
const SW_ID: u8 = 0x0a;

const DEVICE_INFO: u16 = 0x0003;
const DEVICE_NAME: u16 = 0x0005;
const BATTERY_STATUS: u16 = 0x1000;
const BATTERY_VOLTAGE: u16 = 0x1001;
const UNIFIED_BATTERY: u16 = 0x1004;
/// Battery voltage as an ADC reading: the G headsets.
const ADC_MEASUREMENT: u16 = 0x1f20;

/// HID++ 1.0 sub ids and registers.
const GET_REGISTER: u8 = 0x81;
const GET_LONG_REGISTER: u8 = 0x83;
const ERROR_10: u8 = 0x8f;
const REG_BATTERY_STATUS: u8 = 0x07;
const REG_BATTERY_MILEAGE: u8 = 0x0d;
const REG_PAIRING: u8 = 0xb5;
const RECEIVER: u8 = 0xff;

impl Hid {
    /// Sends a HID++ 2.0 request and waits for its reply (`None`: timeout or error).
    fn request(&self, device: u8, feature: u8, function: u8, params: &[u8]) -> Option<[u8; 16]> {
        let mut out = [0u8; LONG_LEN];
        out[..4].copy_from_slice(&[LONG, device, feature, (function << 4) | SW_ID]);
        out[4..4 + params.len().min(16)].copy_from_slice(&params[..params.len().min(16)]);
        self.transfer(true, &mut out, 300)?;
        // A sleeping device takes a radio round trip or two to wake: over half a second.
        // An error ends the wait with nothing.
        let reply = self.read_until(1500, |buf| {
            if buf.len() < 7 || buf[1] != device {
                return None;
            }
            // HID++ 1.0 error (short report, sub id 0x8f), e.g. "device offline".
            if buf[0] == SHORT && buf[2] == ERROR_10 && buf[3] == feature {
                return Some(None);
            }
            if buf[0] != LONG || buf.len() < LONG_LEN {
                return None;
            }
            // HID++ 2.0 error: feature index 0xff, then the request's feature and function.
            if buf[2] == 0xff && buf[3] == feature && buf[4] == (function << 4) | SW_ID {
                return Some(None);
            }
            if buf[2] == feature && buf[3] == (function << 4) | SW_ID {
                let mut r = [0u8; 16];
                r.copy_from_slice(&buf[4..20]);
                return Some(Some(r));
            }
            None
        });
        reply.flatten()
    }

    /// Index of a feature on the device, through the root feature (0 = not supported).
    fn feature(&self, device: u8, id: u16) -> Option<u8> {
        let r = self.request(device, 0, 0, &id.to_be_bytes())?;
        (r[0] != 0).then_some(r[0])
    }
}

/// HID++ device type (feature 0x0005, function 2) as a kind.
fn kind_from_hidpp(t: u8) -> Kind {
    match t {
        0 | 2 => Kind::Keyboard,
        3..=5 => Kind::Mouse,
        8 => Kind::Headset,
        11 | 12 => Kind::Gamepad,
        _ => Kind::Other,
    }
}

/// Unified battery get_status: state of charge %, level flags, charging status.
fn unified_battery(r: &[u8]) -> (u8, bool) {
    let percent = if r[0] > 0 {
        r[0]
    } else {
        // Devices that only report levels: full, good, low, critical.
        match r[1] {
            l if l & 8 != 0 => 100,
            l if l & 4 != 0 => 60,
            l if l & 2 != 0 => 20,
            _ => 5,
        }
    };
    (percent.min(100), matches!(r[2], 1 | 2))
}

/// Battery voltage get_battery_info: millivolts, then flags (bit 7: external power,
/// bits 0-2: charging, full, not charging, errors).
fn battery_voltage(r: &[u8]) -> Option<(u8, bool)> {
    let mv = u16::from_be_bytes([r[0], r[1]]);
    (mv > 0).then(|| (percent_from_mv(mv), r[2] & 0x80 != 0 && r[2] & 0x07 <= 1))
}

/// ADC measurement: millivolts, then flags (bit 1: charging, bit 2: charged). A headset
/// that is off reads zero.
fn adc_measurement(r: &[u8]) -> Option<(u8, bool)> {
    let mv = u16::from_be_bytes([r[0], r[1]]);
    (mv > 0).then(|| (percent_from_mv(mv), r[2] & 0x06 != 0))
}

/// Reads one HID++ 2.0 device: name, type, id and battery. `None` if it does not answer
/// (offline, or not HID++ 2.0) or reports no battery.
fn logitech_device(hid: &Hid, index: u8) -> Option<Peripheral> {
    let (percent, charging) = if let Some(f) = hid.feature(index, UNIFIED_BATTERY) {
        unified_battery(&hid.request(index, f, 1, &[])?)
    } else if let Some(f) = hid.feature(index, BATTERY_STATUS) {
        // get_battery_level_status: level %, next level, status.
        let r = hid.request(index, f, 0, &[])?;
        (r[0].min(100), matches!(r[2], 1 | 2 | 4))
    } else if let Some(f) = hid.feature(index, BATTERY_VOLTAGE) {
        battery_voltage(&hid.request(index, f, 0, &[])?)?
    } else if let Some(f) = hid.feature(index, ADC_MEASUREMENT) {
        adc_measurement(&hid.request(index, f, 0, &[])?)?
    } else {
        return None;
    };

    let mut name = String::new();
    let mut kind = Kind::Other;
    if let Some(f) = hid.feature(index, DEVICE_NAME) {
        let len = hid.request(index, f, 0, &[]).map_or(0, |r| r[0] as usize);
        let mut bytes = Vec::new();
        while bytes.len() < len.min(64) {
            let Some(r) = hid.request(index, f, 1, &[bytes.len() as u8]) else { break };
            let chunk: Vec<u8> = r.iter().copied().take(len - bytes.len()).take_while(|&b| b != 0).collect();
            if chunk.is_empty() {
                break;
            }
            bytes.extend(chunk);
        }
        name = String::from_utf8_lossy(&bytes).trim().to_string();
        if let Some(r) = hid.request(index, f, 2, &[]) {
            kind = kind_from_hidpp(r[0]);
        }
    }
    if kind == Kind::Other {
        kind = kind_from_name(&name);
    }
    // The unit id identifies the device wherever it is plugged in.
    let unit = hid.feature(index, DEVICE_INFO).and_then(|f| hid.request(index, f, 0, &[])).map(|r| u32::from_be_bytes([r[1], r[2], r[3], r[4]])).filter(|&u| u != 0);
    let id = match unit {
        Some(u) => format!("lg:{u:08x}"),
        None => format!("lg:{}", name.to_lowercase()),
    };
    Some(Peripheral { id, name: if name.is_empty() { "Logitech".into() } else { name }, kind, via: Via::Logitech, percent, charging, connected: true, seen: 0, shown: false })
}

/// Which device indexes answer as HID++ 2.0. A receiver's devices sit at 1-6, a device
/// on its own answers at 0xff. Offline ones send their error on another collection (the
/// short reports), so nothing comes back here: ask all at once and collect the replies
/// rather than waiting for each in turn.
fn online(hid: &Hid) -> Vec<u8> {
    const INDEXES: [u8; 7] = [0xff, 1, 2, 3, 4, 5, 6];
    for index in INDEXES {
        // Root feature, function 1: protocol version; the third byte comes back as is.
        let mut out = [0u8; LONG_LEN];
        out[..7].copy_from_slice(&[LONG, index, 0, (1 << 4) | SW_ID, 0, 0, 0x5a]);
        let _ = hid.transfer(true, &mut out, 300);
    }
    let mut found = Vec::new();
    let _ = hid.read_until(1500, |buf| {
        // A reply: protocol major 2 or more, our marker echoed.
        if buf.len() >= 7 && buf[0] == LONG && buf[2] == 0 && buf[3] == (1 << 4) | SW_ID && buf[4] >= 2 && buf[6] == 0x5a && !found.contains(&buf[1]) {
            found.push(buf[1]);
        }
        (found.len() == INDEXES.len()).then_some(())
    });
    found
}

/// HID++ 1.0 register reads to several devices at once: the reply (3 bytes) of each one
/// that answered, `None` for each that sent an error.
fn read_registers(short: &Hid, indexes: &[u8], register: u8) -> HashMap<u8, Option<[u8; 3]>> {
    for &index in indexes {
        let mut out = [SHORT, index, GET_REGISTER, register, 0, 0, 0];
        let _ = short.transfer(true, &mut out, 300);
    }
    let mut got = HashMap::new();
    let _ = short.read_until(1000, |b| {
        if b.len() >= SHORT_LEN && b[0] == SHORT && indexes.contains(&b[1]) {
            if b[2] == GET_REGISTER && b[3] == register {
                got.insert(b[1], Some([b[4], b[5], b[6]]));
            } else if b[2] == ERROR_10 && b[3] == GET_REGISTER && b[4] == register {
                got.insert(b[1], None);
            }
        }
        (got.len() == indexes.len()).then_some(())
    });
    got
}

/// Battery mileage register: level %, maximum mileage, then the charging state in the
/// top two bits of the last byte (discharging, charging, charged, error).
fn mileage(r: [u8; 3]) -> Option<(u8, bool)> {
    (r[0] <= 100).then(|| (r[0], matches!(r[2] >> 6, 1 | 2)))
}

/// Battery status register: a level (7 full, 5 good, 3 low, 1 critical; 0 unknown) and a
/// charging state (0x21-0x24 while on its charger).
fn battery_status(r: [u8; 3]) -> Option<(u8, bool)> {
    let percent = match r[0] {
        7 => 100,
        5 | 6 => 50,
        3 | 4 => 20,
        1 | 2 => 5,
        _ => return None,
    };
    Some((percent, (0x21..=0x24).contains(&r[1])))
}

/// A long register of the receiver (pairing information), asked on the short collection;
/// the reply comes back as a long report.
fn pairing_info(short: &Hid, long: &Hid, sub: u8) -> Option<[u8; 16]> {
    let mut out = [SHORT, RECEIVER, GET_LONG_REGISTER, REG_PAIRING, sub, 0, 0];
    short.transfer(true, &mut out, 300)?;
    long.read_until(500, |b| {
        (b.len() >= LONG_LEN && b[0] == LONG && b[1] == RECEIVER && b[2] == GET_LONG_REGISTER && b[3] == REG_PAIRING && b[4] == sub).then(|| {
            let mut r = [0u8; 16];
            r.copy_from_slice(&b[4..20]);
            r
        })
    })
}

/// HID++ 1.0 devices (older Unifying ones) at the indexes that did not answer as 2.0.
fn hidpp10_devices(short: &Hid, long: &Hid, skip: &[u8]) -> Vec<Peripheral> {
    let indexes: Vec<u8> = (1..=6).filter(|i| !skip.contains(i)).collect();
    if indexes.is_empty() {
        return Vec::new();
    }
    let mut levels: HashMap<u8, (u8, bool)> = read_registers(short, &indexes, REG_BATTERY_MILEAGE).into_iter().filter_map(|(i, r)| Some((i, mileage(r?)?))).collect();
    let rest: Vec<u8> = indexes.iter().copied().filter(|i| !levels.contains_key(i)).collect();
    if !rest.is_empty() {
        levels.extend(read_registers(short, &rest, REG_BATTERY_STATUS).into_iter().filter_map(|(i, r)| Some((i, battery_status(r?)?))));
    }
    let mut out = Vec::new();
    for (index, (percent, charging)) in levels {
        let n = index - 1;
        // Name: length, then the characters.
        let name = pairing_info(short, long, 0x40 + n).map(|r| String::from_utf8_lossy(&r[2..2 + (r[1] as usize).min(14)]).trim().to_string()).unwrap_or_default();
        // Extended pairing information: the serial number.
        let serial = pairing_info(short, long, 0x30 + n).map(|r| u32::from_be_bytes([r[1], r[2], r[3], r[4]])).filter(|&s| s != 0);
        // Pairing information: the device type (1 keyboard, 2 mouse, 8 trackball, 9 touchpad).
        let kind = match pairing_info(short, long, 0x20 + n).map(|r| r[7]) {
            Some(1) => Kind::Keyboard,
            Some(2 | 8 | 9) => Kind::Mouse,
            _ => kind_from_name(&name),
        };
        let id = match serial {
            Some(s) => format!("lg:{s:08x}"),
            None => format!("lg:{}", name.to_lowercase()),
        };
        out.push(Peripheral { id, name: if name.is_empty() { "Logitech".into() } else { name }, kind, via: Via::Logitech, percent, charging, connected: true, seen: 0, shown: false });
    }
    out
}

/// Every connected Logitech HID++ device with a battery.
fn logitech(cols: &[Collection]) -> Vec<Peripheral> {
    let mut out: Vec<Peripheral> = Vec::new();
    // FF00:0002 on receivers and cabled devices, FF43:0202 on Bluetooth and G headsets.
    for c in cols.iter().filter(|c| c.vid == LOGITECH && matches!((c.page, c.usage), (0xff00, 0x0002) | (0xff43, 0x0202))) {
        let Some(hid) = Hid::open(c) else { continue };
        let answered = online(&hid);
        let mut found: Vec<Peripheral> = answered.iter().filter_map(|&i| logitech_device(&hid, i)).collect();
        // A receiver's short collection (FF00:0001) reaches its HID++ 1.0 devices.
        let short = cols.iter().find(|s| s.vid == LOGITECH && (s.page, s.usage) == (0xff00, 0x0001) && !s.parent.is_empty() && s.parent == c.parent);
        if let Some(sh) = short.filter(|_| c.page == 0xff00).and_then(Hid::open) {
            found.extend(hidpp10_devices(&sh, &hid, &answered));
        }
        for p in found {
            if !out.iter().any(|o| o.id == p.id) {
                out.push(p);
            }
        }
    }
    out
}

// ---- game controllers ------------------------------------------------------------------------

thread_local! {
    static WINRT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Controllers Windows.Gaming.Input has a battery report for: Xbox controllers on the
/// wireless adapter, Bluetooth or (with batteries in) a cable.
fn controllers() -> Vec<Peripheral> {
    use windows::Gaming::Input::RawGameController;
    use windows::System::Power::BatteryStatus;
    let first = !WINRT.with(|w| w.replace(true));
    if first {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
    }
    let list = || RawGameController::RawGameControllers().ok();
    let mut all = list();
    // The first call starts the enumeration; controllers turn up a moment later.
    if first && all.as_ref().is_none_or(|l| l.Size().unwrap_or(0) == 0) {
        std::thread::sleep(std::time::Duration::from_millis(800));
        all = list();
    }
    let Some(all) = all else { return Vec::new() };
    let mut out = Vec::new();
    for i in 0..all.Size().unwrap_or(0) {
        let Ok(c) = all.GetAt(i) else { continue };
        // PlayStation controllers are read from their own reports.
        if c.HardwareVendorId().is_ok_and(|v| v == SONY) {
            continue;
        }
        let Ok(report) = c.TryGetBatteryReport() else { continue };
        let status = report.Status().unwrap_or(BatteryStatus::NotPresent);
        if status == BatteryStatus::NotPresent {
            continue;
        }
        let full = report.FullChargeCapacityInMilliwattHours().and_then(|v| v.Value()).unwrap_or(0);
        let left = report.RemainingCapacityInMilliwattHours().and_then(|v| v.Value()).unwrap_or(0);
        if full <= 0 {
            continue;
        }
        let name = c.DisplayName().map(|s| s.to_string()).unwrap_or_default();
        let key = c.NonRoamableId().map(|s| s.to_string()).unwrap_or_else(|_| name.clone());
        out.push(Peripheral {
            id: format!("gi:{:016x}", hash(&key)),
            name: if name.is_empty() { "Controller".into() } else { name },
            kind: Kind::Gamepad,
            via: Via::Xbox,
            percent: (left.clamp(0, full) as i64 * 100 / full as i64) as u8,
            charging: status == BatteryStatus::Charging,
            connected: true,
            seen: 0,
            shown: false,
        });
    }
    out
}

const DUALSHOCK4: [u16; 2] = [0x05c4, 0x09cc];
const DUALSENSE: u16 = 0x0ce6;
const DUALSENSE_EDGE: u16 = 0x0df2;

/// The battery in a PlayStation controller's input report: level 0-10 in the low nibble
/// of its status byte, plus a charging state. `None` for reports without it (the simple
/// Bluetooth ones).
fn playstation_battery(dualsense: bool, bluetooth: bool, r: &[u8]) -> Option<(u8, bool)> {
    let at = match (dualsense, bluetooth, *r.first()?) {
        (true, false, 0x01) => 53,
        (true, true, 0x31) => 54,
        (false, false, 0x01) => 30,
        (false, true, 0x11) => 32,
        _ => return None,
    };
    let s = *r.get(at)?;
    let level = s & 0x0f;
    let percent = (level * 10 + 5).min(100);
    if dualsense {
        // High nibble: 0 discharging, 1 charging, 2 charged; anything else is an error.
        match s >> 4 {
            0 if level <= 10 => Some((percent, false)),
            1 if level <= 10 => Some((percent, true)),
            2 => Some((100, true)),
            _ => None,
        }
    } else if s & 0x10 != 0 {
        // On the cable: levels up to 10 while charging, 10 or 11 once charged.
        match level {
            0..=9 => Some((percent, true)),
            10 | 11 => Some((100, true)),
            _ => None,
        }
    } else {
        (level <= 10).then_some((percent, false))
    }
}

/// DualSense and DualShock 4 controllers, from the reports they are already sending.
fn playstation(cols: &[Collection]) -> Vec<Peripheral> {
    let mut out: Vec<Peripheral> = Vec::new();
    for c in cols.iter().filter(|c| c.vid == SONY && (c.page, c.usage) == (0x01, 0x05)) {
        let (dualsense, name) = match c.pid {
            DUALSENSE => (true, "DualSense"),
            DUALSENSE_EDGE => (true, "DualSense Edge"),
            p if DUALSHOCK4.contains(&p) => (false, "DualShock 4"),
            _ => continue,
        };
        let Some(hid) = Hid::open(c) else { continue };
        // Reports come every few milliseconds; a quiet controller is asleep or away.
        let Some((percent, charging)) = hid.read_until(500, |r| playstation_battery(dualsense, c.bluetooth, r)) else { continue };
        let mut serial = [0u16; 64];
        let serial = unsafe { HidD_GetSerialNumberString(hid.handle, serial.as_mut_ptr().cast(), 128) }
            .then(|| String::from_utf16_lossy(&serial[..serial.iter().position(|&ch| ch == 0).unwrap_or(0)]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| c.parent.clone());
        let p = Peripheral { id: format!("ps:{:016x}", hash(&serial)), name: name.into(), kind: Kind::Gamepad, via: Via::PlayStation, percent, charging, connected: true, seen: 0, shown: false };
        if !out.iter().any(|o| o.id == p.id) {
            out.push(p);
        }
    }
    out
}

// ---- any USB device with a HID battery ---------------------------------------------------------

/// A raw battery strength as a percentage of its logical range.
fn scale(raw: u32, b: &BatteryValue) -> Option<u8> {
    let (min, mut max) = (b.min as i64, b.max as i64);
    // A maximum that reads negative is an unsigned field.
    if max <= min {
        max = (1i64 << b.bits.min(32)) - 1;
    }
    let v = raw as i64;
    (max > min && (min..=max).contains(&v)).then(|| ((v - min) * 100 / (max - min)) as u8)
}

/// Asks a device for its battery report and its name. These requests wait on the device
/// with no time limit of their own, and one dongle that never answers would stop every
/// source (they share the poller's thread): they run on a thread of their own, cancelled
/// after a moment. `None` when the device stayed silent.
fn usb_read(h: HANDLE, b: BatteryValue, len: usize) -> Option<(Option<u32>, String)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let raw_handle = h.0 as isize;
    let reader = std::thread::Builder::new().name("usb battery".into()).spawn(move || unsafe {
        let h = HANDLE(raw_handle as _);
        let mut pp = PHIDP_PREPARSED_DATA::default();
        let mut raw = None;
        if HidD_GetPreparsedData(h, &mut pp) {
            let mut buf = vec![0u8; len];
            buf[0] = b.report;
            let read = if b.feature { HidD_GetFeature(h, buf.as_mut_ptr().cast(), buf.len() as u32) } else { HidD_GetInputReport(h, buf.as_mut_ptr().cast(), buf.len() as u32) };
            let mut v = 0u32;
            if read && HidP_GetUsageValue(if b.feature { HidP_Feature } else { HidP_Input }, BATTERY_PAGE, Some(b.link), BATTERY_STRENGTH, &mut v, pp, &buf) == HIDP_STATUS_SUCCESS {
                raw = Some(v);
            }
            let _ = HidD_FreePreparsedData(pp);
        }
        let mut name = [0u16; 128];
        let name = HidD_GetProductString(h, name.as_mut_ptr().cast(), 256).then(|| String::from_utf16_lossy(&name[..name.iter().position(|&ch| ch == 0).unwrap_or(0)]).trim().to_string()).unwrap_or_default();
        let _ = tx.send((raw, name));
    });
    if reader.is_err() {
        unsafe {
            let _ = CloseHandle(h);
        }
        return None;
    }
    let wait = |ms| rx.recv_timeout(std::time::Duration::from_millis(ms)).ok();
    let answer = wait(1500).or_else(|| {
        unsafe {
            let _ = CancelIoEx(h, None);
        }
        // The cancelled request returns and the thread ends; what it read is not kept.
        wait(1000).and(None)
    });
    // Still inside the request after the cancel: the handle stays with that thread.
    if answer.is_some() || rx.try_recv() != Err(std::sync::mpsc::TryRecvError::Empty) {
        unsafe {
            let _ = CloseHandle(h);
        }
    }
    answer
}

/// USB devices whose own description has a battery strength: dongles of several brands.
/// Bluetooth ones are left out (Windows already reads those), and so are the brands
/// read above.
fn usb_batteries(cols: &[Collection]) -> Vec<Peripheral> {
    let mut out: Vec<Peripheral> = Vec::new();
    for c in cols.iter().filter(|c| c.battery.is_some() && !c.bluetooth && c.vid != LOGITECH && c.vid != SONY) {
        let b = c.battery.unwrap();
        let id = format!("hid:{:04x}{:04x}", c.vid, c.pid);
        if out.iter().any(|o| o.id == id) {
            continue;
        }
        // Feature reports work on a handle without access; keyboards and mice allow no more.
        let open = |access: u32| unsafe { CreateFileW(PCWSTR(c.path.as_ptr()), access, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) };
        let Ok(h) = open((GENERIC_READ | GENERIC_WRITE).0).or_else(|_| open(0)) else { continue };
        let len = if b.feature { c.feature_len } else { c.in_len }.max(2);
        let Some((raw, name)) = usb_read(h, b, len) else { continue };
        {
            // Zero is what wired devices that only declare the field send.
            let Some(percent) = raw.and_then(|v| scale(v, &b)).filter(|&p| p > 0) else { continue };
            let kind = match (c.page, c.usage) {
                (0x01, 0x02) => Kind::Mouse,
                (0x01, 0x06) => Kind::Keyboard,
                (0x01, 0x04 | 0x05) => Kind::Gamepad,
                (0x0b, _) => Kind::Headset,
                _ => kind_from_name(&name),
            };
            out.push(Peripheral { id, name: if name.is_empty() { "USB".into() } else { name }, kind, via: Via::Usb, percent: percent.min(100), charging: false, connected: true, seen: 0, shown: false });
        }
    }
    out
}

// ---- the list ---------------------------------------------------------------------------

/// Every peripheral seen so far, with the latest reading: polled now and then on its
/// own thread, saved so disconnected devices keep their last level.
pub struct Peripherals {
    list: Vec<Peripheral>,
    file: Option<PathBuf>,
    /// Ids dropped for a better entry of the same device (old, new), until taken.
    moved: Vec<(String, String)>,
}

impl Peripherals {
    pub fn load(file: Option<PathBuf>) -> Self {
        let mut list: Vec<Peripheral> = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        for p in &mut list {
            p.connected = false;
        }
        Self { list, file, moved: Vec::new() }
    }

    /// The ids replaced since the last call, so a device picked for the taskbar under
    /// its old id stays picked.
    pub fn take_moved(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.moved)
    }

    pub fn list(&self) -> &[Peripheral] {
        &self.list
    }

    /// Folds a poll into the list: what answered is connected now, what did not keeps
    /// its last reading as disconnected. Saves when anything changed.
    pub fn update(&mut self, polled: Vec<Peripheral>) {
        let before = self.list.clone();
        let now = now_ms();
        for p in &mut self.list {
            p.connected = false;
        }
        for mut p in polled {
            if p.connected {
                p.seen = now;
            }
            match self.list.iter_mut().find(|o| o.id == p.id) {
                Some(o) => {
                    // A disconnected Bluetooth device still reports its last level; keep
                    // the time we last saw it connected.
                    if !p.connected {
                        p.seen = o.seen;
                    }
                    *o = p;
                }
                None => self.list.push(p),
            }
        }
        // A device first seen over Bluetooth alone (its other source was asleep) and read
        // the better way since: the Bluetooth entry would stay behind as a disconnected twin.
        let better: Vec<(String, String)> = self.list.iter().filter(|p| p.connected && p.via != Via::Bluetooth).map(|p| (p.name.to_ascii_lowercase(), p.id.clone())).collect();
        let moved = &mut self.moved;
        self.list.retain(|p| {
            let twin = (!p.connected && p.via == Via::Bluetooth).then(|| better.iter().find(|(name, _)| *name == p.name.to_ascii_lowercase())).flatten();
            if let Some((_, id)) = twin {
                moved.push((p.id.clone(), id.clone()));
            }
            twin.is_none()
        });
        // Connected first, then the most recently seen.
        self.list.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.seen.cmp(&a.seen)).then(a.name.cmp(&b.name)));
        let changed = self.list.len() != before.len() || self.list.iter().zip(&before).any(|(a, b)| a.id != b.id || a.percent != b.percent || a.connected != b.connected);
        if changed {
            if let Some(f) = &self.file {
                if let Ok(json) = serde_json::to_vec(&self.list) {
                    // Write-then-rename: a crash mid-write must not cost the list.
                    let tmp = f.with_extension("json.tmp");
                    if std::fs::write(&tmp, json).is_ok() {
                        let _ = std::fs::rename(tmp, f);
                    }
                }
            }
        }
    }
}

/// The devices the taskbar shows right now, in order: picked and connected.
pub fn on_taskbar(list: &[Peripheral]) -> impl Iterator<Item = &Peripheral> {
    list.iter().filter(|p| p.shown && p.connected)
}

/// Polls on its own thread, every minute while `wanted` says so (the item is on the
/// taskbar or Settings is open), right away when it starts being wanted.
pub fn spawn_poller(list: std::sync::Arc<parking_lot::RwLock<Peripherals>>, wanted: impl Fn() -> bool + Send + 'static, moved: impl Fn(Vec<(String, String)>) + Send + 'static) {
    std::thread::Builder::new()
        .name("peripherals".into())
        .spawn(move || {
            let mut last: Option<std::time::Instant> = None;
            loop {
                if wanted() && last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60)) {
                    let polled = poll();
                    let replaced = {
                        let mut list = list.write();
                        list.update(polled);
                        list.take_moved()
                    };
                    if !replaced.is_empty() {
                        moved(replaced);
                    }
                    last = Some(std::time::Instant::now());
                } else if !wanted() {
                    last = None;
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        })
        .expect("spawn peripherals");
}

/// Polls every source once. Takes a second or two with a receiver whose devices sleep.
pub fn poll() -> Vec<Peripheral> {
    let cols = hid_collections();
    let mut all = bluetooth();
    for p in logitech(&cols).into_iter().chain(controllers()).chain(playstation(&cols)).chain(usb_batteries(&cols)) {
        // A Bluetooth device read another way too (a Logitech mouse, an Xbox controller)
        // shows up twice; the other source has the charging state.
        all.retain(|b| !(b.via == Via::Bluetooth && b.connected && b.name.eq_ignore_ascii_case(&p.name)));
        if !all.iter().any(|o| o.id == p.id) {
            all.push(p);
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test -- --ignored --nocapture live_peripherals` to see what this machine has.
    #[test]
    #[ignore]
    fn live_peripherals() {
        let t = std::time::Instant::now();
        for p in poll() {
            println!("{p:?}");
        }
        println!("took {:?}", t.elapsed());
    }

    #[test]
    fn short_names() {
        let p = |n: &str| Peripheral { name: n.into(), ..Default::default() }.short_name();
        assert_eq!(p("MX Master 3S"), "MX 3S");
        assert_eq!(p("MX Mechanical Mini"), "MX MINI");
        assert_eq!(p("QCY MeloBuds Pro"), "QCY");
        assert_eq!(p("EDIFIER W830BT"), "EDIFIER");
        assert_eq!(p("Xbox Wireless Controller"), "XBOX");
    }

    #[test]
    fn names_lose_the_profile() {
        assert_eq!(device_name("QCY MeloBuds Pro Hands-Free AG"), "QCY MeloBuds Pro");
        assert_eq!(device_name("EDIFIER W830BT"), "EDIFIER W830BT");
    }

    #[test]
    fn class_of_device() {
        // Two headsets: major class 4 (audio/video).
        assert_eq!(kind_from_class(2360324), Kind::Headset);
        assert_eq!(kind_from_class(0x2580), Kind::Mouse);
        assert_eq!(kind_from_class(0x2540), Kind::Keyboard);
    }

    #[test]
    fn voltage_curve() {
        assert_eq!(percent_from_mv(4200), 100);
        assert_eq!(percent_from_mv(3810), 50);
        assert_eq!(percent_from_mv(3835), 55);
        assert_eq!(percent_from_mv(3400), 0);
        assert_eq!(battery_voltage(&[0x0e, 0xe2, 0x00]), Some((50, false)));
        assert_eq!(battery_voltage(&[0x0e, 0xe2, 0x80]), Some((50, true)));
        assert_eq!(battery_voltage(&[0x0e, 0xe2, 0x83]), Some((50, false)));
        assert_eq!(adc_measurement(&[0, 0, 0]), None);
        assert_eq!(adc_measurement(&[0x10, 0x36, 0x03]), Some((100, true)));
    }

    #[test]
    fn hidpp10_registers() {
        assert_eq!(mileage([64, 0, 0x00]), Some((64, false)));
        assert_eq!(mileage([64, 0, 0x40]), Some((64, true)));
        assert_eq!(mileage([200, 0, 0]), None);
        assert_eq!(battery_status([7, 0x00, 0]), Some((100, false)));
        assert_eq!(battery_status([3, 0x21, 0]), Some((20, true)));
        assert_eq!(battery_status([0, 0, 0]), None);
    }

    #[test]
    fn playstation_reports() {
        let mut usb = [0u8; 64];
        usb[0] = 0x01;
        // DualSense on its cable, level 7, charging.
        usb[53] = 0x17;
        assert_eq!(playstation_battery(true, false, &usb), Some((75, true)));
        usb[53] = 0x2a;
        assert_eq!(playstation_battery(true, false, &usb), Some((100, true)));
        usb[53] = 0xa3;
        assert_eq!(playstation_battery(true, false, &usb), None);
        // DualShock 4: level 4 unplugged, level 11 plugged in.
        usb[30] = 0x04;
        assert_eq!(playstation_battery(false, false, &usb), Some((45, false)));
        usb[30] = 0x1b;
        assert_eq!(playstation_battery(false, false, &usb), Some((100, true)));
        // Over Bluetooth, the simple report carries no battery; the full one, shifted.
        let mut bt = [0u8; 78];
        bt[0] = 0x01;
        assert_eq!(playstation_battery(true, true, &bt), None);
        bt[0] = 0x31;
        bt[54] = 0x05;
        assert_eq!(playstation_battery(true, true, &bt), Some((55, false)));
        bt[0] = 0x11;
        bt[32] = 0x08;
        assert_eq!(playstation_battery(false, true, &bt), Some((85, false)));
    }

    #[test]
    fn hid_battery_scale() {
        let b = |min, max, bits| BatteryValue { feature: true, report: 0, link: 0, min, max, bits };
        assert_eq!(scale(50, &b(0, 100, 8)), Some(50));
        assert_eq!(scale(255, &b(0, -1, 8)), Some(100));
        assert_eq!(scale(128, &b(0, 255, 8)), Some(50));
        assert_eq!(scale(300, &b(0, 100, 16)), None);
    }

    #[test]
    fn kinds_from_names() {
        assert_eq!(kind_from_name("Logitech G935 Gaming Headset"), Kind::Headset);
        assert_eq!(kind_from_name("Wireless Mouse M705"), Kind::Mouse);
        assert_eq!(kind_from_name("K800 Keyboard"), Kind::Keyboard);
        assert_eq!(kind_from_name("Xbox Wireless Controller"), Kind::Gamepad);
        assert_eq!(kind_from_name("Thing"), Kind::Other);
    }

    #[test]
    fn update_keeps_the_last_reading() {
        let mut ps = Peripherals::load(None);
        let p = Peripheral { id: "bt:x".into(), name: "X".into(), percent: 40, connected: true, ..Default::default() };
        ps.update(vec![p.clone()]);
        let seen = ps.list()[0].seen;
        assert!(seen > 0 && ps.list()[0].connected);
        ps.update(vec![]);
        assert!(!ps.list()[0].connected);
        assert_eq!((ps.list()[0].percent, ps.list()[0].seen), (40, seen));
        ps.update(vec![Peripheral { connected: false, percent: 35, ..p }]);
        assert_eq!((ps.list()[0].percent, ps.list()[0].seen), (35, seen));
    }
}
