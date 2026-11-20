//! The NPU (Intel AI Boost, AMD XDNA, Qualcomm Hexagon): a compute-only adapter that
//! DXGI does not list, so the GPU collector never sees it. Its load comes from the same
//! counters as a GPU's, which are per process: that is also who is using it.

use super::gpu::luid_of;
use super::model::{Npu, NpuProc};
use super::pdh::{Counter, Query};
use super::procs::Exes;
use crate::win::wide_to_string;
use std::collections::HashMap;
use windows::Wdk::Graphics::Direct3D::*;

/// `D3DKMT_ADAPTERTYPE` bits. An NPU says it renders too; being compute-only and real
/// hardware is what sets it apart.
const SOFTWARE_DEVICE: u32 = 1 << 2;
const COMPUTE_ONLY: u32 = 1 << 11;
const INCLUDE_COMPUTE_ONLY: u64 = 1;

pub struct NpuCollector {
    /// Name and LUID; `None` on machines without an NPU, and then nothing is read.
    npu: Option<(String, String)>,
    engines: Option<Counter>,
    shared: Option<Counter>,
    exes: Exes,
    /// pid -> (exe, display name), looked up once per process.
    seen: HashMap<u32, (String, String)>,
    looked_at: std::time::Instant,
}

/// How often the adapter is looked up again: a driver update gives it a new LUID.
const LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

impl NpuCollector {
    pub fn new(q: &Query) -> Self {
        let npu = find();
        let add = |path| npu.as_ref().and_then(|_| q.add(path));
        Self {
            engines: add(r"\GPU Engine(*)\Utilization Percentage"),
            shared: add(r"\GPU Adapter Memory(*)\Shared Usage"),
            npu,
            exes: Exes::new(),
            seen: HashMap::new(),
            looked_at: std::time::Instant::now(),
        }
    }

    pub fn sample(&mut self, q: &Query) -> Option<Npu> {
        if self.looked_at.elapsed() >= LOOK_EVERY {
            self.looked_at = std::time::Instant::now();
            let npu = find();
            if npu != self.npu {
                log::info!("npu: the adapter changed, following it");
                self.npu = npu;
            }
            // An NPU that came after wStats (its driver installed meanwhile).
            if self.npu.is_some() && self.engines.is_none() {
                self.engines = q.add(r"\GPU Engine(*)\Utilization Percentage");
                self.shared = q.add(r"\GPU Adapter Memory(*)\Shared Usage");
            }
        }
        let (name, luid) = self.npu.clone()?;
        let mine = |instance: &str| luid_of(instance).as_deref() == Some(luid.as_str());

        let mut per_pid: HashMap<u32, f32> = HashMap::new();
        for (instance, v) in q.array(self.engines) {
            if let (true, Some(pid)) = (mine(&instance), pid_of(&instance)) {
                *per_pid.entry(pid).or_default() += v.max(0.0) as f32;
            }
        }
        let shared_used = q.array(self.shared).into_iter().filter(|(n, _)| mine(n)).map(|(_, v)| v.max(0.0) as u64).sum();

        // Grouped by executable, like the process lists.
        let Self { exes, seen, .. } = self;
        seen.retain(|pid, _| per_pid.contains_key(pid));
        let mut groups: HashMap<String, NpuProc> = HashMap::new();
        for (&pid, &util) in &per_pid {
            if util < 0.05 {
                continue;
            }
            let (exe, label) = seen
                .entry(pid)
                .or_insert_with(|| {
                    let exe = exes.path(pid);
                    let file = exe.rsplit('\\').next().unwrap_or_default().to_string();
                    let label = if exe.is_empty() { format!("PID {pid}") } else { exes.label(&exe, &file) };
                    (exe, label)
                })
                .clone();
            let key = if exe.is_empty() { label.clone() } else { exe.to_lowercase() };
            groups.entry(key).or_insert_with(|| NpuProc { name: label, exe, util: 0.0 }).util += util;
        }
        let mut procs: Vec<NpuProc> = groups.into_values().collect();
        procs.sort_by(|a, b| b.util.total_cmp(&a.util));

        Some(Npu {
            name,
            luid,
            util: per_pid.values().sum::<f32>().min(100.0),
            shared_used,
            procs,
        })
    }
}

