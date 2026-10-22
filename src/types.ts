// Mirrors `src-tauri/src/stats/model.rs` (serde camelCase).

export interface Snapshot {
  ts: number;
  cpu: Cpu;
  mem: Mem;
  net: Net;
  disk: Disk;
  gpus: Gpu[];
  /** Null on machines without an NPU. */
  npu: Npu | null;
  battery: Battery | null;
  sensors: Sensors;
  system: SystemInfo;
  procs: Procs | null;
}

export interface Cpu {
  name: string;
  total: number;
  user: number;
  system: number;
  cores: number[];
  coreClasses: number[];
  physicalCores: number;
  freqMhz: number;
  /** Package temperature, from the sensor service. */
  tempC: number | null;
  baseMhz: number;
  processes: number;
  threads: number;
  handles: number;
}

export interface Mem {
  total: number;
  installed: number;
  used: number;
  available: number;
  cached: number;
  modified: number;
  free: number;
  committed: number;
  commitLimit: number;
  pagedPool: number;
  nonpagedPool: number;
  swapUsed: number;
  swapTotal: number;
  usedPct: number;
}

export interface Net {
  up: number;
  down: number;
  sessionUp: number;
  sessionDown: number;
  pingMs: number | null;
  pingHost: string;
  interfaces: NetIf[];
}

export interface NetIf {
  name: string;
  description: string;
  kind: "wifi" | "ethernet" | "cellular" | "other";
  connected: boolean;
  up: number;
  down: number;
  totalUp: number;
  totalDown: number;
  linkBps: number;
  mac: string;
  ipv4: string[];
  ipv6: string[];
  gateway: string | null;
  dns: string[];
  wifi: Wifi | null;
}

export interface Wifi {
  ssid: string;
  bssid: string;
  signal: number;
  rxMbps: number;
  txMbps: number;
  phy: string;
  channel: number;
  rssi: number | null;
}

export interface Disk {
  read: number;
  write: number;
  active: number;
  disks: PhysDisk[];
  volumes: Volume[];
}

export interface PhysDisk {
  name: string;
  read: number;
  write: number;
  active: number;
}

export interface Volume {
  mount: string;
  label: string;
  fs: string;
  total: number;
  free: number;
  removable: boolean;
}

export interface Gpu {
  name: string;
  luid: string;
  util: number;
  engines: [string, number][];
  dedicatedUsed: number;
  dedicatedTotal: number;
  sharedUsed: number;
  sharedTotal: number;
  /** From the driver, when it reports them. */
  tempC: number | null;
  fanRpm: number | null;
  freqMhz: number | null;
  powerW: number | null;
  /** Each fan, in RPM (NVIDIA); 0 while stopped. */
  fansRpm: number[];
}

export interface Npu {
  name: string;
  luid: string;
  util: number;
  sharedUsed: number;
  /** Who is using it right now, busiest first. */
  procs: NpuProc[];
}

export interface NpuProc {
  name: string;
  exe: string;
  util: number;
}

export interface Battery {
  percent: number;
  charging: boolean;
  plugged: boolean;
  secsLeft: number | null;
  saver: boolean;
  health: number | null;
  cycles: number | null;
  designMwh: number | null;
  fullMwh: number | null;
  rateMw: number | null;
  voltageMv: number | null;
}

export interface Reading {
  id: string;
  label: string;
  value: number;
}

export interface ThermalMode {
  current: string;
  options: string[];
}

export interface Sensors {
  power: Reading[];
  temps: Reading[];
  /** Minus the fans hidden in Settings. */
  fans: Reading[];
  /** Every fan found, hidden ones included. */
  allFans: Reading[];
  /** The taskbar's fan; null = the fastest. */
  barFan: string | null;
  thermalMode: ThermalMode | null;
}

export interface SystemInfo {
  hostname: string;
  os: string;
  uptimeSecs: number;
}

export interface Procs {
  byCpu: ProcRow[];
  byMem: ProcRow[];
  byDisk: ProcRow[];
}

export interface ProcRow {
  name: string;
  exe: string;
  count: number;
  cpu: number;
  mem: number;
  diskRead: number;
  diskWrite: number;
}

export interface History {
  cpuUser: number[];
  cpuSystem: number[];
  cores: number[][];
  mem: number[];
  netUp: number[];
  netDown: number[];
  diskRead: number[];
  diskWrite: number[];
  gpus: number[][];
  npu: number[];
  battery: number[];
  batteryCharging: number[];
  power: number[];
  temp: number[];
  fan: number[];
}

export type ModuleId = "cpu" | "gpu" | "mem" | "net" | "disk" | "battery" | "temp" | "fans" | "power" | "npu";

export type Theme = "auto" | "dark" | "light";
export type Position = "left" | "right";
/** Where each taskbar item's name goes relative to its value. */
export type Label = "top" | "bottom" | "side" | "none";

export interface Settings {
  position: Position;
  monitor: string | null;
  modules: ModuleId[];
  intervalMs: number;
  theme: Theme;
  pingHost: string;
  colors: { primary: string; secondary: string };
  saveMemory: boolean;
  /** "auto" follows the Windows display language. */
  language: "auto" | "pt" | "en";
  label: Label;
  /** The graphic left of each item's text. */
  graphics: boolean;
  /** Two items per column, numbers only, tagged by an icon or the name. */
  compact: boolean;
  compactTag: "icon" | "label";
  /** Where the name goes: the compact tag, and the stacked letters of "beside". */
  side: "left" | "right";
  /** The NPU item was added once on this machine (Rust decides; kept as is). */
  npuOffered?: boolean;
  /** Fans (reading ids) left out of the panel and the taskbar. */
  hiddenFans: string[];
  /** The fan the taskbar shows; null = the fastest. */
  barFan: string | null;
}

export interface MonitorInfo {
  id: string;
  name: string;
  width: number;
  height: number;
  primary: boolean;
}
