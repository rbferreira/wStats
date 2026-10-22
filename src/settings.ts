// The settings window: an icon sidebar, a live preview of the selected item (taskbar
// strip + dropdown, real data) in the middle, and the options on the right. Every change
// applies immediately.

import "./styles/popup.css";
import "./styles/settings.css";
import { connect, inTauri, type State } from "./data";
import { onLanguageChange, t } from "./i18n";
import { MODULES } from "./modules";
import type { Module } from "./modules/common";
import { applySettings } from "./theme";
import type { ModuleId, MonitorInfo, Settings } from "./types";
import { h, setText } from "./ui";

const ALL: ModuleId[] = ["cpu", "mem", "disk", "net", "gpu", "npu", "battery", "temp", "fans", "power"];

/** Items this machine can show: the NPU only where there is one. */
const available = () => ALL.filter((m) => m !== "npu" || !!state?.snapshot.npu);

/** Pages that read the sensor service and show its status. */
const SERVICE_PAGES: ModuleId[] = ["temp", "fans"];

/** An item's name in the current language. */
const name = (m: ModuleId) =>
  ({ cpu: t.cpu, gpu: t.gpu, mem: t.memory, net: t.network, disk: t.disks, battery: t.battery, temp: t.temperature, fans: t.fans, power: t.powerDraw, npu: t.npu })[m];

const DEFAULTS: Settings = {
  position: "left",
  monitor: null,
  modules: ["cpu", "mem", "disk", "net", "battery"],
  intervalMs: 1000,
  theme: "auto",
  pingHost: "1.1.1.1",
  colors: { primary: "#3478F6", secondary: "#78C04F" },
  saveMemory: false,
  language: "auto",
  label: "top",
  graphics: true,
  compact: false,
  compactTag: "icon",
  side: "left",
  hiddenFans: [],
  barFan: null,
};

const PALETTE = ["#3478F6", "#78C04F", "#F5C542", "#EF8240", "#EF5350", "#E85D9E", "#A06CF0", "#5AC8E8"];

// ---- icons (24px line icons, drawn for this app) --------------------------------------

const svg = (body: string) =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;

