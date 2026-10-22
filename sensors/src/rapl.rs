//! CPU power from the RAPL energy counters, read through the same PawnIO module as the
//! temperatures. Windows publishes these itself (the "Energy Meter" counters) only on
//! some machines, mostly laptops; desktops, and every AMD CPU, need this.
//!
//! Each counter is a free-running 32-bit energy total; power is its change over time.

use crate::cpu::Reader;
use serde::Serialize;
use std::time::Instant;

const INTEL_POWER_UNIT: u64 = 0x606;
const INTEL_DOMAINS: [(&str, u64); 5] = [("PSYS", 0x64D), ("PKG", 0x611), ("PP0", 0x639), ("PP1", 0x641), ("DRAM", 0x619)];

const AMD_POWER_UNIT: u64 = 0xC001_0299;
const AMD_DOMAINS: [(&str, u64); 1] = [("PKG", 0xC001_029B)];

/// Ignore readings over this: a counter that jumped (reset, sleep) rather than a real draw.
const MAX_WATTS: f64 = 1000.0;

#[derive(Serialize, Clone, Debug)]
pub struct Power {
    /// RAPL domain, as Windows names them: "PKG", "PP0", "PP1", "DRAM", "PSYS".
    pub domain: &'static str,
    pub watts: f32,
}

struct Domain {
    name: &'static str,
    msr: u64,
    last: Option<(u32, Instant)>,
}

pub struct Rapl {
    joules_per_unit: f64,
    domains: Vec<Domain>,
}

fn read_msr(cpu: &Reader, msr: u64) -> Option<u64> {
    cpu.module().call::<1>(b"ioctl_read_msr\0", &[msr]).ok().map(|v| v[0])
}

impl Rapl {
    /// The domains this CPU counts; `None` if it counts none.
    pub fn open(cpu: &Reader) -> Option<Self> {
        let (unit_msr, domains): (u64, &[(&'static str, u64)]) = if cpu.is_amd() { (AMD_POWER_UNIT, &AMD_DOMAINS) } else { (INTEL_POWER_UNIT, &INTEL_DOMAINS) };
        // Energy status unit: bits 12:8, energy counts in 1/2^ESU joules.
        let esu = (read_msr(cpu, unit_msr)? >> 8) & 0x1F;
        let domains: Vec<Domain> = domains
            .iter()
            // A domain the CPU lacks fails to read or never counts.
            .filter(|(_, msr)| read_msr(cpu, *msr).is_some_and(|v| v as u32 != 0))
            .map(|&(name, msr)| Domain { name, msr, last: None })
            .collect();
        (!domains.is_empty()).then(|| Rapl { joules_per_unit: 1.0 / (1u64 << esu) as f64, domains })
    }

    /// Average power of each domain since the previous call (nothing on the first).
    pub fn read(&mut self, cpu: &Reader) -> Vec<Power> {
        let now = Instant::now();
        let mut out = Vec::new();
        for d in &mut self.domains {
            let Some(raw) = read_msr(cpu, d.msr).map(|v| v as u32) else { continue };
            if let Some((prev, at)) = d.last {
                let secs = now.duration_since(at).as_secs_f64();
                if secs > 0.05 {
                    let watts = raw.wrapping_sub(prev) as f64 * self.joules_per_unit / secs;
                    if watts < MAX_WATTS {
                        out.push(Power { domain: d.name, watts: watts as f32 });
                    }
                }
            }
            d.last = Some((raw, now));
        }
        out
    }
}
