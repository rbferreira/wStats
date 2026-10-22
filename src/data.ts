// Where the popup gets its numbers: the Rust sampler inside Tauri, or the mock in a
// plain browser (UI development and the preview gallery).

import { mockState, mockTick } from "./mock";
import type { History, Snapshot } from "./types";

export interface State {
  snapshot: Snapshot;
  history: History;
}

export const inTauri = "__TAURI_INTERNALS__" in window;

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
let invoke: Invoke | null = null;

async function tauri() {
  if (!invoke) invoke = (await import("@tauri-apps/api/core")).invoke;
  return invoke;
}

const HISTORY_LEN = 600;

/** Same bookkeeping as `History::record` in Rust, applied to each streamed snapshot. */
function record(h: History, s: Snapshot) {
  const push = (a: number[], v: number) => {
    a.push(v);
    if (a.length > HISTORY_LEN) a.shift();
  };
  push(h.cpuUser, s.cpu.user);
  push(h.cpuSystem, s.cpu.system);
  if (h.cores.length !== s.cpu.cores.length) h.cores = s.cpu.cores.map(() => []);
  s.cpu.cores.forEach((v, i) => push(h.cores[i], v));
  push(h.mem, s.mem.usedPct);
  push(h.netUp, s.net.up);
  push(h.netDown, s.net.down);
  push(h.diskRead, s.disk.read);
  push(h.diskWrite, s.disk.write);
  if (h.gpus.length !== s.gpus.length) h.gpus = s.gpus.map(() => []);
  s.gpus.forEach((g, i) => push(h.gpus[i], g.util));
  if (s.npu) push(h.npu, s.npu.util);
  if (s.sensors.power.length) push(h.power, s.sensors.power[0].value);
  const temp = s.cpu.tempC ?? (s.sensors.temps.length ? Math.max(...s.sensors.temps.map((r) => r.value)) : null);
  if (temp !== null) push(h.temp, temp);
  if (s.sensors.fans.length) {
    const chosen = s.sensors.fans.find((r) => r.id === s.sensors.barFan);
    push(h.fan, chosen ? chosen.value : Math.max(...s.sensors.fans.map((r) => r.value)));
  }
  if (s.battery) {
    push(h.battery, s.battery.percent);
    push(h.batteryCharging, s.battery.charging ? 1 : 0);
  }
}

/** Starts the data stream; `onUpdate` fires once immediately and then on every sample. */
export async function connect(onUpdate: (s: State) => void): Promise<State> {
  if (!inTauri) {
    const state = mockState();
    onUpdate(state);
    setInterval(() => {
      mockTick(state);
      onUpdate(state);
    }, 1000);
    return state;
  }
  const inv = await tauri();
  const { listen } = await import("@tauri-apps/api/event");
  const state = await inv<State>("get_state");
  onUpdate(state);
  await listen<Snapshot>("stats", (e) => {
    record(state.history, e.payload);
    state.snapshot = e.payload;
    onUpdate(state);
  });
  return state;
}

// ---- app icons ---------------------------------------------------------------------

const icons = new Map<string, Promise<string>>();

/** Coloured letter tile, used in the browser and when Windows has no icon to give. */
function letterTile(name: string): string {
  let hash = 0;
  for (const c of name) hash = (hash * 31 + c.charCodeAt(0)) | 0;
  const hue = Math.abs(hash) % 360;
  const letter = (name.trim()[0] || "?").toUpperCase();
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="hsl(${hue} 55% 48%)"/><text x="16" y="22" font-family="Segoe UI, sans-serif" font-size="17" font-weight="600" text-anchor="middle" fill="#fff">${letter}</text></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

export function appIcon(exe: string, name: string): Promise<string> {
  const key = exe || name;
  let p = icons.get(key);
  if (!p) {
    p = inTauri && exe.includes("\\")
      ? tauri()
          .then((inv) => inv<string | null>("app_icon", { exe }))
          .then((src) => src ?? letterTile(name))
          .catch(() => letterTile(name))
      : Promise.resolve(letterTile(name));
    icons.set(key, p);
  }
  return p;
}

/** Opens one of the footer shortcuts (Task Manager, Resource Monitor, ...). */
export async function launch(id: string) {
  if (inTauri) await (await tauri())("launch", { id });
  else console.info("launch", id);
}
