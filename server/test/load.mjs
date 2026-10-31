// MarsScape load test: autonomous clients spread over several rooms, all walking, gathering,
// crafting, fighting and chatting (including "help"/"stop" for the colonists). Reports tick
// jitter as the clients see it, message rates per room, errors, disconnects, and the CPU time the
// local workerd process used (Windows/PowerShell; skipped elsewhere).
//   npx wrangler dev --port 8787
//   node test/load.mjs [ws://127.0.0.1:8787] [clients=30] [rooms=3] [seconds=300]

import { execSync } from "node:child_process";

const BASE = process.argv[2] ?? "ws://127.0.0.1:8787";
const HTTP = BASE.replace(/^ws/, "http");
const N = Number(process.argv[3] ?? 30);
const R = Number(process.argv[4] ?? 3);
const SECS = Number(process.argv[5] ?? 300);
const RUN = Math.random().toString(36).slice(2, 7);
const SITES = Array.from({ length: R }, (_, i) => `load_${RUN}_${i}`);
const BOUNDS = [-300, -300, 300, 300];
const LINES = ["anyone mining ice?", "skitters by the near rift", "gg", "need a medkit", "help", "this crater is huge",
  "brb", "hello", "who wants to group up", "stop", "nice shot", "where is the fabricator"];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cheb = (ax, az, bx, bz) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));
const pct = (a, p) => (a.length ? a[Math.min(a.length - 1, Math.floor((p / 100) * a.length))] : NaN);
const pick = (a) => a[Math.floor(Math.random() * a.length)];

function workerdCpu() {
  if (process.platform !== "win32") return null;
  try {
    const out = execSync(`powershell -NoProfile -Command "(Get-Process workerd -ErrorAction SilentlyContinue | Measure-Object CPU -Sum).Sum"`,
      { encoding: "utf8", timeout: 15000 }).trim();
    return out ? Number(out) : null;
  } catch { return null; }
}

class LoadClient {
  constructor(i) {
    this.i = i;
    this.site = SITES[i % R];
    this.name = `Load ${i}`;
    this.s = { recv: 0, bytes: 0, sent: 0, ticks: [], errs: new Map(), types: new Map(), drops: 0 };
    this.npcs = new Map();
    this.down = new Set();
    this.alive = false;
    this.lastChat = 0;
    this.lastPing = 0;
  }

  connect() {
    return new Promise((resolve) => {
      const ws = new WebSocket(`${BASE}/room/${this.site}`);
      this.ws = ws;
      ws.onopen = () => this.send({ t: "hello", ver: 1, name: this.name, token: this.token ?? "", bounds: BOUNDS, spawn: [0, 0] });
      ws.onerror = () => {};
      ws.onclose = () => {
        if (this.alive && !this.stopping) { this.s.drops++; this.alive = false; setTimeout(() => this.connect(), 1000); }
        resolve(false);
      };
      ws.onmessage = (ev) => {
        this.s.recv++;
        this.s.bytes += ev.data.length;
        const m = JSON.parse(ev.data);
        this.s.types.set(m.t, (this.s.types.get(m.t) ?? 0) + 1);
        switch (m.t) {
          case "welcome":
            this.id = m.id; this.token = m.token; this.nodes = m.nodes; this.tx = m.you.tx; this.tz = m.you.tz;
            this.inv = m.you.inv; this.hp = m.you.hp;
            for (const n of m.npcs) this.npcs.set(n.id, { ...n });
            this.minLevel = Math.min(...m.npcs.map((n) => n.level));
            this.alive = true;
            resolve(true);
            break;
          case "tick":
            this.s.ticks.push(performance.now());
            for (const [id, tx, tz] of m.p) if (id === this.id) { this.tx = tx; this.tz = tz; }
            for (const [id, tx, tz, anim, hp] of m.m ?? []) Object.assign(this.npcs.get(id), { tx, tz, anim, hp, dead: anim === 7 });
            break;
          case "node": if (m.up) this.down.delete(m.id); else this.down.add(m.id); break;
          case "inv": this.inv = m.inv; break;
          case "hp": this.hp = m.hp; break;
          case "err": this.s.errs.set(m.text, (this.s.errs.get(m.text) ?? 0) + 1); break;
        }
      };
    });
  }

