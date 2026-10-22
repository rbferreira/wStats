//! 24 hours of one-minute averages, kept on disk so the history survives restarts.

use super::model::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

pub const MINUTES: u64 = 24 * 60;

/// One minute, averaged. Field names are short on purpose: the file holds 1440 of them.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Minute {
    /// Minutes since the Unix epoch.
    pub t: u64,
    pub cu: f32,
    pub cs: f32,
    pub mem: f32,
    pub nu: f32,
    pub nd: f32,
    pub dr: f32,
    pub dw: f32,
    pub gpu: f32,
    /// Battery percent; `None` when there is no battery.
    pub bat: Option<f32>,
    /// Fraction of the minute spent charging.
    pub chg: f32,
    /// Headline power draw in watts, when the machine reports one.
    #[serde(default)]
    pub pw: Option<f32>,
    /// Headline temperature, °C.
    #[serde(default)]
    pub tc: Option<f32>,
    /// Fastest fan, RPM.
    #[serde(default)]
    pub fan: Option<f32>,
    /// NPU usage; `None` without an NPU.
    #[serde(default)]
    pub npu: Option<f32>,
}

#[derive(Default)]
struct Acc {
    t: u64,
    n: u32,
    sum: Minute,
    bat_n: u32,
    pw_n: u32,
    tc_n: u32,
    fan_n: u32,
    npu_n: u32,
}

pub struct LongHistory {
    minutes: VecDeque<Minute>,
    acc: Acc,
    path: Option<PathBuf>,
}

impl LongHistory {
    pub fn load(path: Option<PathBuf>) -> Self {
        let minutes = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Vec<Minute>>(&b).ok())
            .unwrap_or_default();
        let mut me = Self { minutes: minutes.into(), acc: Acc::default(), path };
        me.trim(now_minute());
        me
    }

    /// Adds one sample; closes the minute when the clock moves past it.
    pub fn record(&mut self, s: &Snapshot) {
        let t = s.ts / 60_000;
        if self.acc.n > 0 && t != self.acc.t {
            self.close();
        }
        let a = &mut self.acc;
        a.t = t;
        a.n += 1;
        a.sum.cu += s.cpu.user;
        a.sum.cs += s.cpu.system;
        a.sum.mem += s.mem.used_pct;
        a.sum.nu += s.net.up as f32;
        a.sum.nd += s.net.down as f32;
        a.sum.dr += s.disk.read as f32;
        a.sum.dw += s.disk.write as f32;
        a.sum.gpu += s.gpus.first().map(|g| g.util).unwrap_or(0.0);
        if let Some(w) = s.sensors.package_watts() {
            a.pw_n += 1;
            a.sum.pw = Some(a.sum.pw.unwrap_or(0.0) + w);
        }
        if let Some(t) = s.headline_temp() {
            a.tc_n += 1;
            a.sum.tc = Some(a.sum.tc.unwrap_or(0.0) + t);
        }
        if let Some(f) = s.sensors.bar_fan_rpm() {
            a.fan_n += 1;
            a.sum.fan = Some(a.sum.fan.unwrap_or(0.0) + f);
        }
        if let Some(n) = &s.npu {
            a.npu_n += 1;
            a.sum.npu = Some(a.sum.npu.unwrap_or(0.0) + n.util);
        }
        if let Some(b) = &s.battery {
            a.bat_n += 1;
            a.sum.bat = Some(a.sum.bat.unwrap_or(0.0) + b.percent);
            a.sum.chg += if b.charging { 1.0 } else { 0.0 };
        }
    }

    fn close(&mut self) {
        let a = std::mem::take(&mut self.acc);
        let n = a.n.max(1) as f32;
        let bn = a.bat_n.max(1) as f32;
        let has_bat = a.bat_n > 0;
        self.minutes.push_back(Minute {
            t: a.t,
            cu: a.sum.cu / n,
            cs: a.sum.cs / n,
            mem: a.sum.mem / n,
            nu: a.sum.nu / n,
            nd: a.sum.nd / n,
            dr: a.sum.dr / n,
            dw: a.sum.dw / n,
            gpu: a.sum.gpu / n,
            bat: has_bat.then(|| a.sum.bat.unwrap_or(0.0) / bn),
            chg: if has_bat { a.sum.chg / bn } else { 0.0 },
            pw: (a.pw_n > 0).then(|| a.sum.pw.unwrap_or(0.0) / a.pw_n as f32),
            tc: (a.tc_n > 0).then(|| a.sum.tc.unwrap_or(0.0) / a.tc_n as f32),
            fan: (a.fan_n > 0).then(|| a.sum.fan.unwrap_or(0.0) / a.fan_n as f32),
            npu: (a.npu_n > 0).then(|| a.sum.npu.unwrap_or(0.0) / a.npu_n as f32),
        });
        self.trim(a.t);
    }

    fn trim(&mut self, now: u64) {
        while self.minutes.front().is_some_and(|m| m.t + MINUTES <= now) {
            self.minutes.pop_front();
        }
    }

    /// The last 24 hours, oldest first. Minutes wStats was not running are absent; the
    /// UI places samples by their timestamp and leaves gaps.
    pub fn minutes(&self) -> Vec<Minute> {
        self.minutes.iter().copied().collect()
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(&self.minutes) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
}

fn now_minute() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 60).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(ms: u64, cpu: f32) -> Snapshot {
        let mut s = Snapshot { ts: ms, ..Default::default() };
        s.cpu.user = cpu;
        s
    }

    #[test]
    fn averages_each_minute() {
        let mut h = LongHistory { minutes: VecDeque::new(), acc: Acc::default(), path: None };
        let base = now_minute() * 60_000;
        h.record(&snap(base, 10.0));
        h.record(&snap(base + 30_000, 30.0));
        h.record(&snap(base + 60_000, 50.0)); // next minute closes the first
        let m = h.minutes();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].cu, 20.0);
        assert_eq!(m[0].bat, None, "no battery reported");
    }

    #[test]
    fn forgets_after_a_day() {
        let mut h = LongHistory { minutes: VecDeque::new(), acc: Acc::default(), path: None };
        let now = now_minute();
        h.minutes.push_back(Minute { t: now - MINUTES - 5, ..Default::default() });
        h.minutes.push_back(Minute { t: now - 10, ..Default::default() });
        h.trim(now);
        assert_eq!(h.minutes.len(), 1);
    }

    #[test]
    fn round_trips_through_json() {
        let m = vec![Minute { t: 5, cu: 1.5, bat: None, ..Default::default() }, Minute { t: 6, bat: Some(80.0), ..Default::default() }];
        let back: Vec<Minute> = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }
}