const ICONS: Record<string, string> = {
  general: svg('<circle cx="12" cy="12" r="3.2"/><path d="M12 2.8v2.4M12 18.8v2.4M4.2 7.5l2.1 1.2M17.7 15.3l2.1 1.2M4.2 16.5l2.1-1.2M17.7 8.7l2.1-1.2"/><circle cx="12" cy="12" r="7"/>'),
  cpu: svg('<rect x="6" y="6" width="12" height="12" rx="2"/><rect x="9.5" y="9.5" width="5" height="5" rx=".8"/><path d="M9 3v3M12 3v3M15 3v3M9 18v3M12 18v3M15 18v3M3 9h3M3 12h3M3 15h3M18 9h3M18 12h3M18 15h3"/>'),
  gpu: svg('<rect x="2.5" y="6.5" width="19" height="11" rx="2"/><circle cx="9" cy="12" r="2.8"/><path d="M15 10h3.5M15 13.5h3.5M5 17.5v2M8 17.5v2"/>'),
  mem: svg('<rect x="3" y="7" width="18" height="9" rx="1.5"/><path d="M7 10v3M10.5 10v3M14 10v3M17.5 10v3M5 16v2.5M9 16v2.5M13 16v2.5M17 16v2.5"/>'),
  disk: svg('<ellipse cx="12" cy="6.5" rx="8" ry="3"/><path d="M4 6.5v11c0 1.7 3.6 3 8 3s8-1.3 8-3v-11"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>'),
  net: svg('<path d="M7 20V5M3.5 8.5 7 5l3.5 3.5M17 4v15M13.5 15.5 17 19l3.5-3.5"/>'),
  battery: svg('<rect x="2.5" y="7" width="17" height="10" rx="2.2"/><path d="M21.5 10.5v3"/><rect x="5" y="9.5" width="9" height="5" rx="1" fill="currentColor" stroke="none"/>'),
  temp: svg('<path d="M10 14.5V5a2 2 0 1 1 4 0v9.5a4 4 0 1 1-4 0z"/><path d="M12 9v7"/><circle cx="12" cy="17.5" r="1.4" fill="currentColor"/>'),
  fans: svg('<circle cx="12" cy="12" r="9.2"/><circle cx="12" cy="12" r="2.2"/><path d="M11.5 9.7C11.8 7.0 14.0 4.8 16.9 4.6M13.7 10.4C16.2 9.3 19.2 10.1 20.9 12.5M14.2 12.7C16.4 14.3 17.2 17.3 16.0 19.9M12.5 14.3C12.2 17.0 10.0 19.2 7.1 19.4M10.3 13.6C7.8 14.7 4.8 13.9 3.1 11.5M9.8 11.3C7.6 9.7 6.8 6.7 8.0 4.1"/>'),
  power: svg('<path d="M13 2.8 5.5 13.2h5.6L10 21.2l8.5-11h-5.8z"/>'),
  npu: svg('<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 3v3M12 3v3M15 3v3M9 18v3M12 18v3M15 18v3M3 9h3M3 12h3M3 15h3M18 9h3M18 12h3M18 15h3"/><path d="M12 8.6l.9 2.5 2.5.9-2.5.9-.9 2.5-.9-2.5-2.5-.9 2.5-.9z" fill="currentColor" stroke-width="1"/>'),
  about: svg('<circle cx="12" cy="12" r="9"/><path d="M12 11v5.5M12 7.6v.1"/>'),
  grip: svg('<path d="M9 6h.01M9 12h.01M9 18h.01M15 6h.01M15 12h.01M15 18h.01" stroke-width="2.6"/>'),
};

function icon(name: string, cls = "icon") {
  const el = h("span", cls);
  el.innerHTML = ICONS[name];
  return el;
}

// ---- backend --------------------------------------------------------------------------

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
let invoke: Invoke = async () => {
  throw new Error("not in Tauri");
};

let settings: Settings = structuredClone(DEFAULTS);
let monitors: MonitorInfo[] = [{ id: "\\\\.\\DISPLAY1", name: "", width: 2880, height: 1800, primary: true }];
let autostart = false;
let saveTimer = 0;

interface SensorStatus {
  available: boolean;
  installed: boolean;
  running: boolean;
  outdated: boolean;
  pawnio: boolean;
  pawnioBundled: boolean;
  cpu: boolean;
  power: boolean;
  fans: number;
  board: number;
  notes: string[];
}

/** The sensor service as last checked; null = check again when the page is shown. */
let sensorStatus: SensorStatus | null = null;
let sensorBusy = false;
let sensorError = "";

/** Applies a change everywhere at once: this page, the popup, the taskbar. */
function update(change: (s: Settings) => void) {
  change(settings);
  applySettings(settings);
  render();
  clearTimeout(saveTimer);
  // Coalesce bursts (dragging a colour picker) into one write.
  saveTimer = window.setTimeout(() => {
    if (inTauri) invoke("set_settings", { settings }).catch(console.error);
  }, 120);
}

// ---- small controls -------------------------------------------------------------------

function toggle(on: boolean, onChange: (v: boolean) => void, label = "") {
  const b = h("button", `toggle${on ? " on" : ""}`, h("span", "knob"));
  b.setAttribute("role", "switch");
  b.setAttribute("aria-checked", String(on));
  if (label) b.setAttribute("aria-label", label);
  b.onclick = () => onChange(!on);
  return b;
}

function segmented<T extends string | number>(options: [T, string][], value: T, onChange: (v: T) => void) {
  const el = h("div", "segmented");
  for (const [v, label] of options) {
    const b = h("button", v === value ? "active" : "", label);
    b.onclick = () => onChange(v);
    el.append(b);
  }
  return el;
}

