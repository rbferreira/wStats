//! Data shapes shared by the sampler, the taskbar strip and the popup UI.
//! Field names serialize as camelCase; the TypeScript side mirrors them in `ui/types.ts`.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Unix time of the sample, in milliseconds.
    pub ts: u64,
    pub cpu: Cpu,
    pub mem: Mem,
    pub net: Net,
    pub disk: Disk,
    pub gpus: Vec<Gpu>,
    /// `None` on machines without an NPU.
    pub npu: Option<Npu>,
    pub battery: Option<Battery>,
    pub sensors: Sensors,
    pub system: SystemInfo,
    /// Only filled while a popup is open: walking every process is the most expensive
    /// thing the sampler does, and nobody looks at it otherwise.
    pub procs: Option<Procs>,
}

impl Snapshot {
    /// The temperature to show first: the CPU package, else the hottest sensor.
    pub fn headline_temp(&self) -> Option<f32> {
        self.cpu.temp_c.or_else(|| self.sensors.hottest())
    }
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Cpu {
    pub name: String,
    /// 0-100, same definition as Task Manager ("% Processor Utility").
    pub total: f32,
    pub user: f32,
    pub system: f32,
    pub cores: Vec<f32>,
    /// Efficiency class per logical core: 0 = efficiency core, higher = performance.
    /// All equal on CPUs without a hybrid design.
    pub core_classes: Vec<u8>,
    pub physical_cores: u32,
    /// Package temperature, from the sensor service when it runs.
    pub temp_c: Option<f32>,
    pub freq_mhz: f32,
    pub base_mhz: f32,
    pub processes: u32,
    pub threads: u32,
    pub handles: u32,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Mem {
    pub total: u64,
    pub installed: u64,
    pub used: u64,
    pub available: u64,
    /// Standby list: cached pages that are reclaimed on demand.
    pub cached: u64,
    pub modified: u64,
    pub free: u64,
    pub committed: u64,
    pub commit_limit: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub used_pct: f32,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Net {
    /// Bytes per second, summed over physical interfaces.
    pub up: f64,
    pub down: f64,
    /// Bytes moved since wStats started.
    pub session_up: u64,
    pub session_down: u64,
    /// Round trip to `ping_host`, `None` when it did not answer.
    pub ping_ms: Option<u32>,
    pub ping_host: String,
    pub interfaces: Vec<NetIf>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetIf {
    pub name: String,
    pub description: String,
    /// "wifi", "ethernet", "cellular" or "other".
    pub kind: &'static str,
    pub connected: bool,
    pub up: f64,
    pub down: f64,
    pub total_up: u64,
    pub total_down: u64,
    pub link_bps: u64,
    pub mac: String,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub wifi: Option<Wifi>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Wifi {
    /// Empty when Windows withholds it (location permission off).
    pub ssid: String,
    pub bssid: String,
    pub signal: u32,
    pub rx_mbps: f32,
    pub tx_mbps: f32,
    pub phy: String,
    pub channel: u32,
    /// Signal strength in dBm.
    pub rssi: Option<i32>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub read: f64,
    pub write: f64,
    /// Percent of time the busiest disk was active.
    pub active: f32,
    pub disks: Vec<PhysDisk>,
    pub volumes: Vec<Volume>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PhysDisk {
    pub name: String,
    pub read: f64,
    pub write: f64,
    pub active: f32,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    pub mount: String,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub name: String,
    pub luid: String,
    pub util: f32,
    /// Utilization per engine type ("3D", "Copy", "VideoDecode", ...), Task Manager style.
    pub engines: Vec<(String, f32)>,
    pub dedicated_used: u64,
    pub dedicated_total: u64,
    pub shared_used: u64,
    pub shared_total: u64,
    /// From the kernel graphics driver (what Task Manager shows); `None` when the driver
    /// does not report it.
    pub temp_c: Option<f32>,
    pub fan_rpm: Option<u32>,
    /// Current 3D engine clock.
    pub freq_mhz: Option<f32>,
    /// Board power (NVIDIA, through NVML).
    pub power_w: Option<f32>,
    /// Each fan, in RPM (NVIDIA, through NVML); 0 while stopped.
    pub fans_rpm: Vec<u32>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Npu {
    pub name: String,
    pub luid: String,
    /// 0-100: every process's share of it, summed (Task Manager's figure).
    pub util: f32,
    pub shared_used: u64,
    /// Who is using it right now, busiest first.
    pub procs: Vec<NpuProc>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct NpuProc {
    pub name: String,
    pub exe: String,
    pub util: f32,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Battery {
    pub percent: f32,
    pub charging: bool,
    pub plugged: bool,
    /// Seconds until empty while discharging, when Windows has an estimate.
    pub secs_left: Option<u32>,
    pub saver: bool,
    /// Full-charge capacity as a percent of the design capacity.
    pub health: Option<f32>,
    pub cycles: Option<u32>,
    pub design_mwh: Option<u32>,
    pub full_mwh: Option<u32>,
    /// Power flowing into (+) or out of (-) the battery, in mW.
    pub rate_mw: Option<i32>,
    pub voltage_mv: Option<u32>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Sensors {
    /// Watts.
    pub power: Vec<Reading>,
    /// Degrees Celsius.
    pub temps: Vec<Reading>,
    /// RPM, minus the fans hidden in Settings.
    pub fans: Vec<Reading>,
    /// Every fan found, hidden ones included (for Settings).
    pub all_fans: Vec<Reading>,
    /// The fan the taskbar shows, chosen in Settings; `None` = the fastest.
    pub bar_fan: Option<String>,
    /// The firmware's thermal mode (Dell), when the sensor service can read it.
    pub thermal_mode: Option<ThermalMode>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct ThermalMode {
    pub current: String,
    pub options: Vec<String>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub id: String,
    pub label: String,
    pub value: f32,
}

impl Sensors {
    /// The headline power figure: the whole platform if reported, else the CPU package.
    /// GPU readings follow the CPU's and count only when nothing else is reported.
    pub fn package_watts(&self) -> Option<f32> {
        self.power.iter().find(|r| r.id.starts_with("RAPL")).or(self.power.first()).map(|r| r.value)
    }

    pub fn hottest(&self) -> Option<f32> {
        self.temps.iter().map(|r| r.value).fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
    }

    /// The taskbar's fan: the one chosen in Settings, else the fastest, in RPM.
    pub fn bar_fan_rpm(&self) -> Option<f32> {
        if let Some(r) = self.bar_fan.as_ref().and_then(|id| self.fans.iter().find(|r| &r.id == id)) {
            return Some(r.value);
        }
        self.fans.iter().map(|r| r.value).fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
    }

    /// Leaves out the hidden fans and remembers the taskbar's choice.
    pub fn apply_fan_prefs(&mut self, hidden: &[String], bar: Option<&String>) {
        self.all_fans = self.fans.clone();
        self.fans.retain(|r| !hidden.contains(&r.id));
        self.bar_fan = bar.filter(|id| self.fans.iter().any(|r| &r.id == *id)).cloned();
    }
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub uptime_secs: u64,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Procs {
    pub by_cpu: Vec<ProcRow>,
    pub by_mem: Vec<ProcRow>,
    pub by_disk: Vec<ProcRow>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcRow {
    pub name: String,
    pub exe: String,
    /// Number of processes grouped under this row (Chrome, Edge, ... spawn dozens).
    pub count: u32,
    /// Percent of the whole machine, not of one core.
    pub cpu: f32,
    pub mem: u64,
    /// Bytes per second.
    pub disk_read: f64,
    pub disk_write: f64,
}

/// Fixed-capacity series of samples, oldest first.
#[derive(Clone)]
pub struct Ring {
    cap: usize,
    data: VecDeque<f32>,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self { cap, data: VecDeque::with_capacity(cap) }
    }

    pub fn push(&mut self, v: f32) {
        if self.data.len() == self.cap {
            self.data.pop_front();
        }
        self.data.push_back(v);
    }

    pub fn last_n(&self, n: usize) -> impl Iterator<Item = f32> + '_ {
        self.data.iter().skip(self.data.len().saturating_sub(n)).copied()
    }
}

impl Serialize for Ring {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.data.iter())
    }
}

/// Recent history at one-second resolution, enough to draw every popup graph.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub cpu_user: Ring,
    pub cpu_system: Ring,
    pub cores: Vec<Ring>,
    pub mem: Ring,
    pub net_up: Ring,
    pub net_down: Ring,
    pub disk_read: Ring,
    pub disk_write: Ring,
    pub gpus: Vec<Ring>,
    pub npu: Ring,
    pub battery: Ring,
    /// 1 while charging, 0 otherwise; colours the battery graph.
    pub battery_charging: Ring,
    /// Headline power draw in watts.
    pub power: Ring,
    /// Headline temperature (°C): the CPU package, else the hottest sensor.
    pub temp: Ring,
    /// Fastest fan, RPM.
    pub fan: Ring,
}

pub const HISTORY_LEN: usize = 600;

impl History {
    pub fn new() -> Self {
        let r = || Ring::new(HISTORY_LEN);
        Self {
            cpu_user: r(),
            cpu_system: r(),
            cores: Vec::new(),
            mem: r(),
            net_up: r(),
            net_down: r(),
            disk_read: r(),
            disk_write: r(),
            gpus: Vec::new(),
            npu: r(),
            battery: r(),
            battery_charging: r(),
            power: r(),
            temp: r(),
            fan: r(),
        }
    }

    pub fn record(&mut self, s: &Snapshot) {
        self.cpu_user.push(s.cpu.user);
        self.cpu_system.push(s.cpu.system);
        if self.cores.len() != s.cpu.cores.len() {
            self.cores = vec![Ring::new(HISTORY_LEN); s.cpu.cores.len()];
        }
        for (ring, v) in self.cores.iter_mut().zip(&s.cpu.cores) {
            ring.push(*v);
        }
        self.mem.push(s.mem.used_pct);
        self.net_up.push(s.net.up as f32);
        self.net_down.push(s.net.down as f32);
        self.disk_read.push(s.disk.read as f32);
        self.disk_write.push(s.disk.write as f32);
        if self.gpus.len() != s.gpus.len() {
            self.gpus = vec![Ring::new(HISTORY_LEN); s.gpus.len()];
        }
        for (ring, g) in self.gpus.iter_mut().zip(&s.gpus) {
            ring.push(g.util);
        }
        if let Some(n) = &s.npu {
            self.npu.push(n.util);
        }
        if let Some(w) = s.sensors.package_watts() {
            self.power.push(w);
        }
        if let Some(t) = s.headline_temp() {
            self.temp.push(t);
        }
        if let Some(f) = s.sensors.bar_fan_rpm() {
            self.fan.push(f);
        }
        if let Some(b) = &s.battery {
            self.battery.push(b.percent);
            self.battery_charging.push(if b.charging { 1.0 } else { 0.0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_drops_oldest_when_full() {
        let mut r = Ring::new(3);
        for v in 1..=5 {
            r.push(v as f32);
        }
        assert_eq!(r.last_n(3).collect::<Vec<_>>(), vec![3.0, 4.0, 5.0]);
        assert_eq!(r.last_n(2).collect::<Vec<_>>(), vec![4.0, 5.0]);
        assert_eq!(r.last_n(10).count(), 3);
    }

    #[test]
    fn history_follows_core_count() {
        let mut h = History::new();
        let mut s = Snapshot::default();
        s.cpu.cores = vec![10.0, 20.0];
        h.record(&s);
        s.cpu.cores = vec![1.0, 2.0, 3.0];
        h.record(&s);
        assert_eq!(h.cores.len(), 3);
        assert_eq!(h.cores[2].last_n(9).collect::<Vec<_>>(), vec![3.0]);
    }
}
