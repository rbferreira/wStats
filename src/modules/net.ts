import { mirroredBars } from "../charts";
import { inTauri } from "../data";
import { bytes, int, rate } from "../format";
import { t } from "../i18n";
import type { NetIf } from "../types";
import { bucket, minutes, type Minute } from "../history";
import { bigFigure, card, dot, h, legend, qty, rangedGraph, setText } from "../ui";
import { footer, peak, type Module } from "./common";

// Segoe Fluent Icons code points: Wi-Fi strength 1..4, ethernet, cellular.
const WIFI = ["", "", "", ""];
const ETHERNET = "";
const CELL = "";

export function ifaceGlyph(i: NetIf) {
  if (i.kind === "wifi") return WIFI[Math.min(3, Math.floor((i.wifi?.signal ?? 100) / 25.01))];
  if (i.kind === "cellular") return CELL;
  return ETHERNET;
}

const linkSpeed = (bps: number) =>
  bps >= 1e9 ? `${(bps / 1e9).toLocaleString(undefined, { maximumFractionDigits: 1 })} Gbps` : `${int(bps / 1e6)} Mbps`;

/**
 * The traffic card (big figures, mirrored graph, peaks), shared by network and disks.
 * `key` names its saved range; `pick` reads the matching up/down pair from a minute.
 */
export function trafficCard(key: string, upLabel: string, downLabel: string, pick: (m: Minute) => [number, number]) {
  const c = card(undefined, "traffic-card");
  const up = bigFigure("--green", upLabel);
  const down = bigFigure("--blue", downLabel);
  const graph = rangedGraph(key, "graph-mirror");
  const upPeak = qty();
  const downPeak = qty();
  c.el.append(
    h("div", "figures", up.el, down.el),
    graph.el,
    h("div", "peaks", h("span", "peak", `${upLabel} `, upPeak.el), h("span", "peak", `${downLabel} `, downPeak.el)),
  );
  let live: [number[], number[]] = [[], []];
  const draw = () => {
    const r = graph.tabs.value;
    const paint = (u: (number | null)[], d: (number | null)[], bars?: number) => {
      // 8 KB/s floor keeps idle noise from filling the graph.
      mirroredBars(graph.canvas, { data: u, color: "--green" }, { data: d, color: "--blue" }, 8 * 1024, bars);
      upPeak.set(rate(peak(u, bars ?? 100)));
      downPeak.set(rate(peak(d, bars ?? 100)));
    };
    if (r === "live") return paint(live[0], live[1]);
    minutes().then((m) => {
      const u = bucket(m, r, (x) => pick(x)[0]);
      const d = bucket(m, r, (x) => pick(x)[1]);
      paint(u, d, u.length);
    });
  };
  graph.tabs.onChange = draw;
  return {
    el: c.el,
    set(upNow: number, downNow: number, upHist: number[], downHist: number[]) {
      up.set(rate(upNow));
      down.set(rate(downNow));
      live = [upHist, downHist];
      draw();
    },
  };
}

interface PublicIp {
  v4: string | null;
  v6: string | null;
  country: string | null;
}

let publicIp: Promise<PublicIp> | null = null;
let publicIpAt = 0;

async function fetchPublicIp(): Promise<PublicIp> {
  if (!inTauri) return { v4: "203.0.113.24", v6: null, country: "BR" };
  const get = (url: string) =>
    fetch(url, { signal: AbortSignal.timeout(4000) })
      .then((r) => (r.ok ? r.json() : null))
      .catch(() => null);
  const [v4, v6] = await Promise.all([get("https://api.ipify.org?format=json"), get("https://api6.ipify.org?format=json")]);
  const ip4 = v4?.ip ?? null;
  const geo = ip4 ? await get(`https://api.country.is/${ip4}`) : null;
  return { v4: ip4, v6: v6?.ip ?? null, country: geo?.country ?? null };
}

function lookupPublicIp() {
  if (!publicIp || Date.now() - publicIpAt > 10 * 60_000) {
    const lookup = fetchPublicIp();
    publicIp = lookup;
    publicIpAt = Date.now();
    // A lookup that found nothing (offline, waking up) is asked again next time.
    lookup.then((r) => {
      if (!r.v4 && !r.v6 && publicIp === lookup) publicIp = null;
    });
  }
  return publicIp;
}

