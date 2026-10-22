import { stackedBars } from "../charts";
import type { State } from "../data";
import { bytes, ghz, pctFine, qtyText } from "../format";
import { t } from "../i18n";
import { bucket, minutes } from "../history";
import { card, h, legend, rangedGraph, setText } from "../ui";
import { footer, type Module } from "./common";
import { gpuGauges, shortGpuName } from "./cpu";
import { celsius, rpm, watts } from "./readings";


export function gpuModule(): Module {
  const usage = card(t.gpu);
  const graph = rangedGraph("gpu", "graph-cpu");
  usage.el.append(graph.el);
  let last: number[] = [];
  const draw = () => {
    const r = graph.tabs.value;
    if (r === "live") return stackedBars(graph.canvas, [{ data: last, color: "--blue" }]);
    minutes().then((m) => {
      const g = bucket(m, r, (x) => x.gpu);
      stackedBars(graph.canvas, [{ data: g, color: "--blue" }], 100, g.length);
    });
  };
  graph.tabs.onChange = draw;

  const rings = card();
  const gg = gpuGauges();
  rings.el.append(gg.el);

  const engines = card(t.engines);
  const engineList = h("div");
  engines.el.append(engineList);
  let engineRows = new Map<string, ReturnType<typeof legend>>();

  const memory = card();
  const ded = legend(null, t.dedicated);
  const shr = legend(null, t.shared);
  memory.el.append(ded.el, shr.el);

  // What the driver reports about the board: hidden when it reports nothing.
  const sensors = card(t.sensors);
  const temp = legend(null, t.temperature);
  const fan = legend(null, t.fan);
  const fan2 = legend(null, `${t.fan} 2`);
  const clock = legend(null, t.clock);
  const power = legend(null, t.power);
  sensors.el.append(temp.el, fan.el, fan2.el, clock.el, power.el);

  const el = h("div", "module", usage.el, rings.el, engines.el, memory.el, sensors.el, footer("taskmgr", "eventvwr", "terminal", "dxdiag", "resmon"));

  return {
    el,
    update({ snapshot: s, history: hist }: State) {
      const g = s.gpus[0];
      if (!g) return;
      setText(usage.right!, shortGpuName(g.name));
      last = hist.gpus[0] ?? [];
      draw();
      gg.set(g);

      const shown = g.engines.filter(([, v], i) => i < 5 || v > 0.5);
      const names = shown.map(([n]) => n).join("|");
      if (names !== [...engineRows.keys()].join("|")) {
        engineRows = new Map(shown.map(([n]) => [n, legend(null, t.engineNames[n] ?? n)]));
        engineList.replaceChildren(...[...engineRows.values()].map((r) => r.el));
      }
      shown.forEach(([n, v]) => engineRows.get(n)!.set(pctFine(v)));

      ded.el.hidden = g.dedicatedTotal === 0;
      ded.set(bytes(g.dedicatedUsed));
      setText(ded.name, `${t.dedicated} (${t.of} ${qtyText(bytes(g.dedicatedTotal))})`);
      shr.set(bytes(g.sharedUsed));
      setText(shr.name, `${t.shared} (${t.of} ${qtyText(bytes(g.sharedTotal))})`);

      // Two-fan cards get a row each; more fans show the fastest.
      const twoFans = g.fansRpm.length === 2;
      setText(fan.name, twoFans ? `${t.fan} 1` : t.fan);
      const rows: [ReturnType<typeof legend>, number | null | undefined, (v: number) => { v: string; u: string }][] = [
        [temp, g.tempC, celsius],
        [fan, twoFans ? g.fansRpm[0] : g.fanRpm, rpm],
        [fan2, twoFans ? g.fansRpm[1] : null, rpm],
        [clock, g.freqMhz, ghz],
        [power, g.powerW, watts],
      ];
      for (const [row, v, fmt] of rows) {
        row.el.hidden = v == null;
        if (v != null) row.set(fmt(v));
      }
      sensors.el.hidden = rows.every(([, v]) => v == null);
    },
  };
}
