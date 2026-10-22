// Plausible fake data so the UI can be built and reviewed in a plain browser,
// without the Rust side. Shaped after the dev machine (Core Ultra 7 268V, Arc 140V).

import { locale } from "./i18n";
import type { History, ProcRow, Snapshot } from "./types";

const GB = 1024 ** 3;
const MB = 1024 ** 2;
const LEN = 600;

let seed = 7;
const rnd = () => {
  seed = (seed * 16807) % 2147483647;
  return seed / 2147483647;
};
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
/** Bursty traffic: mostly quiet, with occasional spikes. */
const bursty = (base: number, spike: number) => (rnd() < 0.12 ? spike * (0.3 + rnd()) : base * rnd());

const proc = (name: string, exe: string, cpu: number, mem: number, r = 0, w = 0, count = 1): ProcRow => ({
  name, exe, count, cpu, mem, diskRead: r, diskWrite: w,
});

export function mockState(): { snapshot: Snapshot; history: History } {
  // Names in the page's language, so screenshots read the same as the interface.
  const pt = (document.documentElement.lang || locale).startsWith("pt");
  const tr = (p: string, e: string) => (pt ? p : e);
  const history: History = {
    cpuUser: [], cpuSystem: [], cores: Array.from({ length: 8 }, () => []), mem: [],
    netUp: [], netDown: [], diskRead: [], diskWrite: [], gpus: [[]], npu: [], battery: [], batteryCharging: [], power: [], temp: [], fan: [],
  };
  let user = 12, sys = 6, batt = 64;
  for (let i = 0; i < LEN; i++) {
    user = clamp(user + (rnd() - 0.5) * 8 + (rnd() < 0.03 ? 30 : 0), 2, 70) * 0.98 + 0.2;
    sys = clamp(sys + (rnd() - 0.5) * 4, 1, 25);
    history.cpuUser.push(user);
    history.cpuSystem.push(sys);
    history.cores.forEach((c, k) => c.push(clamp(user + sys + (rnd() - 0.5) * 30 - (k >= 4 ? 8 : 0), 0, 100)));
    history.mem.push(61 + Math.sin(i / 90) * 3);
    history.netUp.push(bursty(4 * 1024, 900 * 1024));
    history.netDown.push(bursty(12 * 1024, 4 * MB));
    history.diskRead.push(bursty(60 * 1024, 30 * MB));
    history.diskWrite.push(bursty(200 * 1024, 12 * MB));
    history.gpus[0].push(clamp(8 + Math.sin(i / 20) * 6 + rnd() * 10, 0, 100));
    // The NPU works in bursts: idle, then a while of steady inference.
    history.npu.push(i > 550 ? clamp(55 + (rnd() - 0.5) * 30, 0, 100) : rnd() < 0.05 ? 20 * rnd() : 0);
    const charging = i > 380 && i < 520 ? 1 : 0;
    batt = clamp(batt + (charging ? 0.12 : -0.04), 5, 100);
    history.battery.push(batt);
    history.batteryCharging.push(charging);
    history.power.push(6 + (user + sys) / 6 + rnd() * 2);
    history.temp.push(55 + (user + sys) / 3 + rnd() * 3);
    history.fan.push(2600 + (user + sys) * 40 + rnd() * 150);
  }
  const last = <T>(a: T[]) => a[a.length - 1];

  const snapshot: Snapshot = {
    ts: Date.now(),
    cpu: {
      name: "Intel(R) Core(TM) Ultra 7 268V",
      total: last(history.cpuUser) + last(history.cpuSystem),
      user: last(history.cpuUser),
      system: last(history.cpuSystem),
      cores: history.cores.map(last),
      coreClasses: [1, 1, 1, 1, 0, 0, 0, 0],
      physicalCores: 8,
      freqMhz: 2180,
      tempC: 72,
      baseMhz: 2200,
      processes: 312,
      threads: 4812,
      handles: 168_220,
    },
    mem: {
      total: 31.6 * GB, installed: 32 * GB, used: 19.3 * GB, available: 12.3 * GB,
      cached: 9.1 * GB, modified: 420 * MB, free: 3.2 * GB,
      committed: 24.8 * GB, commitLimit: 36.3 * GB, pagedPool: 1.1 * GB, nonpagedPool: 612 * MB,
      swapUsed: 322 * MB, swapTotal: 4.7 * GB, usedPct: last(history.mem),
    },
    net: {
      up: last(history.netUp), down: last(history.netDown),
      sessionUp: 182 * MB, sessionDown: 2.4 * GB,
      pingMs: 11, pingHost: "1.1.1.1",
      interfaces: [{
        name: "Wi-Fi", description: "Intel(R) Wi-Fi 7 BE201 320MHz", kind: "wifi", connected: true,
        up: last(history.netUp), down: last(history.netDown), totalUp: 3.1 * GB, totalDown: 28 * GB,
        linkBps: 2_882_000_000, mac: "A4:C3:F0:12:9B:7E",
        ipv4: ["192.168.1.48"], ipv6: ["2001:db8:85a3::8a2e:370:7334"],
        gateway: "192.168.1.1", dns: ["192.168.1.1"],
        wifi: { ssid: tr("Casa", "Home"), bssid: "", signal: 88, rxMbps: 2882, txMbps: 2401, phy: "Wi-Fi 7", channel: 37, rssi: -41 },
      }],
    },
    disk: {
      read: last(history.diskRead), write: last(history.diskWrite), active: 4,
      disks: [{ name: "0 C:", read: 0, write: 0, active: 4 }],
      volumes: [
        { mount: "C:", label: "Windows", fs: "NTFS", total: 951 * GB, free: 412.6 * GB, removable: false },
        { mount: "D:", label: "Backup", fs: "exFAT", total: 1863 * GB, free: 1290 * GB, removable: true },
      ],
    },
    gpus: [{
      name: "Intel(R) Arc(TM) 140V GPU (16GB)", luid: "0x00000000_0x0000d1a3", util: last(history.gpus[0]),
      engines: [["3D", last(history.gpus[0])], ["VideoDecode", 3.1], ["Copy", 0.4], ["Compute", 0]],
      dedicatedUsed: 0, dedicatedTotal: 128 * MB, sharedUsed: 1.8 * GB, sharedTotal: 15.8 * GB,
      tempC: 52, fanRpm: null, freqMhz: 1950, powerW: null, fansRpm: [],
    }],
    npu: {
      name: "Intel(R) AI Boost", luid: "0x00000000_0x000121f0", util: last(history.npu), sharedUsed: 1.8 * GB,
      procs: [
        { name: "Windows Studio Effects", exe: "VideoEffects.exe", util: 48 },
        { name: "WorkloadsSessionHost", exe: "WorkloadsSessionHost.exe", util: 16 },
      ],
    },
    battery: { percent: Math.round(batt), charging: false, plugged: false, secsLeft: 4 * 3600 + 23 * 60, saver: false,
      health: 91.7, cycles: 39, designMwh: 58898, fullMwh: 54019, rateMw: -7200, voltageMv: 12914 },
    sensors: {
      power: [
        { id: "RAPL_Package0_PKG", label: tr("Pacote da CPU", "CPU package"), value: 12.6 },
        { id: "RAPL_Package0_PP0", label: tr("Núcleos da CPU", "CPU cores"), value: 7.4 },
        { id: "RAPL_Package0_PP1", label: tr("Gráficos integrados", "Integrated graphics"), value: 0.3 },
        { id: "RAPL_Package0_DRAM", label: tr("Memória", "Memory"), value: 0.2 },
      ],
      temps: [
        { id: "cpu-package", label: tr("CPU (pacote)", "CPU (package)"), value: 72 },
        { id: "board-0", label: tr("CPU (placa)", "CPU (board)"), value: 57 },
        { id: "board-1", label: tr("Memória 1", "Memory 1"), value: 48 },
        { id: "board-2", label: tr("Ambiente", "Ambient"), value: 37 },
        { id: "cpu-core-0", label: tr("Núcleo 1", "Core 1"), value: 69 },
        { id: "cpu-core-1", label: tr("Núcleo 2", "Core 2"), value: 74 },
      ],
      fans: [
        { id: "fan-0", label: "CPU", value: 3775 },
        { id: "fan-1", label: "GPU", value: 3761 },
      ],
      allFans: [
        { id: "fan-0", label: "CPU", value: 3775 },
        { id: "fan-1", label: "GPU", value: 3761 },
      ],
      barFan: null,
      thermalMode: { current: "Optimized", options: ["Optimized", "Cool", "Quiet", "UltraPerformance"] },
    },
    system: { hostname: "DESKTOP-WSTATS", os: "Windows 11 Pro 25H2 (build 26300)", uptimeSecs: 18 * 86400 + 6 * 3600 + 120 },
    procs: {
      byCpu: [
        proc("Google Chrome", "chrome.exe", 5.8, 3.0 * GB, 0, 0, 31),
        proc("Spotify", "Spotify.exe", 3.2, 851 * MB, 0, 0, 6),
        proc(tr("Explorador de Arquivos", "File Explorer"), "explorer.exe", 2.2, 310 * MB),
        proc(tr("Gerenciador de Janelas da Área de Trabalho", "Desktop Window Manager"), "dwm.exe", 1.0, 180 * MB),
        proc("Windows Defender", "MsMpEng.exe", 0.9, 240 * MB),
      ],
      byMem: [
        proc("Google Chrome", "chrome.exe", 5.8, 3.0 * GB, 0, 0, 31),
        proc("Visual Studio Code", "Code.exe", 0.4, 1.7 * GB, 0, 0, 12),
        proc("Spotify", "Spotify.exe", 3.2, 851 * MB, 0, 0, 6),
        proc("Microsoft Teams", "ms-teams.exe", 0.2, 549 * MB, 0, 0, 4),
        proc("WSL", "vmmemWSL", 0.1, 501 * MB),
      ],
      byDisk: [
        proc(tr("Sistema", "System"), "System", 0.3, 4 * MB, 8.8 * MB, 8.8 * MB),
        proc("Windows Defender", "MsMpEng.exe", 0.9, 240 * MB, 140 * 1024, 822 * 1024),
        proc("OneDrive", "OneDrive.exe", 0.1, 120 * MB, 0, 258 * 1024),
        proc("Google Chrome", "chrome.exe", 5.8, 3.0 * GB, 9 * 1024, 72 * 1024, 31),
        proc("Windows Search", "SearchIndexer.exe", 0.0, 60 * MB, 37 * 1024, 0),
      ],
    },
  };
  return { snapshot, history };
}