export function netModule(): Module {
  const traffic = trafficCard("net", t.upload, t.download, (m) => [m.nu, m.nd]);

  const iface = card(undefined, "iface-card");
  const glyph = h("span", "glyph");
  const ifName = h("span", "iface-name");
  const ifRight = h("span", "iface-right");
  const ifDetail = h("div", "iface-detail");
  iface.el.append(h("div", "iface-row", glyph, ifName, ifRight), ifDetail);

  const ping = card(t.ping);
  const pingDot = dot("--green");
  const pingHost = h("span", "legend-label");
  const pingValue = qty("legend-value");
  ping.el.append(h("div", "legend", pingDot, pingHost, pingValue.el));

  const pub = card(t.publicIp);
  const pubList = h("div", "ip-list");
  pub.el.append(pubList);

  const local = card(t.localIp);
  const localList = h("div", "ip-list");
  const gateway = legend(null, t.gateway);
  local.el.append(localList, gateway.el);

  const session = card(t.session);
  const sUp = legend("--green", t.upload);
  const sDown = legend("--blue", t.download);
  session.el.append(sUp.el, sDown.el);

  const el = h(
    "div",
    "module",
    traffic.el,
    iface.el,
    ping.el,
    pub.el,
    local.el,
    session.el,
    footer("taskmgr", "eventvwr", "terminal", "netconn", "resmon"),
  );

  let pubShown = "";
  return {
    el,
    update({ snapshot: s, history: hist }) {
      const n = s.net;
      traffic.set(n.up, n.down, hist.netUp, hist.netDown);

      const i = n.interfaces[0];
      if (i) {
        setText(glyph, ifaceGlyph(i));
        setText(ifName, i.kind === "wifi" ? "Wi-Fi" : i.name);
        if (i.wifi) {
          setText(ifRight, i.wifi.ssid || "—");
          ifRight.title = i.wifi.ssid ? "" : t.hiddenSsid;
          const parts = [i.wifi.phy, i.wifi.channel ? `${t.channel} ${i.wifi.channel}` : "", linkSpeed(i.wifi.rxMbps * 1e6), `${t.signal} ${i.wifi.signal}%`];
          setText(ifDetail, parts.filter(Boolean).join(" · "));
        } else {
          setText(ifRight, linkSpeed(i.linkBps));
          setText(ifDetail, i.description);
        }
      } else {
        setText(glyph, ETHERNET);
        setText(ifName, t.notConnected);
        setText(ifRight, "");
        setText(ifDetail, "");
      }

      ping.el.hidden = n.pingMs === null && !n.pingHost;
      setText(pingHost, n.pingHost);
      pingDot.style.background = `var(${n.pingMs === null ? "--red" : n.pingMs > 80 ? "--yellow" : "--green"})`;
      pingValue.set(n.pingMs === null ? { v: "—", u: "" } : { v: int(n.pingMs), u: "ms" });

      lookupPublicIp().then((p) => {
        const key = JSON.stringify(p);
        if (key === pubShown) return;
        pubShown = key;
        setText(pub.right!, p.country ?? "");
        pub.right!.className = p.country ? "card-right badge" : "card-right";
        pubList.replaceChildren(...[p.v4, p.v6].filter(Boolean).map((ip) => h("div", "ip", ip!)));
        pub.el.hidden = !p.v4 && !p.v6;
      });

      // The IPv4 addresses plus one global and one unique-local IPv6. Windows
      // also lists every temporary privacy address, which is noise here.
      const v6 = i ? i.ipv6.filter((a) => !a.startsWith("fe80")) : [];
      const global = v6.find((a) => /^[23]/.test(a));
      const ula = v6.find((a) => /^f[cd]/i.test(a));
      const ips = i ? [...i.ipv4, ...[global, ula].filter((a): a is string => !!a)] : [];
      if (localList.textContent !== ips.join("")) localList.replaceChildren(...ips.map((ip) => h("div", "ip", ip)));
      local.el.hidden = ips.length === 0;
      gateway.el.hidden = !i?.gateway;
      gateway.set({ v: i?.gateway ?? "", u: "" });

      sUp.set(bytes(n.sessionUp));
      sDown.set(bytes(n.sessionDown));
    },
  };
}
