import type { State } from "../data";
import { t } from "../i18n";
import { card, h } from "../ui";
import { footer, peak, type Module } from "./common";
import { headlineRings, historyGraph, readingList, rpm } from "./readings";

/** Dell's names for its BIOS thermal modes. */
const modeName = (m: string) =>
  ({ Optimized: t.modeOptimized, Cool: t.modeCool, Quiet: t.modeQuiet, UltraPerformance: t.modeUltra })[m] ?? m;

/** Rings fill against this, or the fastest speed seen if higher. */
const TYPICAL_MAX_RPM = 5000;

export function fansModule(): Module {
  const rings = headlineRings(4);

  const graphCard = card(t.fanSpeed);
  const graph = historyGraph("fans", "--blue", 2000, (m) => m.fan ?? null);
  graphCard.el.append(graph.el);

  const list = card(t.fans);
  const fans = readingList(rpm);
  list.el.append(fans.el);

  // The firmware's thermal mode, shown (not yet changed) here.
  const mode = card(t.thermalMode);
  const modeRow = h("div", "mode-row");
  mode.el.append(modeRow);
  let modeKey = "";

  const note = card(undefined, "note-card");
  note.el.append(h("p", "note", t.noFansNote));

  const el = h("div", "module", rings.el, graphCard.el, list.el, mode.el, note.el, footer("taskmgr", "eventvwr", "terminal", "power", "resmon"));

  return {
    el,
    update({ snapshot: s, history: hist }: State) {
      const all = s.sensors.fans;
      const top = Math.max(TYPICAL_MAX_RPM, peak(hist.fan, hist.fan.length));
      rings.set(
        all.slice(0, 4).map((r) => {
          const q = rpm(r.value);
          return { label: r.label, q: { v: q.v, u: "" }, frac: r.value / top, color: "--blue" };
        }),
      );
      graphCard.el.hidden = hist.fan.length === 0;
      graph.set(hist.fan);
      list.el.hidden = all.length === 0;
      fans.set(all);
      note.el.hidden = all.length > 0;

      const m = s.sensors.thermalMode;
      mode.el.hidden = !m;
      const key = m ? `${m.current}|${m.options.join(",")}` : "";
      if (m && key !== modeKey) {
        modeKey = key;
        modeRow.replaceChildren(...m.options.map((o) => h("span", `mode-chip${o === m.current ? " on" : ""}`, modeName(o))));
      }
    },
  };
}