function field(label: string, control: Node, hint?: string) {
  return h("div", "field", h("div", "field-label", label), control, hint ? h("div", "field-hint", hint) : null);
}

function group(title: string, ...children: Node[]) {
  return h("section", "group", h("h3", "group-title", title), ...children);
}

function monitorLabel(m: MonitorInfo, i: number) {
  const label = m.name && m.name !== "Display" ? m.name : m.primary ? t.set.builtinDisplay : `${t.set.monitor} ${i + 1}`;
  return `${label} · ${m.width}×${m.height}${m.primary ? ` (${t.set.primary})` : ""}`;
}

// ---- live previews --------------------------------------------------------------------

let state: State | null = null;
let preview: { id: ModuleId; mod: Module } | null = null;
let stripImg: HTMLImageElement | null = null;
let stripModules: ModuleId[] = [];

async function refreshStrip() {
  if (!stripImg || !inTauri) return;
  const img = stripImg;
  const look = { label: settings.label, graphics: settings.graphics, compact: settings.compact, tag: settings.compactTag, side: settings.side };
  const src = await invoke<string | null>("strip_preview", { modules: stripModules, light: null, look }).catch(() => null);
  if (src && img === stripImg) img.src = src;
}

function taskbarBand(modules: ModuleId[]) {
  stripModules = modules;
  stripImg = h("img", "strip-img");
  stripImg.alt = "";
  const band = h("div", "taskbar", h("div", "taskbar-slot", stripImg));
  if (!inTauri) band.append(h("div", "strip-placeholder", modules.map(name).join("  ·  ")));
  refreshStrip();
  return band;
}

// ---- pages ----------------------------------------------------------------------------

type Page = "general" | ModuleId | "about";
let page: Page = "general";

const sidebar = h("nav", "sidebar");
const stage = h("section", "stage");
const options = h("aside", "options");

function renderSidebar() {
  const pages: Page[] = ["general", ...available(), "about"];
  sidebar.replaceChildren(
    ...pages.map((p) => {
      const b = h("button", `nav${p === page ? " active" : ""}`, icon(p));
      b.title = p === "general" ? t.set.general : p === "about" ? t.set.about : name(p);
      if (p !== "general" && p !== "about" && settings.modules.includes(p)) b.append(h("span", "nav-dot"));
      b.onclick = () => {
        page = p;
        if (SERVICE_PAGES.includes(p as ModuleId)) sensorStatus = null;
        render();
      };
      return b;
    }),
  );
}

/** The reorderable list of taskbar items (drag the rows, flip the switches). Rows move
 * in the DOM while dragging; the new order is saved when the drag ends. */
function itemList() {
  const list = h("div", "items");
  const order = [...settings.modules.filter((m) => available().includes(m)), ...available().filter((m) => !settings.modules.includes(m))];
  let dragged: HTMLElement | null = null;
  for (const m of order) {
    const on = settings.modules.includes(m);
    const row = h(
      "div",
      `item${on ? "" : " off"}`,
      icon("grip", "grip"),
      icon(m, "item-icon"),
      h("span", "item-name", name(m)),
      toggle(on, (v) =>
        update((s) => {
          s.modules = v ? [...s.modules, m] : s.modules.filter((x) => x !== m);
        }), t.set.showOnBar(name(m))),
    );
    row.dataset.module = m;
    row.draggable = on;
    row.ondragstart = (e) => {
      dragged = row;
      e.dataTransfer!.effectAllowed = "move";
      requestAnimationFrame(() => row.classList.add("dragging"));
    };
    row.ondragover = (e) => {
      if (!dragged || dragged === row || !on) return;
      e.preventDefault();
      const before = e.offsetY < row.offsetHeight / 2;
      list.insertBefore(dragged, before ? row : row.nextSibling);
    };
    row.ondragend = () => {
      row.classList.remove("dragging");
      dragged = null;
      const visible = [...list.children]
        .map((r) => (r as HTMLElement).dataset.module as ModuleId)
        .filter((x) => settings.modules.includes(x));
      if (visible.join() !== settings.modules.join()) update((s) => (s.modules = visible));
    };
    list.append(row);
  }
  return list;
}

