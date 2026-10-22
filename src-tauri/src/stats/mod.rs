//! The sampler: one background thread that reads every collector once per tick and
//! publishes the result to a shared store.

pub mod model;

mod cpu;
mod disk;
mod gpu;
pub mod long;
mod mem;
mod net;
mod npu;
mod nvml;
mod pdh;
pub mod ping;
mod sensors;
mod power;
mod procs;
pub mod service;

pub use npu::present as npu_present;

use model::{History, Snapshot, SystemInfo};
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::System::SystemInformation::{ComputerNamePhysicalDnsHostname, GetComputerNameExW, GetTickCount64};
use windows::core::PWSTR;

pub struct Store {
    pub snapshot: Snapshot,
    pub history: History,
    /// 24 hours of one-minute averages, persisted.
    pub long: long::LongHistory,
}

pub type Shared = Arc<RwLock<Store>>;

pub fn new_store(history_file: Option<std::path::PathBuf>) -> Shared {
    Arc::new(RwLock::new(Store { snapshot: Snapshot::default(), history: History::new(), long: long::LongHistory::load(history_file) }))
}

/// How often to look for the sensor service while it is not running.
const SERVICE_RETRY: Duration = Duration::from_secs(5);
const SAVE_EVERY: Duration = Duration::from_secs(300);

/// Knobs the rest of the app turns while the sampler runs.
#[derive(Default)]
pub struct Control {
    /// Set while a popup is visible: enables the per-process scan.
    pub detailed: AtomicBool,
    pub interval_ms: AtomicU32,
    /// Set while the settings window is open: its live previews show processes too.
    pub settings_open: AtomicBool,
    pub pinger: Option<Arc<ping::Pinger>>,
    /// Fan choices from Settings.
    pub fans: RwLock<FanPrefs>,
}

#[derive(Default, Clone)]
pub struct FanPrefs {
    /// Reading ids left out of the panel and the taskbar.
    pub hidden: Vec<String>,
    /// The taskbar's fan; `None` = the fastest.
    pub bar: Option<String>,
}

pub fn spawn(store: Shared, control: Arc<Control>, on_tick: impl Fn(&Snapshot) + Send + 'static) {
    std::thread::Builder::new()
        .name("sampler".into())
        .spawn(move || run(store, control, on_tick))
        .expect("spawn sampler");
}

fn run(store: Shared, control: Arc<Control>, on_tick: impl Fn(&Snapshot)) {
    let Some(q) = pdh::Query::new() else {
        log::error!("PDH unavailable; sampler not running");
        return;
    };
    let cpu = cpu::CpuCollector::new(&q);
    let mem = mem::MemCollector::new(&q);
    let mut disk = disk::DiskCollector::new(&q);
    let gpu = gpu::GpuCollector::new(&q);
    let mut npu = npu::NpuCollector::new(&q);
    let sensor = sensors::SensorCollector::new(&q);
    let mut net = net::NetCollector::new();
    let mut procs = procs::ProcCollector::new();
    let mut battery = power::BatteryCollector::new();
    let static_info = system_info();
    let mut service: Option<service::Reader> = None;
    let mut service_tried: Option<Instant> = None;

    // Rate counters need two collections before they report anything.
    q.collect();
    net.sample();

    let mut next = Instant::now();
    let mut saved = Instant::now();
    loop {
        let interval = Duration::from_millis(control.interval_ms.load(Ordering::Relaxed).clamp(250, 10_000) as u64);
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            // Fell behind (machine was asleep): resynchronize instead of bursting.
            next = now;
        }

        q.collect();
        let detailed = control.detailed.load(Ordering::Relaxed) || control.settings_open.load(Ordering::Relaxed);
        let snap = Snapshot {
            ts: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            cpu: cpu.sample(&q),
            mem: mem.sample(&q),
            net: {
                let mut n = net.sample();
                if let Some(p) = &control.pinger {
                    n.ping_ms = p.last();
                    n.ping_host = p.host.read().clone();
                }
                n
            },
            disk: disk.sample(&q),
            gpus: gpu.sample(&q),
            npu: npu.sample(&q),
            battery: battery.sample(),
            sensors: sensor.sample(&q),
            system: SystemInfo { uptime_secs: unsafe { GetTickCount64() } / 1000, ..static_info.clone() },
            procs: detailed.then(|| procs.sample()),
        };
        let mut snap = snap;
        // The sensor service may be installed or started at any time: look for it every
        // few seconds while it is absent.
        if service.is_none() && service_tried.is_none_or(|t| t.elapsed() >= SERVICE_RETRY) {
            service_tried = Some(Instant::now());
            service = service::Reader::open();
        }
        if let Some(p) = service.as_ref().and_then(service::Reader::read) {
            snap.cpu.temp_c = service::merge(&p, &mut snap.sensors);
        }
        fold_gpu_sensors(&mut snap);
        {
            let prefs = control.fans.read();
            snap.sensors.apply_fan_prefs(&prefs.hidden, prefs.bar.as_ref());
        }
        {
            let mut s = store.write();
            s.history.record(&snap);
            s.long.record(&snap);
            s.snapshot = snap;
            if saved.elapsed() >= SAVE_EVERY {
                s.long.save();
                saved = Instant::now();
            }
        }
        on_tick(&store.read().snapshot);
    }
}