  send(m) {
    if (this.ws?.readyState !== 1) return;
    this.ws.send(JSON.stringify(m));
    this.s.sent++;
  }

  count(item) { return this.inv.reduce((a, s) => a + (s && s.item === item ? s.qty : 0), 0); }

  pathTo(tx, tz, stop) {
    const path = [];
    let [x, z] = [this.tx, this.tz];
    while (cheb(x, z, tx, tz) > stop && path.length < 64) { x += Math.sign(tx - x); z += Math.sign(tz - z); path.push([x, z]); }
    return path;
  }

  walk(tx, tz, stop = 0, run = Math.random() < 0.5) {
    const path = this.pathTo(tx, tz, stop);
    if (path.length) this.send({ t: "walk", path, run });
  }

  /** One decision; called every 1-3 s. */
  act() {
    if (!this.alive) return;
    const now = Date.now();
    if (now - this.lastPing > 10000) { this.lastPing = now; this.send({ t: "ping" }); }
    const kit = this.inv.findIndex((s) => s?.item === "medkit");
    if (this.hp <= 3 && kit >= 0) return this.send({ t: "eat", slot: kit });
    if (this.inv.indexOf(null) < 0) return this.send({ t: "drop", slot: Math.floor(Math.random() * 28) });
    const fab = this.nodes[0];
    if (this.count("regolith") >= 2 && Math.random() < 0.5) {
      this.walk(fab.tx, fab.tz, 1);
      return this.send({ t: "craft", recipe: "brick" });
    }
    const r = Math.random();
    if (r < 0.35) {
      const [x0, z0, x1, z1] = BOUNDS;
      const tx = Math.min(x1, Math.max(x0, this.tx + Math.floor(Math.random() * 25) - 12));
      const tz = Math.min(z1, Math.max(z0, this.tz + Math.floor(Math.random() * 25) - 12));
      return this.walk(tx, tz);
    }
    if (r < 0.65) {
      const node = this.nodes.filter((n) => (n.kind === "regolith" || n.kind === "outcrop") && !this.down.has(n.id) && cheb(this.tx, this.tz, n.tx, n.tz) <= 40)
        .sort((a, b) => cheb(this.tx, this.tz, a.tx, a.tz) - cheb(this.tx, this.tz, b.tx, b.tz))[Math.floor(Math.random() * 3)];
      if (node) { this.walk(node.tx, node.tz, 1, false); return this.send({ t: "act", node: node.id }); }
      return;
    }
    if (r < 0.85) {
      const npc = [...this.npcs.values()].filter((n) => n.level === this.minLevel && !n.dead && cheb(this.tx, this.tz, n.tx, n.tz) <= 45)
        .sort((a, b) => cheb(this.tx, this.tz, a.tx, a.tz) - cheb(this.tx, this.tz, b.tx, b.tz))[0];
      if (npc) return this.send({ t: "attack", npc: npc.id });
      return;
    }
    if (now - this.lastChat > 8000) { this.lastChat = now; this.send({ t: "chat", msg: pick(LINES) }); }
  }

  async run(until) {
    while (Date.now() < until) {
      this.act();
      await sleep(1000 + Math.random() * 2000);
    }
    this.stopping = true;
    try { this.ws.close(); } catch {}
  }
}