/** Which side the name goes: the compact tag, or the letters of the "beside" label. */
function sideField() {
  return field(t.set.side, segmented<Settings["side"]>([["left", t.set.left], ["right", t.set.right]], settings.side, (v) => update((s) => (s.side = v))));
}

function generalPage() {
  stage.replaceChildren(
    h("header", "stage-head", h("h1", "", t.set.general)),
    h(
      "div",
      "stage-body",
      h("h2", "stage-title", t.set.taskbarItems),
      taskbarBand(settings.modules),
      h("p", "stage-hint", t.set.dragHint),
      itemList(),
    ),
  );

  const colorRow = (key: "primary" | "secondary", label: string) => {
    const input = h("input", "color-input");
    input.type = "color";
    input.value = settings.colors[key];
    input.oninput = () => update((s) => (s.colors[key] = input.value.toUpperCase()));
    const swatches = h(
      "div",
      "swatches",
      ...PALETTE.map((c) => {
        const b = h("button", `swatch${c.toLowerCase() === settings.colors[key].toLowerCase() ? " active" : ""}`);
        b.style.background = c;
        b.title = c;
        b.onclick = () => update((s) => (s.colors[key] = c));
        return b;
      }),
      h("label", "swatch custom", input),
    );
    return field(label, swatches);
  };

  options.replaceChildren(
    group(
      t.set.appearance,
      field(
        t.set.language,
        // Language names stay in their own language, so either is findable.
        segmented<Settings["language"]>([["auto", t.set.automatic], ["pt", "Português"], ["en", "English"]], settings.language, (v) => update((s) => (s.language = v))),
        t.set.languageHint,
      ),
      field(
        t.set.menuTheme,
        segmented<Settings["theme"]>([["auto", t.set.automatic], ["dark", t.set.dark], ["light", t.set.light]], settings.theme, (v) => update((s) => (s.theme = v))),
        t.set.themeHint,
      ),
      colorRow("primary", t.set.primaryColor),
      colorRow("secondary", t.set.secondaryColor),
    ),
    group(
      t.set.taskbar,
      field(
        t.set.position,
        segmented<Settings["position"]>([["left", t.set.nextToWidget], ["right", t.set.nextToTray]], settings.position, (v) => update((s) => (s.position = v))),
      ),
      h(
        "div",
        "field",
        h("div", "field row", h("div", "field-label", t.set.compact), toggle(settings.compact, (v) => update((s) => (s.compact = v)), t.set.compact)),
        h("p", "field-hint", t.set.compactHint),
      ),
      // Compact mode has its own two choices; the label and graphics apply to the full one.
      ...(settings.compact
        ? [
            field(
              t.set.compactTag,
              segmented<Settings["compactTag"]>([["icon", t.set.tagIcon], ["label", t.set.tagLabel]], settings.compactTag, (v) => update((s) => (s.compactTag = v))),
            ),
            sideField(),
          ]
        : [
            field(
              t.set.label,
              segmented<Settings["label"]>(
                [["top", t.set.labelTop], ["bottom", t.set.labelBottom], ["side", t.set.labelSide], ["none", t.set.labelNone]],
                settings.label,
                (v) => update((s) => (s.label = v)),
              ),
              t.set.labelHint,
            ),
            ...(settings.label === "side" ? [sideField()] : []),
            h(
              "div",
              "field",
              h("div", "field row", h("div", "field-label", t.set.graphics), toggle(settings.graphics, (v) => update((s) => (s.graphics = v)), t.set.graphics)),
              h("p", "field-hint", t.set.graphicsHint),
            ),
          ]),
      field(t.set.monitor, monitorSelect(), t.set.monitorHint),
    ),
    group(
      t.set.behaviour,
      field(
        t.set.interval,
        segmented<number>([500, 1000, 2000, 5000].map((ms) => [ms, `${(ms / 1000).toLocaleString()} s`] as [number, string]), settings.intervalMs, (v) => update((s) => (s.intervalMs = v))),
      ),
      field(t.ping, pingInput(), t.set.pingHint),
      h(
        "div",
        "field row",
        h("div", "field-label", t.set.autostart),
        toggle(autostart, async (v) => {
          autostart = v;
          if (inTauri) await invoke("set_autostart", { enabled: v }).catch(console.error);
          render();
        }),
      ),
      h(
        "div",
        "field",
        h(
          "div",
          "field row",
          h("div", "field-label", t.set.saveMemory),
          toggle(settings.saveMemory, (v) => update((s) => (s.saveMemory = v))),
        ),
        h("p", "field-hint", t.set.saveMemoryHint),
      ),
    ),
  );
}

