import type { State } from "../data";
import { t } from "../i18n";
import type { Peripheral, PeripheralKind } from "../types";
import { card, h, setText } from "../ui";
import type { Module } from "./common";

const svg = (body: string) =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;

/** The same drawings as `kind_shapes` in `src-tauri/src/strip/icons.rs`. */
export const KIND_ICONS: Record<PeripheralKind, string> = {
  headset: svg('<path d="M4 14v-2a8 8 0 0 1 16 0v2"/><rect x="3" y="14" width="4" height="6" rx="1.5"/><rect x="17" y="14" width="4" height="6" rx="1.5"/>'),
  mouse: svg('<rect x="6" y="3" width="12" height="18" rx="6"/><path d="M12 7v4"/>'),
  keyboard: svg('<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>'),
  gamepad: svg(
    '<path d="M6 8h12a4 4 0 0 1 4 4l-.5 4a2.5 2.5 0 0 1-4.3 1.4L15.5 16h-7l-1.7 1.4A2.5 2.5 0 0 1 2.5 16L2 12a4 4 0 0 1 4-4z"/><path d="M7 11v3M5.5 12.5h3"/>',
  ),
  phone: svg('<rect x="7" y="2.5" width="10" height="19" rx="2"/><path d="M11 18.5h2"/>'),
  other: svg('<path d="M7 7l10 10-5 4V3l5 4L7 17"/>'),
};

/** "há 19 h" / "19 h ago" for a reading taken at `ms`. */
export function ago(ms: number, now = Date.now()) {
  if (!ms) return "";
  const min = Math.max(0, Math.round((now - ms) / 60000));
  if (min < 1) return t.devices.justNow;
  if (min < 60) return t.devices.ago(`${min} min`);
  const hours = Math.round(min / 60);
  if (hours < 48) return t.devices.ago(`${hours} h`);
  return t.devices.ago(`${Math.round(hours / 24)} d`);
}

const VIA: Record<Peripheral["via"], string> = {
  bluetooth: "Bluetooth",
  logitech: "Logitech",
  xbox: "Xbox",
  playstation: "PlayStation",
  usb: "USB",
};

/** Where a device was read: "Bluetooth", "Logitech", plus its state. */
export function sourceText(p: Peripheral) {
  const parts = [VIA[p.via] ?? "Bluetooth"];
  if (p.connected && p.charging) parts.push(t.charging.toLowerCase());
  if (!p.connected && p.seen) parts.push(ago(p.seen));
  return parts.join(" · ");
}

function deviceRow() {
  const icon = h("span", "dev-icon");
  const name = h("div", "dev-name");
  const sub = h("div", "dev-sub");
  const fill = h("div", "meter-fill");
  const pct = h("div", "dev-pct");
  const el = h("div", "dev", icon, h("div", "dev-text", name, sub), h("div", "meter dev-meter", fill), pct);
  let kind: PeripheralKind | null = null;
  return {
    el,
    set(p: Peripheral) {
      if (kind !== p.kind) {
        icon.innerHTML = KIND_ICONS[p.kind] ?? KIND_ICONS.other;
        kind = p.kind;
      }
      el.classList.toggle("off", !p.connected);
      setText(name, p.name);
      setText(sub, sourceText(p));
      fill.style.width = `${p.percent}%`;
      fill.style.background = !p.connected ? "var(--text-3)" : p.charging ? "var(--green)" : p.percent <= 20 ? "var(--red)" : "var(--blue)";
      setText(pct, `${p.percent}%`);
    },
  };
}

export function devicesModule(): Module {
  const now = card(t.devices.title);
  const list = h("div", "devs");
  const away = h("div", "devs");
  const awayHead = h("div", "dev-section", t.devices.disconnected);
  const empty = h("p", "dev-empty", t.devices.none);
  now.el.append(empty, list, awayHead, away);
  const el = h("div", "module", now.el);

  const rows = new Map<string, ReturnType<typeof deviceRow>>();
  return {
    el,
    update({ snapshot: s }: State) {
      const all = s.peripherals ?? [];
      empty.hidden = all.length > 0;
      const seen = new Set<string>();
      const place = (into: HTMLElement, ps: Peripheral[]) => {
        for (const p of ps) {
          let row = rows.get(p.id);
          if (!row) rows.set(p.id, (row = deviceRow()));
          row.set(p);
          into.append(row.el); // also reorders
          seen.add(p.id);
        }
      };
      const connected = all.filter((p) => p.connected);
      const disconnected = all.filter((p) => !p.connected);
      place(list, connected);
      place(away, disconnected);
      awayHead.hidden = disconnected.length === 0;
      for (const [id, row] of rows) {
        if (!seen.has(id)) {
          row.el.remove();
          rows.delete(id);
        }
      }
    },
  };
}
