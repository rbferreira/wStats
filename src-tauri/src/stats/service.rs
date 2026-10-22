//! Readings from the wStats sensor service (sensors/ in this repo), which runs as
//! SYSTEM and publishes JSON in a shared-memory section any user can read. Absent,
//! stopped or stale service: no readings, and the rest of wStats is unaffected.
//!
//! Layout (mirrors sensors/src/publish.rs): magic u32, seq u32 (seqlock: odd while
//! writing), len u32, reserved u32, then `len` bytes of JSON.

use super::model::{Reading, Sensors, ThermalMode};
use serde::Deserialize;
use std::sync::atomic::{AtomicU32, Ordering, fence};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile};
use windows::core::w;

const MAGIC: u32 = 0x5345_5357;
const HEADER: usize = 16;
const SIZE: usize = 64 * 1024;
/// A sample older than this means the service stopped.
const STALE: Duration = Duration::from_secs(5);

#[derive(Deserialize, Default, Debug)]
pub struct Payload {
    pub t: u64,
    pub cpu: Option<CpuTemps>,
    /// CPU power by RAPL domain (service v2+).
    #[serde(default)]
    pub power: Vec<Power>,
    /// The motherboard's Super I/O chip (service v2+).
    #[serde(default)]
    pub board: Option<Board>,
    #[serde(default)]
    pub fans: Vec<Fan>,
    #[serde(default)]
    pub thermals: Vec<Thermal>,
    #[serde(default)]
    pub thermal_mode: Option<ThermalMode>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Deserialize, Default, Debug)]
pub struct CpuTemps {
    pub vendor: String,
    pub package: Option<f32>,
    #[serde(default)]
    pub cores: Vec<f32>,
}

#[derive(Deserialize, Debug)]
pub struct Power {
    pub domain: String,
    pub watts: f32,
}

#[derive(Deserialize, Default, Debug)]
pub struct Board {
    #[serde(default)]
    pub temps: Vec<BoardTemp>,
    #[serde(default)]
    pub fans: Vec<BoardFan>,
}

#[derive(Deserialize, Debug)]
pub struct BoardTemp {
    pub key: String,
    pub celsius: f32,
}

#[derive(Deserialize, Debug)]
pub struct BoardFan {
    pub key: String,
    /// Stable header number (service 0.2+ builds after the first).
    #[serde(default)]
    pub header: Option<u8>,
    pub rpm: u32,
}

#[derive(Deserialize, Debug)]
pub struct Fan {
    pub kind: u8,
    pub rpm: u16,
}

#[derive(Deserialize, Debug)]
pub struct Thermal {
    pub kind: u8,
    pub celsius: i8,
}

pub struct Reader {
    map: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// Only read, from the sampler thread.
unsafe impl Send for Reader {}

impl Reader {
    /// `None` while the service is not running.
    pub fn open() -> Option<Self> {
        unsafe {
            let map = OpenFileMappingW(FILE_MAP_READ.0, false, w!(r"Global\wStats.Sensors.v1")).ok()?;
            let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, SIZE);
            if view.Value.is_null() {
                let _ = CloseHandle(map);
                return None;
            }
            Some(Self { map, view })
        }
    }

    fn base(&self) -> *const u8 {
        self.view.Value as *const u8
    }

    /// The latest sample, if fresh.
    pub fn read(&self) -> Option<Payload> {
        unsafe {
            if (self.base() as *const u32).read_volatile() != MAGIC {
                return None;
            }
            let seq = &*(self.base().add(4) as *const AtomicU32);
            for _ in 0..4 {
                let before = seq.load(Ordering::Acquire);
                if before & 1 == 1 {
                    std::thread::yield_now();
                    continue;
                }
                let len = ((self.base().add(8) as *const u32).read_volatile() as usize).min(SIZE - HEADER);
                let bytes = std::slice::from_raw_parts(self.base().add(HEADER), len).to_vec();
                fence(Ordering::Acquire);
                if seq.load(Ordering::Relaxed) != before {
                    continue;
                }
                if len == 0 {
                    return None;
                }
                let payload: Payload = serde_json::from_slice(&bytes).ok()?;
                let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
                return (now.saturating_sub(payload.t) < STALE.as_millis() as u64).then_some(payload);
            }
            None
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.map);
        }
    }
}

/// Dell's thermal sensor types (as in Linux's dell-wmi-ddv).
fn thermal_name(kind: u8, pt: bool) -> &'static str {
    let tr = |a, b| if pt { a } else { b };
    match kind {
        0x00 => tr("CPU (placa)", "CPU (board)"),
        0x11 => tr("GPU (placa)", "GPU (board)"),
        0x22 => tr("Memória", "Memory"),
        0x44 => tr("Ambiente", "Ambient"),
        _ => tr("Placa", "Board"),
    }
}

fn fan_name(kind: u8, pt: bool) -> &'static str {
    let tr = |a, b| if pt { a } else { b };
    match kind {
        0 => "CPU",
        1 => tr("Gabinete", "Chassis"),
        2 => "GPU",
        3 => tr("Fonte", "Power supply"),
        4 => "Chipset",
        5 => tr("Memória", "Memory"),
        6 => "PCI",
        7 => tr("Disco", "Disk"),
        _ => tr("Ventoinha", "Fan"),
    }
}