/// Whether this machine has an NPU.
pub fn present() -> bool {
    find().is_some()
}

/// `"pid_22928_luid_0x00000000_0x000121F0_phys_0_eng_0_engtype_Compute"` -> 22928.
fn pid_of(instance: &str) -> Option<u32> {
    instance.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

/// The first adapter that computes but does not render: what Task Manager calls the NPU.
fn find() -> Option<(String, String)> {
    // The older enumeration leaves compute-only adapters out; this one includes them
    // when asked (filter bit 0, IncludeComputeOnly).
    let mut e = D3DKMT_ENUMADAPTERS3 { Filter: D3DKMT_ENUMADAPTERS_FILTER { Value: INCLUDE_COMPUTE_ONLY }, ..Default::default() };
    // First call: how many adapters.
    unsafe { D3DKMTEnumAdapters3(&mut e) }.ok().ok()?;
    let mut list = vec![D3DKMT_ADAPTERINFO::default(); e.NumAdapters as usize];
    e.pAdapters = list.as_mut_ptr();
    unsafe { D3DKMTEnumAdapters3(&mut e) }.ok().ok()?;
    list.truncate(e.NumAdapters as usize);

    let mut found = None;
    for a in &list {
        if found.is_none() {
            let mut ty = D3DKMT_ADAPTERTYPE::default();
            let flags = if query(a.hAdapter, KMTQAITYPE_ADAPTERTYPE, &mut ty) { unsafe { ty.Anonymous.Value } } else { 0 };
            if flags & COMPUTE_ONLY != 0 && flags & SOFTWARE_DEVICE == 0 {
                // Boxed: the description is a 4096-character buffer.
                let mut desc = Box::new(D3DKMT_DRIVER_DESCRIPTION { DriverDescription: [0; 4096] });
                let name = if query(a.hAdapter, KMTQAITYPE_DRIVER_DESCRIPTION, &mut *desc) { wide_to_string(&desc.DriverDescription) } else { String::new() };
                let luid = format!("0x{:08x}_0x{:08x}", a.AdapterLuid.HighPart as u32, a.AdapterLuid.LowPart);
                found = Some((if name.trim().is_empty() { "NPU".to_string() } else { name.trim().to_string() }, luid));
            }
        }
        unsafe {
            let _ = D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: a.hAdapter });
        }
    }
    found
}

fn query<T>(adapter: u32, kind: KMTQUERYADAPTERINFOTYPE, out: &mut T) -> bool {
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter,
        Type: kind,
        pPrivateDriverData: (out as *mut T).cast(),
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&mut q) }.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_parsing() {
        assert_eq!(pid_of("pid_22928_luid_0x00000000_0x000121F0_phys_0_eng_0_engtype_Compute"), Some(22928));
        assert_eq!(pid_of("luid_0x00000000_0x000121F0_phys_0"), None);
    }

    /// Prints what this machine's NPU looks like; it passes on machines without one.
    #[test]
    fn live_npu() {
        let Some((name, luid)) = find() else { return };
        println!("npu {name} {luid}");
        assert!(!name.is_empty() && luid.starts_with("0x"));
        let q = Query::new().unwrap();
        let mut c = NpuCollector::new(&q);
        q.collect();
        std::thread::sleep(std::time::Duration::from_millis(500));
        q.collect();
        let n = c.sample(&q).expect("sample");
        println!("util={:.1} shared={}MB procs={:?}", n.util, n.shared_used >> 20, n.procs.iter().map(|p| (&p.name, p.util)).collect::<Vec<_>>());
    }
}