const ALL_MONITORS = "*";

function monitorSelect() {
  const sel = h("select", "select");
  monitors.forEach((m, i) => {
    const o = h("option", "", monitorLabel(m, i));
    o.value = m.primary ? "" : m.id;
    sel.append(o);
  });
  if (monitors.length > 1) {
    const all = h("option", "", t.set.allMonitors);
    all.value = ALL_MONITORS;
    sel.append(all);
  }
  const current = settings.monitor === ALL_MONITORS ? ALL_MONITORS : monitors.find((m) => m.id === settings.monitor && !m.primary)?.id;
  sel.value = current ?? "";
  sel.onchange = () => update((s) => (s.monitor = sel.value || null));
  return sel;
}

function pingInput() {
  const input = h("input", "text-input");
  input.value = settings.pingHost;
  input.spellcheck = false;
  input.onchange = () => {
    const v = input.value.trim();
    if (v) update((s) => (s.pingHost = v));
  };
  return input;
}

function modulePage(id: ModuleId) {
  const on = settings.modules.includes(id);
  if (!preview || preview.id !== id) preview = { id, mod: MODULES[id]() };
  if (state) preview.mod.update(state);

  stage.replaceChildren(
    h(
      "header",
      "stage-head",
      toggle(on, (v) => update((s) => (s.modules = v ? [...s.modules, id] : s.modules.filter((x) => x !== id))), t.set.showOnBar(name(id))),
      h("h1", "", name(id)),
    ),
    h(
      "div",
      "stage-body center",
      taskbarBand([id]),
      h("div", "popup-frame", h("main", "popup", preview.mod.el)),
    ),
  );

  const index = settings.modules.indexOf(id);
  const move = (d: number) =>
    update((s) => {
      const i = s.modules.indexOf(id);
      const j = i + d;
      if (i < 0 || j < 0 || j >= s.modules.length) return;
      [s.modules[i], s.modules[j]] = [s.modules[j], s.modules[i]];
    });
  const left = h("button", "btn", t.set.moveLeft);
  const right = h("button", "btn", t.set.moveRight);
  left.disabled = index <= 0;
  right.disabled = index < 0 || index >= settings.modules.length - 1;
  left.onclick = () => move(-1);
  right.onclick = () => move(1);

  options.replaceChildren(
    group(t.set.aboutItem, h("p", "prose", t.set.blurbs[id])),
    group(
      t.set.onTaskbar,
      h("div", "field row", h("div", "field-label", t.set.showOnTaskbar), toggle(on, (v) => update((s) => (s.modules = v ? [...s.modules, id] : s.modules.filter((x) => x !== id))))),
      on ? field(t.set.positionOf(index + 1, settings.modules.length), h("div", "btn-row", left, right)) : h("p", "field-hint", t.set.turnOnForPosition),
    ),
    ...(id === "fans" ? [fanChoiceGroup()] : []),
    ...(SERVICE_PAGES.includes(id) ? [sensorServiceGroup()] : []),
    group(t.set.colours, h("p", "prose", t.set.coloursProse)),
  );
}