/// GPU temperatures, fans and power belong in the sensor lists too.
fn fold_gpu_sensors(s: &mut Snapshot) {
    for (i, g) in s.gpus.iter().enumerate() {
        let label = if s.gpus.len() > 1 { format!("GPU {}", i + 1) } else { "GPU".to_string() };
        if let Some(t) = g.temp_c {
            s.sensors.temps.push(model::Reading { id: format!("gpu{i}"), label: label.clone(), value: t });
        }
        if g.fans_rpm.len() > 1 {
            // One row per fan: "GPU 1", "GPU 2" (or "GPU 2 (1)" with several GPUs).
            for (n, f) in g.fans_rpm.iter().enumerate() {
                let name = if s.gpus.len() > 1 { format!("{label} ({})", n + 1) } else { format!("{label} {}", n + 1) };
                s.sensors.fans.push(model::Reading { id: format!("gpu{i}-fan{n}"), label: name, value: *f as f32 });
            }
        } else if let Some(f) = g.fan_rpm {
            s.sensors.fans.push(model::Reading { id: format!("gpu{i}-fan"), label: label.clone(), value: f as f32 });
        }
        if let Some(w) = g.power_w {
            s.sensors.power.push(model::Reading { id: format!("gpu{i}-power"), label, value: w });
        }
    }
}

fn system_info() -> SystemInfo {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    let hostname = unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, Some(PWSTR(buf.as_mut_ptr())), &mut len) }
        .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
        .unwrap_or_default();

    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let get = |v| crate::win::reg_string(HKEY_LOCAL_MACHINE, key, v).unwrap_or_default();
    let build: u32 = get("CurrentBuild").parse().unwrap_or(0);
    let mut product = get("ProductName");
    // Windows 11 still reports "Windows 10" in ProductName.
    if build >= 22000 {
        product = product.replace("Windows 10", "Windows 11");
    }
    let os = format!("{product} {} (build {build})", get("DisplayVersion")).trim().to_string();
    SystemInfo { hostname, os, uptime_secs: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real collectors against this machine and prints what they see.
    /// `cargo test -- --nocapture live_snapshot` to eyeball the values.
    #[test]
    fn live_snapshot() {
        let q = pdh::Query::new().unwrap();
        let cpu = cpu::CpuCollector::new(&q);
        let mem = mem::MemCollector::new(&q);
        let mut disk = disk::DiskCollector::new(&q);
        let gpu = gpu::GpuCollector::new(&q);
        let mut procs = procs::ProcCollector::new();
        q.collect();
        procs.sample();
        std::thread::sleep(Duration::from_millis(1000));
        q.collect();

        let c = cpu.sample(&q);
        assert!(!c.cores.is_empty(), "per-core counters");
        assert_eq!(c.cores.len(), c.core_classes.len());
        assert!((0.0..=100.0).contains(&c.total));
        let m = mem.sample(&q);
        assert!(m.total > 0 && m.used <= m.total);
        let d = disk.sample(&q);
        assert!(!d.volumes.is_empty());
        let g = gpu.sample(&q);
        let p = procs.sample();
        assert!(!p.by_mem.is_empty());

        println!("cpu {} total={:.1} user={:.1} sys={:.1} freq={:.0}/{:.0}MHz cores={:?} classes={:?} procs={} threads={}",
            c.name, c.total, c.user, c.system, c.freq_mhz, c.base_mhz, c.cores, c.core_classes, c.processes, c.threads);
        println!("mem used={}MB/{}MB cached={}MB modified={}MB free={}MB swap={}MB/{}MB",
            m.used >> 20, m.total >> 20, m.cached >> 20, m.modified >> 20, m.free >> 20, m.swap_used >> 20, m.swap_total >> 20);
        println!("disk read={:.0} write={:.0} disks={:?}", d.read, d.write, d.disks.iter().map(|x| &x.name).collect::<Vec<_>>());
        for v in &d.volumes { println!("  vol {} {} {} {}GB free of {}GB", v.mount, v.label, v.fs, v.free >> 30, v.total >> 30); }
        for x in &g { println!("gpu {} util={:.1} temp={:?} fan={:?} power={:?} freq={:?} engines={:?} ded={}MB shared={}MB", x.name, x.util, x.temp_c, x.fans_rpm, x.power_w, x.freq_mhz, x.engines, x.dedicated_used >> 20, x.shared_used >> 20); }
        println!("top mem {:?}", p.by_mem.iter().map(|r| (&r.name, r.count, r.mem >> 20)).collect::<Vec<_>>());
        println!("system {:?}", { let s = system_info(); (s.hostname, s.os) });
        let mut n = net::NetCollector::new();
        n.sample();
        for i in n.sample().interfaces { println!("net {} [{}] {:?} gw={:?} wifi={:?}", i.name, i.kind, i.ipv4, i.gateway, i.wifi.map(|w| (w.ssid, w.signal, w.phy, w.channel))); }
    }
}