/** Advances the fake machine by one second, in place. */
export function mockTick(s: { snapshot: Snapshot; history: History }) {
  const h = s.history;
  const push = (a: number[], v: number) => {
    a.push(v);
    if (a.length > LEN) a.shift();
  };
  const user = clamp(h.cpuUser[h.cpuUser.length - 1] + (rnd() - 0.5) * 10, 2, 70);
  const sys = clamp(h.cpuSystem[h.cpuSystem.length - 1] + (rnd() - 0.5) * 4, 1, 25);
  push(h.cpuUser, user);
  push(h.cpuSystem, sys);
  h.cores.forEach((c, k) => push(c, clamp(user + sys + (rnd() - 0.5) * 30 - (k >= 4 ? 8 : 0), 0, 100)));
  push(h.mem, 61 + rnd() * 2);
  push(h.netUp, bursty(4 * 1024, 900 * 1024));
  push(h.netDown, bursty(12 * 1024, 4 * MB));
  push(h.diskRead, bursty(60 * 1024, 30 * MB));
  push(h.diskWrite, bursty(200 * 1024, 12 * MB));
  push(h.gpus[0], clamp(h.gpus[0][h.gpus[0].length - 1] + (rnd() - 0.5) * 8, 0, 100));
  push(h.npu, clamp(h.npu[h.npu.length - 1] + (rnd() - 0.5) * 20, 0, 100));
  push(h.battery, h.battery[h.battery.length - 1]);
  push(h.batteryCharging, 0);
  const pw = 6 + (user + sys) / 6 + rnd() * 2;
  push(h.power, pw);
  s.snapshot.sensors.power[0].value = pw;

  const sn = s.snapshot;
  sn.ts = Date.now();
  sn.cpu.user = user;
  sn.cpu.system = sys;
  sn.cpu.total = user + sys;
  sn.cpu.cores = h.cores.map((c) => c[c.length - 1]);
  sn.cpu.freqMhz = 1800 + rnd() * 1200;
  sn.net.up = h.netUp[h.netUp.length - 1];
  sn.net.down = h.netDown[h.netDown.length - 1];
  sn.net.pingMs = 9 + Math.round(rnd() * 6);
  sn.disk.read = h.diskRead[h.diskRead.length - 1];
  sn.disk.write = h.diskWrite[h.diskWrite.length - 1];
  sn.gpus[0].util = h.gpus[0][h.gpus[0].length - 1];
  sn.gpus[0].engines[0][1] = sn.gpus[0].util;
  if (sn.npu) sn.npu.util = h.npu[h.npu.length - 1];
  sn.mem.usedPct = h.mem[h.mem.length - 1];
  sn.system.uptimeSecs += 1;
}