async function checkSensors() {
  if (!inTauri) {
    sensorStatus = { available: true, installed: false, running: false, outdated: false, pawnio: true, pawnioBundled: true, cpu: false, power: false, fans: 0, board: 0, notes: [] };
  } else {
    sensorStatus = await invoke<SensorStatus>("sensor_status").catch(() => null);
  }
  if (SERVICE_PAGES.includes(page as ModuleId)) render();
}

async function toggleSensorService(install: boolean) {
  sensorBusy = true;
  sensorError = "";
  render();
  try {
    if (inTauri) await invoke("sensor_service", { install });
  } catch (e) {
    sensorError = String(e);
  }
  sensorBusy = false;
  // The service publishes its first sample about a second after starting.
  setTimeout(checkSensors, install ? 1500 : 0);
}

/** Which fans show, and which one the taskbar shows. */
function fanChoiceGroup() {
  const all = state?.snapshot.sensors.allFans ?? [];
  if (!all.length) return group(t.set.fansShown, h("p", "field-hint", t.set.noFansYet));
  const hidden = new Set(settings.hiddenFans ?? []);
  const rows = all.map((r) =>
    h(
      "div",
      "field row",
      h("div", "field-label", r.label),
      toggle(!hidden.has(r.id), (v) => update((s) => (s.hiddenFans = v ? (s.hiddenFans ?? []).filter((x) => x !== r.id) : [...(s.hiddenFans ?? []), r.id])), r.label),
    ),
  );

  const sel = h("select", "select");
  const fastest = h("option", "", t.set.fastestFan);
  fastest.value = "";
  sel.append(fastest);
  for (const r of all.filter((r) => !hidden.has(r.id))) {
    const o = h("option", "", r.label);
    o.value = r.id;
    sel.append(o);
  }
  sel.value = settings.barFan && !hidden.has(settings.barFan) && all.some((r) => r.id === settings.barFan) ? settings.barFan : "";
  sel.onchange = () => update((s) => (s.barFan = sel.value || null));

  return group(t.set.fansShown, h("p", "field-hint", t.set.fansShownHint), ...rows, field(t.set.barFan, sel));
}

/** Status of the background service that reads CPU temperatures and fans. */
function sensorServiceGroup() {
  const st = sensorStatus;
  if (!st) {
    checkSensors();
    return group(t.set.sensorsGroup, h("p", "field-hint", t.set.checking));
  }
  const dot = (on: boolean) => h("span", `status-dot${on ? " on" : ""}`);
  const found: string[] = [];
  if (st.cpu) found.push(t.set.cpuTemperature);
  if (st.power) found.push(t.set.cpuPower);
  if (st.board) found.push(t.set.boardSensors(st.board));
  if (st.fans) found.push(t.set.fanCount(st.fans));

  const state = st.running
    ? h("p", "status-line", dot(true), found.length ? t.set.activeWith(found.join(", ")) : t.set.activeNothing)
    : st.installed
      ? h("p", "status-line", dot(false), t.set.installedStopped)
      : h("p", "status-line", dot(false), t.set.turnedOff);

  const button = h("button", "btn", sensorBusy ? t.set.waitingPermission : st.installed ? t.set.turnOff : t.set.turnOn);
  button.disabled = sensorBusy || !st.available;
  button.onclick = () => toggleSensorService(!st.installed);
  const buttons: HTMLElement[] = [button];
  const needsPawnio = st.installed && !st.pawnio && st.pawnioBundled;
  if ((st.outdated || needsPawnio) && !sensorBusy) {
    const update = h("button", "btn", st.outdated ? t.set.updateService : t.set.installPawnio);
    update.onclick = () => toggleSensorService(true);
    buttons.unshift(update);
  }

  const hints: Node[] = [];
  if (!st.installed) {
    hints.push(h("p", "field-hint", t.set.adminHint));
  }
  if (!st.pawnio && st.pawnioBundled) {
    hints.push(h("p", "field-hint", st.installed ? t.set.pawnioMissing : t.set.pawnioWillInstall));
  } else if (!st.pawnio) {
    hints.push(h("p", "field-hint", t.set.pawnioAbsent));
  }
  if (st.running && st.notes.length) hints.push(h("p", "field-hint", `${t.set.details}: ${st.notes.join(" · ")}`));
  if (!st.available) hints.push(h("p", "field-hint", t.set.notShipped));
  if (sensorError) hints.push(h("p", "field-hint error", t.set.failed(sensorError)));

  if (st.outdated) hints.unshift(h("p", "field-hint", t.set.newerService));
  return group(t.set.sensorsGroup, state, h("div", "btn-row", ...buttons), ...hints);
}

