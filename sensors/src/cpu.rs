//! CPU die temperatures through PawnIO.
//!
//! Intel: each core's digital thermal sensor (IA32_THERM_STATUS, read on that core)
//! and the package sensor, as degrees below TjMax.
//! AMD (family 17h-1Ah): Tctl and the CCD sensors from the SMU's thermal registers.

use crate::pawnio::{AMD_FAMILY_17, INTEL_MSR, Lib, Module};
use serde::Serialize;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

const MSR_IA32_THERM_STATUS: u64 = 0x19C;
const MSR_IA32_TEMPERATURE_TARGET: u64 = 0x1A2;
const MSR_IA32_PACKAGE_THERM_STATUS: u64 = 0x1B1;

const SMN_THM_TCON_CUR_TMP: u64 = 0x0005_9800;
const SMN_THM_CCD_TEMP: u64 = 0x0005_9954;
const AMD_MAX_CCDS: u64 = 8;

#[derive(Serialize, Clone, Default, Debug)]
pub struct CpuTemps {
    /// "intel" or "amd".
    pub vendor: &'static str,
    /// Package (Intel) or Tctl (AMD), °C.
    pub package: Option<f32>,
    /// Per logical CPU (Intel) or per CCD (AMD), °C.
    pub cores: Vec<f32>,
    pub tjmax: Option<f32>,
}

pub enum Reader {
    Intel { msr: Module, tjmax: f32, cpus: usize },
    Amd { smn: Module, ccds: Vec<u64> },
}

impl Reader {
    /// The reader whose module accepts this CPU, if any.
    pub fn open(lib: &Lib) -> Result<Self, String> {
        let intel = lib.module(INTEL_MSR).and_then(|msr| {
            let target = msr.call::<1>(b"ioctl_read_msr\0", &[MSR_IA32_TEMPERATURE_TARGET])?[0];
            let tjmax = ((target >> 16) & 0xFF) as f32;
            // Some firmware leaves the field empty; 100 °C is the common value.
            let tjmax = if tjmax > 0.0 { tjmax } else { 100.0 };
            let cpus = std::thread::available_parallelism().map_or(1, |n| n.get()).min(64);
            Ok(Reader::Intel { msr, tjmax, cpus })
        });
        let intel_err = match intel {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let amd = lib.module(AMD_FAMILY_17).map(|smn| {
            // A CCD is present when its register reports a valid reading.
            let ccds = (0..AMD_MAX_CCDS)
                .filter(|i| smn.call::<1>(b"ioctl_read_smn\0", &[SMN_THM_CCD_TEMP + 4 * i]).is_ok_and(|v| v[0] & (1 << 11) != 0))
                .collect();
            Reader::Amd { smn, ccds }
        });
        amd.map_err(|e| format!("IntelMSR: {intel_err}; AMDFamily17: {e}"))
    }

    /// The loaded module, for other readers of the same MSRs (RAPL energy).
    pub fn module(&self) -> &Module {
        match self {
            Reader::Intel { msr, .. } => msr,
            Reader::Amd { smn, .. } => smn,
        }
    }

    pub fn is_amd(&self) -> bool {
        matches!(self, Reader::Amd { .. })
    }

    pub fn read(&self) -> CpuTemps {
        match self {
            Reader::Intel { msr, tjmax, cpus } => {
                let read = |reg| msr.call::<1>(b"ioctl_read_msr\0", &[reg]).ok().map(|v| v[0]);
                let below = |v: u64| *tjmax - ((v >> 16) & 0x7F) as f32;
                let mut cores = Vec::with_capacity(*cpus);
                unsafe {
                    let thread = GetCurrentThread();
                    let previous = SetThreadAffinityMask(thread, 1);
                    for cpu in 0..*cpus {
                        // The MSR belongs to the core the instruction runs on.
                        SetThreadAffinityMask(thread, 1usize << cpu);
                        if let Some(v) = read(MSR_IA32_THERM_STATUS).filter(|v| v & (1 << 31) != 0) {
                            cores.push(below(v));
                        }
                    }
                    if previous != 0 {
                        SetThreadAffinityMask(thread, previous);
                    }
                }
                CpuTemps { vendor: "intel", package: read(MSR_IA32_PACKAGE_THERM_STATUS).map(below), cores, tjmax: Some(*tjmax) }
            }
            Reader::Amd { smn, ccds } => {
                let read = |reg| smn.call::<1>(b"ioctl_read_smn\0", &[reg]).ok().map(|v| v[0]);
                let tctl = read(SMN_THM_TCON_CUR_TMP).map(|v| {
                    let t = ((v >> 21) & 0x7FF) as f32 / 8.0;
                    // "Range select": the reading is offset by 49 °C.
                    if v & (1 << 19) != 0 { t - 49.0 } else { t }
                });
                let cores = ccds
                    .iter()
                    .filter_map(|i| read(SMN_THM_CCD_TEMP + 4 * i))
                    .map(|v| (v & 0x7FF) as f32 / 8.0 - 49.0)
                    .collect();
                CpuTemps { vendor: "amd", package: tctl, cores, tjmax: None }
            }
        }
    }
}