/// Super I/O temperature inputs.
fn board_temp_name(key: &str, pt: bool) -> &'static str {
    let tr = |a, b| if pt { a } else { b };
    match key {
        "board" => tr("Placa-mãe", "Motherboard"),
        "cpu-socket" => tr("CPU (soquete)", "CPU (socket)"),
        "cpu" => tr("CPU (placa)", "CPU (board)"),
        "vrm" => "VRM",
        "pch" => "Chipset",
        "pcie" => "PCIe",
        "m2" => "M.2",
        _ => tr("Auxiliar", "Auxiliary"),
    }
}

/// Super I/O fan headers.
fn board_fan_name(key: &str, pt: bool) -> &'static str {
    let tr = |a, b| if pt { a } else { b };
    match key {
        "cpu" => "CPU",
        "pump" => tr("Bomba", "Pump"),
        "sys" => tr("Gabinete", "Chassis"),
        _ => tr("Auxiliar", "Auxiliary"),
    }
}

/// Labels with a running number for names that repeat ("Memory 1", "Memory 2").
fn numbered<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Vec<String> {
    let mut seen = std::collections::HashMap::<&str, usize>::new();
    names
        .clone()
        .map(|n| {
            let total = names.clone().filter(|m| *m == n).count();
            let i = seen.entry(n).or_insert(0);
            *i += 1;
            if total > 1 { format!("{n} {i}") } else { n.to_string() }
        })
        .collect()
}

/// Adds the service's readings to `sensors`: CPU package first, then the board's
/// sensors, then each core; fans before the GPU's. Returns the package temperature.
pub fn merge(p: &Payload, sensors: &mut Sensors) -> Option<f32> {
    merge_in(p, sensors, crate::lang::pt())
}