function aboutPage() {
  stage.replaceChildren(
    h("header", "stage-head", h("h1", "", t.set.about)),
    h(
      "div",
      "stage-body center about",
      h("div", "about-mark", icon("cpu")),
      h("h2", "about-name", "wStats"),
      h("p", "about-tag", t.set.tagline),
    ),
  );
  options.replaceChildren(
    group(t.set.howItWorks, h("p", "prose", t.set.howItWorksProse)),
    group(t.set.data, h("p", "prose", t.set.dataProse)),
  );
}

function render() {
  renderSidebar();
  if (page === "general") generalPage();
  else if (page === "about") aboutPage();
  else modulePage(page);
}

// ---- boot -----------------------------------------------------------------------------

async function boot() {
  const app = document.getElementById("app")!;
  // Inside Tauri the window has Mica behind a transparent page; a browser needs a fill.
  if (!inTauri) document.body.classList.add("browser");
  app.append(sidebar, h("div", "page", stage, options));
  if (inTauri) {
    invoke = (await import("@tauri-apps/api/core")).invoke;
    const { listen } = await import("@tauri-apps/api/event");
    [settings, monitors, autostart] = await Promise.all([
      invoke<Settings>("get_settings"),
      invoke<MonitorInfo[]>("list_monitors"),
      invoke<boolean>("get_autostart"),
    ]);
    // Changes made elsewhere (e.g. the taskbar menu) show up here too.
    await listen<Settings>("settings", (e) => {
      if (JSON.stringify(e.payload) === JSON.stringify(settings)) return;
      settings = e.payload;
      applySettings(settings);
      render();
    });
  }
  // In a browser (UI development), ?lang=en|pt picks the language.
  if (!inTauri) settings.language = (new URLSearchParams(location.search).get("lang") as Settings["language"] | null) ?? "auto";
  applySettings(settings);
  render();
  let ticks = 0;
  let hadNpu = false;
  let fanIds = "";
  await connect((s) => {
    state = s;
    // The fan list in Settings follows the fans found.
    const ids = s.snapshot.sensors.allFans.map((r) => `${r.id}:${r.label}`).join("|");
    if (ids !== fanIds) {
      fanIds = ids;
      if (page === "fans") render();
    }
    // The NPU page and row appear once the first sample says there is one.
    if (!!s.snapshot.npu !== hadNpu) {
      hadNpu = !hadNpu;
      render();
    }
    preview?.mod.update(s);
    // The strip preview is rendered by Rust; refresh it every couple of samples.
    if (ticks++ % 2 === 0) refreshStrip();
  });
  setText(document.querySelector("title")!, "wStats");
}

// Every page is rebuilt from scratch in the new language, the item preview included.
onLanguageChange(() => {
  preview = null;
  render();
});

boot();
