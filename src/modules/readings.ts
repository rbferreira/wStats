// Pieces shared by the temperature, fans and power popups: number formats, the list of
// readings that come and go with the hardware, and the ranged history graph.

import { stackedBars } from "../charts";
import { bucket, minutes, type Minute } from "../history";
import type { Reading } from "../types";
import { card, gauge, h, legend, rangedGraph, setText } from "../ui";
import { peak } from "./common";

const nf1 = new Intl.NumberFormat(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
export const watts = (w: number) => ({ v: w >= 100 ? Math.round(w).toString() : nf1.format(w), u: "W" });
export const celsius = (c: number) => ({ v: `${Math.round(c)}°`, u: "" });
export const rpm = (v: number) => ({ v: Math.round(v).toLocaleString(), u: "rpm" });

/** Rows that come and go with the hardware (one per reading), reused between ticks. */
export function readingList(format: (v: number) => { v: string; u: string }) {
  const el = h("div");
  let rows = new Map<string, ReturnType<typeof legend>>();
  return {
    el,
    set(readings: Reading[]) {
      const ids = readings.map((r) => r.id).join("|");
      if (ids !== [...rows.keys()].join("|")) {
        rows = new Map(readings.map((r) => [r.id, legend(null, r.label)]));
        el.replaceChildren(...[...rows.values()].map((r) => r.el));
      }
      readings.forEach((r) => rows.get(r.id)!.set(format(r.value)));
    },
  };
}

/**
 * A bar graph over "now / 1 h / 24 h": the live ring buffer, or the minute averages
 * picked by `pick`. The scale is at least `floor`, else the visible peak plus 10 %.
 */
export function historyGraph(key: string, color: string, floor: number, pick: (m: Minute) => number | null) {
  const graph = rangedGraph(key, "graph-cpu");
  let live: number[] = [];
  const draw = () => {
    const r = graph.tabs.value;
    if (r === "live") return stackedBars(graph.canvas, [{ data: live, color }], Math.max(floor, peak(live, live.length) * 1.1));
    minutes().then((m) => {
      const p = bucket(m, r, pick);
      stackedBars(graph.canvas, [{ data: p, color }], Math.max(floor, peak(p, p.length) * 1.1), p.length);
    });
  };
  graph.tabs.onChange = draw;
  return {
    el: graph.el,
    set(data: number[]) {
      live = data;
      draw();
    },
  };
}

export interface Headline {
  label: string;
  q: { v: string; u: string };
  /** 0..1 of the ring. */
  frac: number;
  color: string;
}

/** Ring gauges at the top of the popup: up to three, or four smaller ones when `max` is 4. */
export function headlineRings(max: 3 | 4 = 3) {
  const c = card(undefined, "rings-card");
  const big = [gauge("", "md"), gauge("", "md"), gauge("", "md")];
  const bigRow = h("div", "gauges gauges-3", ...big.map((g) => g.el));
  const small = max === 4 ? [gauge("", "sm"), gauge("", "sm"), gauge("", "sm"), gauge("", "sm")] : [];
  const smallRow = h("div", "gauges gauges-4", ...small.map((g) => g.el));
  c.el.append(bigRow, smallRow);
  return {
    el: c.el,
    set(items: Headline[]) {
      items = items.slice(0, max);
      c.el.hidden = items.length === 0;
      // Four don't fit at full size: switch to the small row.
      const four = items.length === 4;
      bigRow.hidden = four;
      smallRow.hidden = !four;
      (four ? small : big).forEach((g, i) => {
        const x = items[i];
        g.el.hidden = !x;
        if (!x) return;
        setText(g.caption, x.label.toUpperCase());
        g.set(x.q, [{ value: Math.min(1, x.frac), color: x.color }]);
      });
    },
  };
}
