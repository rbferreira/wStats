//! NVIDIA GPUs: board power, each fan's speed, and temperature when the kernel interface
//! (D3DKMT) does not report it, from NVML: the management library every NVIDIA driver
//! installs (nvml.dll). No admin rights needed. Absent on other GPUs, and then unused.

use std::ffi::c_void;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::{PCSTR, s, w};

type Device = *mut c_void;
type InitFn = unsafe extern "C" fn() -> i32;
type CountFn = unsafe extern "C" fn(*mut u32) -> i32;
type HandleFn = unsafe extern "C" fn(u32, *mut Device) -> i32;
type U32Fn = unsafe extern "C" fn(Device, *mut u32) -> i32;
type TempFn = unsafe extern "C" fn(Device, u32, *mut u32) -> i32;
type FanRpmFn = unsafe extern "C" fn(Device, *mut FanSpeedInfo) -> i32;

const NVML_SUCCESS: i32 = 0;
const NVML_TEMPERATURE_GPU: u32 = 0;

/// `nvmlFanSpeedInfo_t`.
#[repr(C)]
#[derive(Default)]
struct FanSpeedInfo {
    version: u32,
    fan: u32,
    speed: u32,
}
/// `nvmlFanSpeedInfo_v1`: the struct size, with the version in the top byte.
const FAN_SPEED_INFO_V1: u32 = size_of::<FanSpeedInfo>() as u32 | 1 << 24;

/// One NVIDIA GPU's readings.
#[derive(Default, Clone, Debug)]
pub struct Reading {
    pub watts: Option<f32>,
    pub temp_c: Option<f32>,
    /// Each fan's speed, in RPM; 0 while stopped (zero-RPM mode). Empty when the driver
    /// cannot tell (older drivers, passively cooled cards).
    pub fans_rpm: Vec<u32>,
}

struct Gpu {
    device: Device,
    fans: u32,
}

pub struct Nvml {
    gpus: Vec<Gpu>,
    power: U32Fn,
    temp: TempFn,
    fan_rpm: Option<FanRpmFn>,
}

// NVML handles are process-wide and thread-safe; we use them from the sampler only.
unsafe impl Send for Nvml {}

impl Nvml {
    /// `None` without an NVIDIA driver.
    pub fn open() -> Option<Self> {
        unsafe {
            let lib = LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let get = |name: PCSTR| GetProcAddress(lib, name);
            let init: InitFn = std::mem::transmute(get(s!("nvmlInit_v2"))?);
            let count: CountFn = std::mem::transmute(get(s!("nvmlDeviceGetCount_v2"))?);
            let handle: HandleFn = std::mem::transmute(get(s!("nvmlDeviceGetHandleByIndex_v2"))?);
            let power: U32Fn = std::mem::transmute(get(s!("nvmlDeviceGetPowerUsage"))?);
            let temp: TempFn = std::mem::transmute(get(s!("nvmlDeviceGetTemperature"))?);
            // Newer drivers only.
            let num_fans: Option<U32Fn> = get(s!("nvmlDeviceGetNumFans")).map(|f| std::mem::transmute(f));
            let fan_rpm: Option<FanRpmFn> = get(s!("nvmlDeviceGetFanSpeedRPM")).map(|f| std::mem::transmute(f));
            if init() != NVML_SUCCESS {
                return None;
            }
            let mut n = 0u32;
            if count(&mut n) != NVML_SUCCESS {
                return None;
            }
            let gpus: Vec<Gpu> = (0..n)
                .filter_map(|i| {
                    let mut device: Device = std::ptr::null_mut();
                    if handle(i, &mut device) != NVML_SUCCESS {
                        return None;
                    }
                    let mut fans = 0u32;
                    if num_fans.is_none_or(|f| f(device, &mut fans) != NVML_SUCCESS) {
                        fans = 0;
                    }
                    Some(Gpu { device, fans: fans.min(8) })
                })
                .collect();
            (!gpus.is_empty()).then_some(Self { gpus, power, temp, fan_rpm })
        }
    }

    /// Readings per GPU, in NVML's order (the driver's enumeration order).
    pub fn sample(&self) -> Vec<Reading> {
        self.gpus
            .iter()
            .map(|g| unsafe {
                let d = g.device;
                let mut v = 0u32;
                let watts = ((self.power)(d, &mut v) == NVML_SUCCESS).then(|| v as f32 / 1000.0);
                let temp_c = ((self.temp)(d, NVML_TEMPERATURE_GPU, &mut v) == NVML_SUCCESS).then_some(v as f32);
                let fans_rpm = self
                    .fan_rpm
                    .map(|f| {
                        (0..g.fans)
                            .map_while(|fan| {
                                let mut info = FanSpeedInfo { version: FAN_SPEED_INFO_V1, fan, speed: 0 };
                                (f(d, &mut info) == NVML_SUCCESS).then_some(info.speed)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Reading { watts, temp_c, fans_rpm }
            })
            .collect()
    }
}
