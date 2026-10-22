//! GPU utilization the way Task Manager computes it: for each adapter, sum every
//! process's share per engine type, and report the busiest engine type.

use super::model::Gpu;
use super::nvml::Nvml;
use super::pdh::{Counter, Query};
use crate::win::wide_to_string;
use std::collections::HashMap;
use windows::Wdk::Graphics::Direct3D::*;
use windows::Win32::Graphics::Dxgi::*;

const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;
const VENDOR_NVIDIA: u32 = 0x10DE;

struct Adapter {
    name: String,
    vendor: u32,
    luid: String,
    /// Kernel handle for the D3DKMT perf queries (temperature, fan, clock); 0 if unavailable.
    kmt: u32,
    dedicated_total: u64,
    shared_total: u64,
}

pub struct GpuCollector {
    engines: Option<Counter>,
    dedicated: Option<Counter>,
    shared: Option<Counter>,
    adapters: Vec<Adapter>,
    /// NVIDIA's management library, when an NVIDIA GPU is present.
    nvml: Option<Nvml>,
}

impl GpuCollector {
    pub fn new(q: &Query) -> Self {
        let adapters = list_adapters();
        Self {
            engines: q.add(r"\GPU Engine(*)\Utilization Percentage"),
            dedicated: q.add(r"\GPU Adapter Memory(*)\Dedicated Usage"),
            shared: q.add(r"\GPU Adapter Memory(*)\Shared Usage"),
            nvml: adapters.iter().any(|a| a.vendor == VENDOR_NVIDIA).then(Nvml::open).flatten(),
            adapters,
        }
    }

    pub fn sample(&self, q: &Query) -> Vec<Gpu> {
        // luid -> engine type -> summed utilization
        let mut per: HashMap<String, HashMap<String, f64>> = HashMap::new();
        for (name, v) in q.array(self.engines) {
            let eng = name.split("engtype_").nth(1).unwrap_or("");
            // Unnamed and security-controller engines are never interesting to look at.
            if eng.is_empty() || eng == "GSC" {
                continue;
            }
            if let Some(luid) = luid_of(&name) {
                *per.entry(luid).or_default().entry(eng.to_string()).or_default() += v.max(0.0);
            }
        }
        let mem = |c| -> HashMap<String, u64> {
            q.array(c)
                .into_iter()
                .filter_map(|(n, v)| Some((luid_of(&n)?, v.max(0.0) as u64)))
                .fold(HashMap::new(), |mut m, (k, v)| {
                    *m.entry(k).or_default() += v;
                    m
                })
        };
        let dedicated = mem(self.dedicated);
        let shared = mem(self.shared);

        // NVML lists NVIDIA GPUs in the driver's order, as DXGI does.
        let mut nvidia = self.nvml.as_ref().map(Nvml::sample).unwrap_or_default().into_iter();
        let nv: Vec<_> = self.adapters.iter().map(|a| if a.vendor == VENDOR_NVIDIA { nvidia.next() } else { None }).collect();

        // DXGI can list one GPU under a second LUID (e.g. for an indirect display); only the
        // LUID that owns engine counters is the real one.
        self.adapters
            .iter()
            .zip(nv)
            .filter(|(a, _)| per.contains_key(&a.luid))
            .map(|(a, nv)| {
                let mut engines: Vec<(String, f32)> = per
                    .get(&a.luid)
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.min(100.0) as f32)).collect())
                    .unwrap_or_default();
                engines.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
                let mut g = Gpu {
                    name: a.name.clone(),
                    luid: a.luid.clone(),
                    util: engines.first().map(|e| e.1).unwrap_or(0.0),
                    engines,
                    dedicated_used: dedicated.get(&a.luid).copied().unwrap_or(0),
                    dedicated_total: a.dedicated_total,
                    shared_used: shared.get(&a.luid).copied().unwrap_or(0),
                    shared_total: a.shared_total,
                    ..kernel_perf(a.kmt)
                };
                if let Some(r) = nv {
                    g.power_w = r.watts;
                    g.temp_c = g.temp_c.or(r.temp_c);
                    g.fan_rpm = g.fan_rpm.or(r.fans_rpm.iter().max().copied());
                    g.fans_rpm = r.fans_rpm;
                }
                g
            })
            .collect()
    }
}

/// Temperature, fan and 3D clock from the kernel driver interface (no admin needed).
fn kernel_perf(handle: u32) -> Gpu {
    let mut g = Gpu::default();
    if handle == 0 {
        return g;
    }
    let mut perf = D3DKMT_ADAPTER_PERFDATA::default();
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: KMTQAITYPE_ADAPTERPERFDATA,
        pPrivateDriverData: (&mut perf as *mut D3DKMT_ADAPTER_PERFDATA).cast(),
        PrivateDriverDataSize: size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
    };
    if unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok() {
        // Deci-degrees Celsius; 0 means "not reported".
        g.temp_c = (perf.Temperature > 0).then(|| perf.Temperature as f32 / 10.0);
        g.fan_rpm = (perf.FanRPM > 0).then_some(perf.FanRPM);
    }
    let mut node = D3DKMT_NODE_PERFDATA::default();
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: KMTQAITYPE_NODEPERFDATA,
        pPrivateDriverData: (&mut node as *mut D3DKMT_NODE_PERFDATA).cast(),
        PrivateDriverDataSize: size_of::<D3DKMT_NODE_PERFDATA>() as u32,
    };
    if unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok() && node.Frequency > 0 {
        g.freq_mhz = Some(node.Frequency as f32 / 1e6);
    }
    g
}

/// `"pid_12_luid_0x00000000_0x0000D1A3_phys_0_eng_0_engtype_3D"` -> `"0x00000000_0x0000d1a3"`.
pub(super) fn luid_of(instance: &str) -> Option<String> {
    let rest = instance.split("luid_").nth(1)?;
    let mut parts = rest.splitn(3, '_');
    let (hi, lo) = (parts.next()?, parts.next()?);
    Some(format!("{hi}_{lo}").to_ascii_lowercase())
}

fn list_adapters() -> Vec<Adapter> {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return Vec::new() };
    let mut out: Vec<Adapter> = Vec::new();
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let Ok(d) = (unsafe { adapter.GetDesc1() }) else { continue };
        if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE != 0 {
            continue;
        }
        let luid = format!("0x{:08x}_0x{:08x}", d.AdapterLuid.HighPart as u32, d.AdapterLuid.LowPart);
        if out.iter().any(|a| a.luid == luid) {
            continue;
        }
        let mut open = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: d.AdapterLuid, hAdapter: 0 };
        let kmt = if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) }.is_ok() { open.hAdapter } else { 0 };
        out.push(Adapter {
            name: wide_to_string(&d.Description),
            vendor: d.VendorId,
            luid,
            kmt,
            dedicated_total: d.DedicatedVideoMemory as u64,
            shared_total: d.SharedSystemMemory as u64,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luid_parsing() {
        assert_eq!(
            luid_of("pid_1234_luid_0x00000000_0x0000D1A3_phys_0_eng_0_engtype_3D").as_deref(),
            Some("0x00000000_0x0000d1a3")
        );
        assert_eq!(luid_of("luid_0x00000000_0x0000D1A3_phys_0").as_deref(), Some("0x00000000_0x0000d1a3"));
        assert_eq!(luid_of("_Total"), None);
    }
}