fn merge_in(p: &Payload, sensors: &mut Sensors, pt: bool) -> Option<f32> {
    let tr = |a, b| if pt { a } else { b };
    let mut temps = Vec::new();
    let package = p.cpu.as_ref().and_then(|c| c.package);
    if let (Some(cpu), Some(t)) = (&p.cpu, package) {
        let label = if cpu.vendor == "amd" { "CPU (Tctl)" } else { tr("CPU (pacote)", "CPU (package)") };
        temps.push(Reading { id: "cpu-package".into(), label: label.into(), value: t });
    }
    let names = numbered(p.thermals.iter().map(|t| thermal_name(t.kind, pt)));
    for (i, (t, label)) in p.thermals.iter().zip(names).enumerate() {
        temps.push(Reading { id: format!("board-{i}"), label, value: t.celsius as f32 });
    }
    let board = p.board.as_ref();
    let board_temps = board.map_or(&[][..], |b| &b.temps[..]);
    let names = numbered(board_temps.iter().map(|t| board_temp_name(&t.key, pt)));
    for (i, (t, label)) in board_temps.iter().zip(names).enumerate() {
        temps.push(Reading { id: format!("sio-{i}"), label, value: t.celsius });
    }
    if let Some(cpu) = &p.cpu {
        let unit = if cpu.vendor == "amd" { "CCD" } else { tr("Núcleo", "Core") };
        for (i, t) in cpu.cores.iter().enumerate() {
            temps.push(Reading { id: format!("cpu-core-{i}"), label: format!("{unit} {}", i + 1), value: *t });
        }
    }
    sensors.temps.splice(0..0, temps);

    let names = numbered(p.fans.iter().map(|f| fan_name(f.kind, pt)));
    let fans = p.fans.iter().zip(names).enumerate().map(|(i, (f, label))| Reading { id: format!("fan-{i}"), label, value: f.rpm as f32 });
    let board_fans = board.map_or(&[][..], |b| &b.fans[..]);
    let names = numbered(board_fans.iter().map(|f| board_fan_name(&f.key, pt)));
    let fans = fans.chain(board_fans.iter().zip(names).enumerate().map(|(i, (f, label))| {
        // The header number keeps a fan's id when another header starts spinning.
        let id = f.header.map_or_else(|| format!("sio-fan-{i}"), |h| format!("sio-fan-h{h}"));
        Reading { id, label, value: f.rpm as f32 }
    }));
    sensors.fans.splice(0..0, fans);

    // Windows' own RAPL counters, when present, come first; the service fills in otherwise.
    if sensors.power.is_empty() {
        sensors.power = p
            .power
            .iter()
            .map(|w| {
                // Named like Windows' Energy Meter instances so the panels label them alike.
                let id = format!("RAPL_Package0_{}", w.domain);
                let label = super::sensors::domain_label(&id, pt).map_or_else(|| w.domain.clone(), str::to_string);
                Reading { id, label, value: w.watts }
            })
            .collect();
    }
    sensors.thermal_mode = p.thermal_mode.clone();
    package
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_in_display_order() {
        let p: Payload = serde_json::from_str(
            r#"{"v":1,"t":0,"cpu":{"vendor":"intel","package":78,"cores":[69,72],"tjmax":100},
                "fans":[{"kind":0,"rpm":3775},{"kind":2,"rpm":3761}],
                "thermals":[{"kind":0,"celsius":57},{"kind":34,"celsius":48},{"kind":34,"celsius":35}],"notes":[]}"#,
        )
        .unwrap();
        let mut s = Sensors::default();
        assert_eq!(merge_in(&p, &mut s, true), Some(78.0));
        let labels: Vec<_> = s.temps.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["CPU (pacote)", "CPU (placa)", "Memória 1", "Memória 2", "Núcleo 1", "Núcleo 2"]);
        let fans: Vec<_> = s.fans.iter().map(|r| (r.label.as_str(), r.value)).collect();
        assert_eq!(fans, [("CPU", 3775.0), ("GPU", 3761.0)]);
    }

    #[test]
    fn labels_follow_the_language() {
        let p: Payload = serde_json::from_str(
            r#"{"t":0,"cpu":{"vendor":"intel","package":70,"cores":[65]},"fans":[{"kind":1,"rpm":900}],
                "thermals":[{"kind":0,"celsius":50},{"kind":68,"celsius":30}]}"#,
        )
        .unwrap();
        let mut s = Sensors::default();
        merge_in(&p, &mut s, false);
        let labels: Vec<_> = s.temps.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["CPU (package)", "CPU (board)", "Ambient", "Core 1"]);
        assert_eq!(s.fans[0].label, "Chassis");
    }

    #[test]
    fn merges_desktop_board_and_power() {
        let p: Payload = serde_json::from_str(
            r#"{"v":2,"t":0,"cpu":{"vendor":"amd","package":57.5,"cores":[55.5]},
                "power":[{"domain":"PKG","watts":31.5}],
                "board":{"chip":"NCT6779D","temps":[{"key":"board","celsius":34},{"key":"cpu-socket","celsius":41},{"key":"aux","celsius":30},{"key":"aux","celsius":28}],
                         "fans":[{"key":"sys","rpm":820},{"key":"cpu","rpm":1150},{"key":"aux","rpm":0}]},
                "fans":[],"thermals":[]}"#,
        )
        .unwrap();
        let mut s = Sensors::default();
        assert_eq!(merge_in(&p, &mut s, false), Some(57.5));
        let labels: Vec<_> = s.temps.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["CPU (Tctl)", "Motherboard", "CPU (socket)", "Auxiliary 1", "Auxiliary 2", "CCD 1"]);
        let fans: Vec<_> = s.fans.iter().map(|r| (r.label.as_str(), r.value)).collect();
        assert_eq!(fans, [("Chassis", 820.0), ("CPU", 1150.0), ("Auxiliary", 0.0)]);
        assert_eq!(s.power.len(), 1);
        assert_eq!((s.power[0].id.as_str(), s.power[0].label.as_str(), s.power[0].value), ("RAPL_Package0_PKG", "CPU package", 31.5));
    }

    #[test]
    fn hidden_fans_and_the_taskbar_choice() {
        let fan = |id: &str, v| Reading { id: id.into(), label: id.into(), value: v };
        let mut s = Sensors { fans: vec![fan("sio-fan-h1", 2300.0), fan("sio-fan-h2", 760.0), fan("gpu0-fan0", 0.0)], ..Default::default() };
        s.apply_fan_prefs(&["gpu0-fan0".to_string()], Some(&"sio-fan-h2".to_string()));
        assert_eq!(s.all_fans.len(), 3);
        assert_eq!(s.fans.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sio-fan-h1", "sio-fan-h2"]);
        assert_eq!(s.bar_fan_rpm(), Some(760.0));
        // A hidden fan cannot be the taskbar's: back to the fastest.
        s.fans = s.all_fans.clone();
        s.apply_fan_prefs(&["sio-fan-h2".to_string()], Some(&"sio-fan-h2".to_string()));
        assert_eq!(s.bar_fan, None);
        assert_eq!(s.bar_fan_rpm(), Some(2300.0));
    }

    #[test]
    fn board_fans_keep_their_header_id() {
        let p: Payload = serde_json::from_str(r#"{"t":0,"board":{"fans":[{"key":"cpu","header":1,"rpm":2300},{"key":"aux","header":2,"rpm":760}]}}"#).unwrap();
        let mut s = Sensors::default();
        merge_in(&p, &mut s, true);
        assert_eq!(s.fans.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sio-fan-h1", "sio-fan-h2"]);
    }

    #[test]
    fn windows_power_counters_win() {
        let p: Payload = serde_json::from_str(r#"{"t":0,"power":[{"domain":"PKG","watts":30}]}"#).unwrap();
        let mut s = Sensors::default();
        s.power.push(Reading { id: "RAPL_Package0_PKG".into(), label: "CPU package".into(), value: 12.0 });
        merge_in(&p, &mut s, false);
        assert_eq!(s.power.len(), 1);
        assert_eq!(s.power[0].value, 12.0);
    }
}