async function main() {
  console.log(`${N} clients over ${R} rooms for ${SECS} s on ${BASE} (run ${RUN})`);
  const clients = Array.from({ length: N }, (_, i) => new LoadClient(i));
  for (const c of clients) { await c.connect(); await sleep(150); }
  const cpu0 = workerdCpu();
  const t0 = Date.now();
  for (const c of clients) c.s.ticks = [];
  const until = t0 + SECS * 1000;
  for (const c of clients) for (const k of ["recv", "bytes", "sent"]) c.s[k] = 0;
  for (const c of clients) c.s.types = new Map();
  const info = (s) => fetch(`${HTTP}/room/${s}/info`).then((r) => r.json()).catch(() => null);
  const infos0 = await Promise.all(SITES.map(info));
  let infosLate = [];
  const late = setTimeout(async () => { infosLate = await Promise.all(SITES.map(info)); }, SECS * 1000 - 1500);
  const infosMid = [];
  const mid = setTimeout(async () => { for (const s of SITES) infosMid.push(await fetch(`${HTTP}/room/${s}/info`).then((r) => r.json()).catch(() => null)); }, (SECS * 1000) / 2);
  await Promise.all(clients.map((c) => c.run(until)));
  clearTimeout(mid);
  clearTimeout(late);
  const secs = (Date.now() - t0) / 1000;
  const cpu1 = workerdCpu();
  await sleep(2500);
  const infosEnd = [];
  for (const s of SITES) infosEnd.push(await fetch(`${HTTP}/room/${s}/info`).then((r) => r.json()).catch(() => null));

  let allIv = [], totalTicks = 0;
  for (const [ri, site] of SITES.entries()) {
    const cs = clients.filter((c) => c.site === site);
    const iv = [];
    for (const c of cs) for (let k = 1; k < c.s.ticks.length; k++) iv.push(c.s.ticks[k] - c.s.ticks[k - 1]);
    iv.sort((a, b) => a - b);
    allIv = allIv.concat(iv);
    const recv = cs.reduce((a, c) => a + c.s.recv, 0), sent = cs.reduce((a, c) => a + c.s.sent, 0);
    const bytes = cs.reduce((a, c) => a + c.s.bytes, 0);
    const ticks = Math.max(...cs.map((c) => c.s.ticks.length));
    totalTicks += ticks;
    const late = iv.filter((x) => x > 900).length;
    console.log(`room ${ri} (${cs.length} clients, ${infosMid[ri]?.bots ?? "?"} colonists mid-run): tick interval p50 ${pct(iv, 50).toFixed(0)} ms, ` +
      `p99 ${pct(iv, 99).toFixed(0)} ms, max ${iv.length ? iv[iv.length - 1].toFixed(0) : "-"} ms, >900 ms: ${late}/${iv.length}; ` +
      `server->clients ${(recv / secs).toFixed(1)} msg/s (${(bytes / secs / 1024).toFixed(1)} KiB/s), clients->server ${(sent / secs).toFixed(2)} msg/s`);
  }
  allIv.sort((a, b) => a - b);
  const sent = clients.reduce((a, c) => a + c.s.sent, 0);
  const types = new Map();
  for (const c of clients) for (const [t, n] of c.s.types) types.set(t, (types.get(t) ?? 0) + n);
  const errs = new Map();
  for (const c of clients) for (const [t, n] of c.s.errs) errs.set(t, (errs.get(t) ?? 0) + n);
  const drops = clients.reduce((a, c) => a + c.s.drops, 0);
  console.log(`all rooms: tick interval p50 ${pct(allIv, 50).toFixed(0)} ms, p99 ${pct(allIv, 99).toFixed(0)} ms over ${allIv.length} intervals`);
  console.log(`per client: ${(sent / N / secs).toFixed(3)} msg/s sent to the server`);
  console.log(`message mix received: ${[...types].sort((a, b) => b[1] - a[1]).map(([t, n]) => `${t} ${n}`).join(", ")}`);
  console.log(`errors: ${errs.size ? [...errs].map(([t, n]) => `"${t}" x${n}`).join(", ") : "none"}`);
  console.log(`unexpected disconnects: ${drops}`);
  if (cpu0 != null && cpu1 != null) {
    const d = cpu1 - cpu0;
    console.log(`workerd CPU: ${d.toFixed(1)} s over ${secs.toFixed(0)} s wall (${((100 * d) / secs).toFixed(1)}% of one core), ` +
      `${((1000 * d) / Math.max(1, totalTicks)).toFixed(2)} ms per room-tick incl. socket I/O`);
  }
  // SQLite rows written while the run was steady (rooms report rows_written since they started)
  const rows = SITES.map((_, i) => (infos0[i]?.rows_written != null && infosLate[i]?.rows_written != null ? infosLate[i].rows_written - infos0[i].rows_written : null));
  if (rows.every((r) => r != null)) {
    const span = (SECS - 1.5) / 3600;
    console.log(`SQLite rows written: ${rows.map((r, i) => `room ${i} ${r} (${(r / span).toFixed(0)}/h, ${(r / span / (N / R)).toFixed(0)}/h per player)`).join(" | ")}`);
  }
  console.log(`rooms after the run: ${infosEnd.map((i) => (i ? `players ${i.players}, ticking ${i.ticking}, bots ${i.bots}` : "?")).join(" | ")}`);
  process.exit(0);
}

main().catch((e) => { console.error(e); process.exit(1); });
