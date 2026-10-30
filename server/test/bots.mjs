// MarsScape server smoke test: bots connect, chat, walk, mine, craft, drop, fight skitters,
// and reconnect with their token to check persistence.
//   node test/bots.mjs [ws://127.0.0.1:8787] [bots=3]
// Needs Node 22+ (global WebSocket). Exits non-zero on failure.

const BASE = process.argv[2] ?? "ws://127.0.0.1:8787";
const N = Number(process.argv[3] ?? 3);
const SITE = "bots_" + Math.random().toString(36).slice(2, 8);
const BOUNDS = [-200, -200, 200, 200];

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}`);
  if (!ok) failures++;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cheb = (ax, az, bx, bz) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));

class Bot {
  constructor(name, token = "") {
    this.name = name;
    this.token = token;
    this.events = [];
    this.waiters = [];
    this.others = new Map();
    this.npcs = new Map();
  }

  connect() {
    return new Promise((resolve, reject) => {
      this.ws = new WebSocket(`${BASE}/room/${SITE}`);
      this.ws.onopen = () => this.send({ t: "hello", ver: 1, name: this.name, token: this.token, bounds: BOUNDS, spawn: [0, 0] });
      this.ws.onerror = (e) => reject(new Error(`${this.name}: socket error ${e.message ?? ""}`));
      this.ws.onclose = () => { this.closed = true; };
      this.ws.onmessage = (ev) => {
        const m = JSON.parse(ev.data);
        this.events.push(m);
        if (m.t === "welcome") {
          this.id = m.id; this.token = m.token; this.you = m.you; this.nodes = m.nodes;
          this.tx = m.you.tx; this.tz = m.you.tz; this.inv = m.you.inv; this.skills = { ...m.you.skills };
          this.hp = m.you.hp; this.weapon = m.you.weapon; this.welcome = m;
          for (const n of m.npcs) this.npcs.set(n.id, { ...n });
          resolve(m);
        } else if (m.t === "tick") {
          for (const [id, tx, tz] of m.p) if (id === this.id) { this.tx = tx; this.tz = tz; } else this.others.set(id, [tx, tz]);
          for (const [id, tx, tz, anim, hp] of m.m ?? []) Object.assign(this.npcs.get(id), { tx, tz, anim, hp });
        } else if (m.t === "inv") this.inv = m.inv;
        else if (m.t === "xp") this.skills[m.skill] = m.xp;
        else if (m.t === "hp") this.hp = m.hp;
        else if (m.t === "equip") this.weapon = m.weapon;
        for (const w of [...this.waiters]) if (w.pred(m)) { this.waiters.splice(this.waiters.indexOf(w), 1); w.resolve(m); }
      };
      setTimeout(() => reject(new Error(`${this.name}: no welcome`)), 10000);
    });
  }

  send(m) { this.ws.send(JSON.stringify(m)); }

  wait(pred, ms = 10000) {
    const seen = this.events.find(pred);
    if (seen) { this.events.splice(this.events.indexOf(seen), 1); return Promise.resolve(seen); }
    return new Promise((resolve) => {
      const w = { pred, resolve };
      this.waiters.push(w);
      setTimeout(() => { const i = this.waiters.indexOf(w); if (i >= 0) { this.waiters.splice(i, 1); resolve(null); } }, ms);
    });
  }

  clearEvents() { this.events = []; }

  /** Straight-line 8-neighbour path to a tile adjacent to (tx, tz). */
  pathNextTo(tx, tz) {
    const path = [];
    let [x, z] = [this.tx, this.tz];
    while (cheb(x, z, tx, tz) > 1) {
      x += Math.sign(tx - x); z += Math.sign(tz - z);
      path.push([x, z]);
    }
    return path;
  }

  async walkNextTo(tx, tz, run = true) {
    const path = this.pathNextTo(tx, tz);
    if (!path.length) return true;
    this.send({ t: "walk", path, run });
    const deadline = Date.now() + 600 * (path.length + 6);
    while (Date.now() < deadline) {
      if (cheb(this.tx, this.tz, tx, tz) <= 1) return true;
      await sleep(150);
    }
    return cheb(this.tx, this.tz, tx, tz) <= 1;
  }

  count(item) { return this.inv.reduce((a, s) => a + (s && s.item === item ? s.qty : 0), 0); }
  close() { try { this.ws.close(); } catch {} }
}

async function main() {
  console.log(`site ${SITE} on ${BASE}, ${N} bots`);
  const health = await fetch(BASE.replace(/^ws/, "http") + "/health").then((r) => r.text()).catch(() => "");
  check(health === "ok", "GET /health returns ok");

  const bots = Array.from({ length: N }, (_, i) => new Bot(`Bot ${i + 1}`));
  await bots[0].connect();
  const w = bots[0].welcome;
  check(w.tick_ms === 600 && w.token.length === 32, "welcome has tick_ms 600 and a token");
  check(w.you.inv.length === 28 && w.you.inv.every((s) => s === null), "new player has 28 empty slots");
  check(w.you.skills.hitpoints === 1154 && w.you.hp === 10 && w.you.max_hp === 10, "hitpoints start at level 10");
  check(w.nodes.some((n) => n.kind === "fabricator" && n.id === 0), "fabricator node id 0 present");
  const kinds = new Set(w.nodes.map((n) => n.kind));
  check(["regolith", "ice", "hematite", "outcrop", "rift"].every((k) => kinds.has(k)), `all node kinds present (${w.nodes.length} nodes)`);
  check(w.npcs.some((n) => n.kind === "skitter"), `skitters present (${w.npcs.length} creatures)`);

  for (const b of bots.slice(1)) await b.connect();
  const join = await bots[0].wait((m) => m.t === "join" && m.id === bots[N - 1].id);
  check(!!join, "first bot sees later bots join");
  check(bots[N - 1].welcome.players.length === N - 1, "welcome lists the players already online");
  check(JSON.stringify(bots[1].welcome.nodes.map((n) => [n.kind, n.tx, n.tz])) === JSON.stringify(w.nodes.map((n) => [n.kind, n.tx, n.tz])),
    "every player gets the same node layout");

  // chat + rate limit + filter
  bots[1].send({ t: "chat", msg: "hello mars\u0007" });
  const chat = await bots[0].wait((m) => m.t === "chat" && m.id === bots[1].id);
  check(chat?.msg === "hello mars", "chat broadcast, control chars stripped");
  bots[1].send({ t: "chat", msg: "again" });
  const rl = await bots[1].wait((m) => m.t === "err" && /fast/.test(m.text), 3000);
  check(!!rl, "chat rate limit (1/s)");

  // bad input
  bots[2 % N].send({ t: "walk", path: [[50, 50]] });
  check(!!(await bots[2 % N].wait((m) => m.t === "err" && /adjacent/.test(m.text), 3000)), "non-adjacent walk rejected");

  // walking broadcasts
  const mover = bots[1];
  mover.send({ t: "walk", path: [[1, 0], [2, 0], [3, 0]] });
  await sleep(2500);
  check(cheb(mover.tx, mover.tz, 3, 0) === 0, "walk advances one tile per tick");
  check(JSON.stringify(bots[0].others.get(mover.id)) === JSON.stringify([3, 0]), "other players see the move in tick updates");

  // mining: regolith until 2 pieces
  const a = bots[0];
  const t0 = Date.now();
  let regolith = 0;
  while (a.count("regolith") < 2 && Date.now() - t0 < 120000) {
    const node = a.nodes
      .filter((n) => n.kind === "regolith")
      .sort((p, q) => cheb(a.tx, a.tz, p.tx, p.tz) - cheb(a.tx, a.tz, q.tx, q.tz))
      .find((n) => !a.events.some((e) => e.t === "node" && e.id === n.id && !e.up));
    await a.walkNextTo(node.tx, node.tz);
    a.send({ t: "act", node: node.id });
    const r = await a.wait((m) => (m.t === "inv") || (m.t === "node" && m.id === node.id && !m.up) || (m.t === "msg" && /stand|worked out/.test(m.text)), 30000);
    if (r?.t === "node") a.events.push(r); // remember depleted
    regolith = a.count("regolith");
  }
  check(regolith >= 2, `mined ${regolith} regolith in ${((Date.now() - t0) / 1000).toFixed(0)} s`);
  check(a.skills.mining >= 36, `mining xp granted (${a.skills.mining})`);

  // craft a brick at the fabricator
  const fab = a.nodes.find((n) => n.kind === "fabricator");
  check(await a.walkNextTo(fab.tx, fab.tz), "walked to the fabricator");
  a.send({ t: "craft", recipe: "brick" });
  await a.wait((m) => m.t === "xp" && m.skill === "fabrication", 8000);
  check(a.count("brick") === 1 && a.count("regolith") === 0, "crafted regolith -> brick");
  a.send({ t: "craft", recipe: "iron_plate" });
  check(!!(await a.wait((m) => m.t === "msg" && /level/.test(m.text), 3000)), "recipe level requirement enforced");

  // drop
  a.clearEvents();
  const slot = a.inv.findIndex((s) => s?.item === "brick");
  a.send({ t: "drop", slot });
  await a.wait((m) => m.t === "inv", 3000);
  check(a.count("brick") === 0, "drop empties the slot");

  // combat: punch a skitter until it dies (or we do)
  const b = bots[N - 1];
  const skitter = [...b.npcs.values()].filter((n) => n.kind === "skitter").sort((p, q) => cheb(0, 0, p.tx, p.tz) - cheb(0, 0, q.tx, q.tz))[0];
  await b.walkNextTo(skitter.tx, skitter.tz);
  b.clearEvents();
  const tc = Date.now();
  b.send({ t: "attack", npc: skitter.id });
  let outcome = null;
  while (!outcome && Date.now() - tc < 150000) {
    outcome = await b.wait((m) => m.t === "death", 5000);
    if (!outcome && !b.events.some((e) => e.t === "hit")) b.send({ t: "attack", npc: skitter.id });
  }
  const hits = b.events.filter((e) => e.t === "hit");
  check(hits.some((h) => h.src === `p:${b.id}`), `player attacks land as hit events (${hits.length} hits)`);
  check(hits.some((h) => h.src === `n:${skitter.id}`) || outcome?.who === `n:${skitter.id}`, "the skitter fights back");
  check(outcome?.who === `n:${skitter.id}`, `skitter killed in ${((Date.now() - tc) / 1000).toFixed(0)} s (outcome ${outcome?.who})`);
  await b.wait((m) => m.t === "msg" && /defeat/.test(m.text), 3000);
  check(b.count("chitin") >= 1, "skitter drops chitin");
  check(b.skills.ranged > 0 && b.skills.hitpoints > 1154, `combat xp (ranged ${b.skills.ranged}, hp ${b.skills.hitpoints})`);
  const seen = await bots[0].wait((m) => m.t === "death" && m.who === `n:${skitter.id}`, 2000);
  check(!!seen, "other players see the kill");

  // persistence across reconnect
  const before = { skills: { ...a.skills }, inv: JSON.stringify(a.inv), token: a.token, tx: a.tx, tz: a.tz };
  a.close();
  check(!!(await bots[1].wait((m) => m.t === "leave" && m.id === a.id)), "leave broadcast on disconnect");
  await sleep(300);
  const a2 = new Bot("Bot 1 again", before.token);
  await a2.connect();
  check(a2.token === before.token, "returning token accepted");
  check(a2.skills.mining === before.skills.mining && a2.skills.fabrication === before.skills.fabrication, "skills persisted across reconnect");
  check(JSON.stringify(a2.inv) === before.inv, "inventory persisted across reconnect");
  check(a2.tx === before.tx && a2.tz === before.tz, "last tile persisted");
  check(a2.you.name === "Bot 1 again", "rename on login");

  // duplicate login kicks the old socket
  const a3 = new Bot("Bot 1 dup", before.token);
  await a3.connect();
  await sleep(500);
  check(a2.closed === true, "second login with the same token kicks the first");

  for (const x of [...bots, a2, a3]) x.close();
  console.log(failures ? `\n${failures} FAILED` : "\nALL PASSED");
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
