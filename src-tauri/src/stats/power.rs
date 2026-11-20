//! Battery: charge from `GetSystemPowerStatus`, and the details the popup shows (health,
//! cycles, power draw) straight from the battery driver through its IOCTLs.

use super::model::Battery;
use std::time::{Duration, Instant};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Power::*;
use windows::core::PCWSTR;

const NO_BATTERY: u8 = 128;
const CHARGING: u8 = 8;
const UNKNOWN: u8 = 255;
/// Design/full capacity barely move; re-read them once a minute.
const INFO_REFRESH: Duration = Duration::from_secs(60);
/// `BATTERY_UNKNOWN_RATE` / `BATTERY_UNKNOWN_VOLTAGE`.
const UNKNOWN_RATE: i32 = i32::MIN;
const REOPEN_EVERY: Duration = Duration::from_secs(30);

struct Device {
    handle: HANDLE,
    tag: u32,
}

pub struct BatteryCollector {
    device: Option<Device>,
    info: Option<BATTERY_INFORMATION>,
    info_at: Option<Instant>,
    /// The driver stopped answering: the battery was enumerated again and has a new tag.
    lost: bool,
    opened_at: Instant,
}

unsafe impl Send for BatteryCollector {}

impl BatteryCollector {
    pub fn new() -> Self {
        Self { device: open_first_battery(), info: None, info_at: None, lost: false, opened_at: Instant::now() }
    }

    pub fn sample(&mut self) -> Option<Battery> {
        let mut s = SYSTEM_POWER_STATUS::default();
        unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
        if s.BatteryFlag == UNKNOWN || s.BatteryFlag & NO_BATTERY != 0 || s.BatteryLifePercent > 100 {
            return None;
        }
        let mut b = Battery {
            percent: s.BatteryLifePercent as f32,
            charging: s.BatteryFlag & CHARGING != 0,
            plugged: s.ACLineStatus == 1,
            secs_left: (s.BatteryLifeTime != u32::MAX && s.ACLineStatus != 1).then_some(s.BatteryLifeTime),
            saver: s.SystemStatusFlag == 1,
            ..Default::default()
        };

        // No device (the battery came after wStats) or a dead tag: open it again, unhurried.
        if (self.device.is_none() || self.lost) && self.opened_at.elapsed() >= REOPEN_EVERY {
            self.close();
            self.device = open_first_battery();
            self.opened_at = Instant::now();
            self.lost = false;
            self.info_at = None;
        }

        if let Some(dev) = &self.device {
            if self.info_at.is_none_or(|t| t.elapsed() >= INFO_REFRESH) {
                self.info = query_info(dev);
                self.info_at = Some(Instant::now());
            }
            if let Some(info) = &self.info {
                if info.DesignedCapacity > 0 && info.FullChargedCapacity > 0 {
                    b.design_mwh = Some(info.DesignedCapacity);
                    b.full_mwh = Some(info.FullChargedCapacity);
                    b.health = Some((info.FullChargedCapacity as f32 * 100.0 / info.DesignedCapacity as f32).min(100.0));
                }
                b.cycles = (info.CycleCount > 0).then_some(info.CycleCount);
            }
            match query_status(dev) {
                Some(st) => {
                    // Positive while charging, negative while discharging, in mW.
                    b.rate_mw = (st.Rate != UNKNOWN_RATE && st.Rate != 0).then_some(st.Rate);
                    b.voltage_mv = (st.Voltage != u32::MAX && st.Voltage > 0).then_some(st.Voltage);
                }
                None => self.lost = true,
            }
        }
        Some(b)
    }
}

impl BatteryCollector {
    fn close(&mut self) {
        if let Some(d) = self.device.take() {
            unsafe {
                let _ = CloseHandle(d.handle);
            }
        }
    }
}

impl Drop for BatteryCollector {
    fn drop(&mut self) {
        self.close();
    }
}

fn ioctl<I, O: Default>(dev: HANDLE, code: u32, input: &I) -> Option<O> {
    let mut out = O::default();
    let mut n = 0u32;
    unsafe {
        DeviceIoControl(
            dev,
            code,
            Some((input as *const I).cast()),
            size_of::<I>() as u32,
            Some((&mut out as *mut O).cast()),
            size_of::<O>() as u32,
            Some(&mut n),
            None,
        )
    }
    .ok()?;
    Some(out)
}

fn query_info(d: &Device) -> Option<BATTERY_INFORMATION> {
    let q = BATTERY_QUERY_INFORMATION { BatteryTag: d.tag, InformationLevel: BatteryInformation, AtRate: 0 };
    ioctl(d.handle, IOCTL_BATTERY_QUERY_INFORMATION, &q)
}

fn query_status(d: &Device) -> Option<BATTERY_STATUS> {
    let w = BATTERY_WAIT_STATUS { BatteryTag: d.tag, ..Default::default() };
    ioctl(d.handle, IOCTL_BATTERY_QUERY_STATUS, &w)
}

/// Opens the first battery device interface and gets its tag.
fn open_first_battery() -> Option<Device> {
    unsafe {
        let set = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_BATTERY), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE).ok()?;
        let mut iface = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        let found = SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVCLASS_BATTERY, 0, &mut iface).is_ok();
        let path = found.then(|| {
            let mut needed = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &iface, None, 0, Some(&mut needed), None);
            let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
            let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(set, &iface, Some(detail), needed, None, None).ok()?;
            Some(PCWSTR((*detail).DevicePath.as_ptr()).to_string().ok()?)
        });
        let _ = SetupDiDestroyDeviceInfoList(set);
        let path = path.flatten()?;

        let handle = CreateFileW(
            &windows::core::HSTRING::from(path),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .ok()?;
        let wait = 0u32;
        let Some(tag) = ioctl::<u32, u32>(handle, IOCTL_BATTERY_QUERY_TAG, &wait).filter(|t| *t != 0) else {
            let _ = CloseHandle(handle);
            return None;
        };
        Some(Device { handle, tag })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_battery_details_when_present() {
        let mut c = BatteryCollector::new();
        if let Some(b) = c.sample() {
            println!(
                "battery {}% health={:?} cycles={:?} rate={:?}mW voltage={:?}mV design={:?} full={:?}",
                b.percent, b.health, b.cycles, b.rate_mw, b.voltage_mv, b.design_mwh, b.full_mwh
            );
        }
    }
}
