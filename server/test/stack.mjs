// MarsScape inventory stacking test: everything but gear stacks in one slot. Covers stacking while
// gathering and from loot, crafting from stacks (and a full pack with a part-used stack), eating and
// lamps taking one, partial / "all" / legacy drops, a full pack with an existing stack, part-stack
// trades plus the anti-dupe check, the shop and the locker with stacks, and the merge of old
// one-item-per-slot saves on load.
//   npx wrangler dev --port 8787 --var MARSSCAPE_TEST:1     (the test hook is required)
//   node test/stack.mjs [ws://127.0.0.1:8787]
// Against production (no test hook) add --live: runs only the gathering and drop checks.

const BASE = process.argv[2] && !process.argv[2].startsWith("--") ? process.argv[2] : "ws://127.0.0.1:8787";
const LIVE = process.argv.includes("--live");
const SITE = "stack_" + Math.random().toString(36).slice(2, 8);
const BOUNDS = [-200, -200, 200, 200];
const XP99 = 13034431;

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}`);
  if (!ok) failures++;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cheb = (ax, az, bx, bz) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));

class Client {
  constructor(name, token = "", room = SITE) {
    this.name = name;
    this.token = token;
    this.room = room;
    this.events = [];
    this.waiters = [];
    this.roster = new Map();
    this.npcs = new Map();
  }

  connect() {
    return new Promise((resolve, reject) => {
      this.closed = false;
      this.ws = new WebSocket(`${BASE}/room/${this.room}`);
      this.ws.onopen = () => this.send({ t: "hello", ver: 1, name: this.name, token: this.token, bounds: BOUNDS, spawn: [0, 0] });
      this.ws.onerror = (e) => reject(new Error(`${this.name}: socket error ${e.message ?? ""}`));
      this.ws.onclose = (e) => { this.closed = true; this.closeReason = e.reason; };
      this.ws.onmessage = (ev) => {
        const m = JSON.parse(ev.data);
        this.events.push(m);
        if (m.t === "welcome") {
          this.id = m.id; this.token = m.token; this.welcome = m; this.nodes = m.nodes;
          this.tx = m.you.tx; this.tz = m.you.tz; this.inv = m.you.inv; this.hp = m.you.hp;
          this.roster.clear(); this.npcs.clear();
          for (const p of m.players) this.roster.set(p.id, { ...p, anim: 0 });
          for (const n of m.npcs) this.npcs.set(n.id, { ...n });
          if (m.cave) this.grid = decode(m.cave);
          resolve(m);
        } else if (m.t === "join") this.roster.set(m.id, { ...m, anim: 0 });
        else if (m.t === "leave") this.roster.delete(m.id);
        else if (m.t === "npc_spawn") this.npcs.set(m.npc.id, { ...m.npc });
        else if (m.t === "npc_despawn") this.npcs.delete(m.id);
        else if (m.t === "tick") {
          for (const [id, tx, tz, anim] of m.p) {
            if (id === this.id) { this.tx = tx; this.tz = tz; } else if (this.roster.has(id)) Object.assign(this.roster.get(id), { tx, tz, anim });
          }
          for (const [id, tx, tz, anim, hp] of m.m ?? []) if (this.npcs.has(id)) Object.assign(this.npcs.get(id), { tx, tz, anim, hp });
        } else if (m.t === "inv") this.inv = m.inv;
        else if (m.t === "hp") this.hp = m.hp;
        for (const w of [...this.waiters]) if (w.pred(m)) { this.waiters.splice(this.waiters.indexOf(w), 1); w.resolve(m); }
      };
      setTimeout(() => reject(new Error(`${this.name}: no welcome`)), 10000);
    });
  }

  /** Send; later waits only look at what arrives after the most recent send. */
  send(m) { this.since = this.events.length; this.ws.send(JSON.stringify(m)); }

  wait(pred, ms = 10000) {
    const i = this.events.findIndex((m, k) => k >= (this.since ?? 0) && pred(m));
    if (i >= 0) { const [seen] = this.events.splice(i, 1); return Promise.resolve(seen); }
    return new Promise((resolve) => {
      const w = { pred, resolve };
      this.waiters.push(w);
      setTimeout(() => { const i = this.waiters.indexOf(w); if (i >= 0) { this.waiters.splice(i, 1); resolve(null); } }, ms);
    });
  }

  async until(cond, ms) {
    const end = Date.now() + ms;
    while (Date.now() < end) { if (cond()) return true; await sleep(200); }
    return cond();
  }

  count(item) { return this.inv.reduce((a, s) => a + (s && s.item === item ? s.qty : 0), 0); }
  slot(item) { return this.inv.findIndex((s) => s && s.item === item); }
  node(kind) { return this.nodes.find((n) => n.kind === kind); }
  colonist(role) { return [...this.roster.values()].find((p) => p.bot && p.role === role); }

  /** Straight steps on the surface; breadth-first search in the cave. */
  path(tx, tz, stop) {
    if (!this.grid) {
      const path = [];
      let [x, z] = [this.tx, this.tz];
      while (cheb(x, z, tx, tz) > stop) { x += Math.sign(tx - x); z += Math.sign(tz - z); path.push([x, z]); }
      return path;
    }
    return bfs(this.grid, [this.tx, this.tz], (x, z) => cheb(x, z, tx, tz) <= stop);
  }

  async walkTo(tx, tz, stop = 0, run = true) {
    const path = this.path(tx, tz, stop);
    if (!path) return false;
    if (!path.length) return true;
    for (let i = 0; i < path.length; i += 250) this.send({ t: "walk", path: path.slice(i, i + 250), run });
    return this.until(() => cheb(this.tx, this.tz, tx, tz) <= stop, 400 * (path.length + 10));
  }

  close() { try { this.ws.close(); } catch {} }
}

function decode({ w, h, rle }) {
  const g = new Uint8Array(w * h);
  let i = 0, v = 0;
  for (const run of rle.split(",").map(Number)) { g.fill(v, i, i + run); i += run; v ^= 1; }
  return { w, h, g, floor: (x, z) => x >= 0 && z >= 0 && x < w && z < h && g[z * w + x] === 1, total: i };
}

function bfs(grid, [sx, sz], goal) {
  const key = (x, z) => z * grid.w + x;
  const prev = new Map([[key(sx, sz), null]]);
  const q = [[sx, sz]];
  while (q.length) {
    const [x, z] = q.shift();
    if (goal(x, z)) {
      const out = [];
      let k = key(x, z);
      while (prev.get(k) !== null) { out.unshift([k % grid.w, Math.floor(k / grid.w)]); k = prev.get(k); }
      return out;
    }
    for (let dz = -1; dz <= 1; dz++) for (let dx = -1; dx <= 1; dx++) {
      const nx = x + dx, nz = z + dz;
      if ((!dx && !dz) || prev.has(key(nx, nz)) || !grid.floor(nx, nz)) continue;
      if (dx && dz && (!grid.floor(nx, z) || !grid.floor(x, nz))) continue;
      prev.set(key(nx, nz), key(x, z));
      q.push([nx, nz]);
    }
  }
  return null;
}

function dump(c) {
  console.log(`  ${c.name} at ${c.tx},${c.tz}:`, c.events.filter((m) => m.t !== "tick" && m.t !== "chat" && m.t !== "hit").slice(-8).map((m) => JSON.stringify(m).slice(0, 140)).join(" | "));
}

async function testHook(c, m) {
  c.send({ t: "test", ...m });
  await sleep(300);
}

async function talkTo(c, role) {
  const col = c.colonist(role);
  if (!col) return null;
  await sleep(450); // talk is rate limited to one every 400 ms
  c.send({ t: "talk", id: col.id });
  return c.wait((m) => m.t === "dialog", 20000);
}

/** Talk to a colonist and take quest `qid`, through the quest menu if there is one. Returns the offer dialog. */
async function takeQuest(c, role, qid) {
  let d = await talkTo(c, role);
  if (d?.options.some((o) => o.id === `offer:${qid}`)) {
    c.send({ t: "dialog_choose", option: `offer:${qid}` });
    d = await c.wait((m) => m.t === "dialog", 3000);
  }
  if (d?.quest !== qid || !d.options.some((o) => o.id === "accept")) return null;
  c.send({ t: "dialog_choose", option: "accept" });
  await c.wait((m) => m.t === "quest" && m.id === qid && m.state === "active", 3000);
  return d;
}


const shelf = (shop, item) => shop.stock.find((s) => s.item === item);
const idxOf = (shop, item) => shop.stock.findIndex((s) => s.item === item);

async function openShop(c) {
  await sleep(320);
  c.send({ t: "shop_open" });
  return c.wait((m) => m.t === "shop", 20000);
}

/** Send a shop message and wait for the refreshed window (or null). */
async function shopDo(c, m) {
  await sleep(120);
  c.send(m);
  return c.wait((x) => x.t === "shop", 3000);
}

const slotsOf = (c, item) => c.inv.filter((s) => s && s.item === item).length;
const raw = (list) => Array.from({ length: 28 }, (_, i) => list[i] ?? null);
const one = (item) => ({ item, qty: 1 });
async function invAfter(c, m, ms = 3000) { c.send(m); return c.wait((x) => x.t === "inv", ms); }

async function main() {
  console.log(`site ${SITE} on ${BASE}${LIVE ? " (live: no test hook)" : ""}`);
  const a = new Client("Stacker");
  await a.connect();
  await a.until(() => !!a.colonist("quartermaster"), 5000);
  await a.until(() => a.count("credits") > 0, 20000); // the starter kit lands first, so it can't race the checks below

  // ---- gathering stacks into one slot ------------------------------------------------------
  const before = a.count("regolith");
  const deadline = Date.now() + 90000;
  while (a.count("regolith") < before + 3 && Date.now() < deadline) {
    // the nearest regolith mound that isn't worked out (colonists mine too)
    const down = new Set(a.events.filter((m) => m.t === "node" && !m.up).map((m) => m.id));
    const reg = a.nodes.filter((n) => n.kind === "regolith" && !down.has(n.id))
      .sort((m, n) => cheb(a.tx, a.tz, m.tx, m.tz) - cheb(a.tx, a.tz, n.tx, n.tz))[0];
    await a.walkTo(reg.tx, reg.tz, 1);
    a.send({ t: "act", node: reg.id });
    await a.until(() => a.count("regolith") >= before + 3 || a.events.some((m) => m.t === "node" && m.id === reg.id && !m.up), 25000);
  }
  check(a.count("regolith") >= before + 3 && slotsOf(a, "regolith") === 1, `mined regolith stacks in one slot (${a.count("regolith")} in ${slotsOf(a, "regolith")} slot)`);
  a.send({ t: "walk", path: [[a.tx, a.tz]] });
  await sleep(700);

  // ---- drops: part of a stack, more than the stack --------------------------------------------
  const rs = a.slot("regolith"), have = a.count("regolith");
  let inv = await invAfter(a, { t: "drop", slot: rs, qty: 1 });
  check(inv && a.count("regolith") === have - 1 && a.slot("regolith") === rs, `drop qty 1 takes one from the stack (${have} -> ${a.count("regolith")})`);
  inv = await invAfter(a, { t: "drop", slot: rs, qty: 999 });
  check(inv && a.count("regolith") === 0, "drop qty larger than the stack drops the whole stack");

  if (LIVE) { a.close(); return finish(); }

  // ---- gifts / loot stack; a full pack only refuses new items --------------------------------
  await testHook(a, { rawinv: [] });
  await testHook(a, { give: ["chitin", "chitin", "chitin", "chitin", "chitin"] });
  check(a.count("chitin") === 5 && slotsOf(a, "chitin") === 1, `5 chitin from separate drops share one slot (${slotsOf(a, "chitin")})`);
  await testHook(a, { give: ["pistol", "pistol"] });
  check(slotsOf(a, "pistol") === 2, "gear does not stack (2 blasters in 2 slots)");
  inv = await invAfter(a, { t: "drop", slot: a.slot("chitin") });
  check(inv && a.count("chitin") === 0, "legacy drop (no qty) drops the whole stack");
  await testHook(a, { give: ["chitin", "chitin", "chitin"] });
  inv = await invAfter(a, { t: "drop", slot: a.slot("chitin"), qty: "all" });
  check(inv && a.count("chitin") === 0, "drop \"all\" drops the whole stack");

  const full = raw([{ item: "chitin", qty: 10 }, ...Array.from({ length: 27 }, () => one("pistol"))]);
  await testHook(a, { rawinv: full });
  await testHook(a, { give: ["chitin", "void_crystal"] });
  check(a.count("chitin") === 11 && a.count("void_crystal") === 0, `full pack: chitin joins its stack (11), a new item is refused (${a.count("void_crystal")})`);

  // ---- crafting from stacks ---------------------------------------------------------------
  const fab = a.nodes[0];
  await testHook(a, { rawinv: [] });
  await a.walkTo(fab.tx, fab.tz, 1);
  await testHook(a, { rawinv: [{ item: "regolith", qty: 10 }] });
  a.send({ t: "craft", recipe: "brick" });
  await a.until(() => a.count("brick") === 5, 30000);
  check(a.count("brick") === 5 && a.count("regolith") === 0 && slotsOf(a, "brick") === 1, `crafting uses the regolith stack and stacks the bricks (${a.count("brick")} bricks, ${a.count("regolith")} regolith)`);

  await testHook(a, { rawinv: raw([{ item: "regolith", qty: 10 }, ...Array.from({ length: 27 }, () => one("pistol"))]) });
  a.send({ t: "craft", recipe: "brick" });
  const refusal = await a.wait((m) => m.t === "msg" && /room/i.test(m.text), 8000);
  check(!!refusal && a.count("regolith") === 10 && a.count("brick") === 0, `full pack + part-used stack: craft refused, nothing lost (${a.count("regolith")} regolith)`);

  // ---- using one from a stack -------------------------------------------------------------
  await testHook(a, { rawinv: [{ item: "medkit", qty: 3 }, { item: "xp_lamp", qty: 2 }], hp: 3 });
  inv = await invAfter(a, { t: "eat", slot: 0 });
  check(inv && a.count("medkit") === 2, `eating takes one medkit from the stack (${a.count("medkit")} left)`);
  await sleep(350);
  inv = await invAfter(a, { t: "rub", slot: 1, skill: "mining" });
  check(inv && a.count("xp_lamp") === 1, `rubbing takes one lamp from the stack (${a.count("xp_lamp")} left)`);

  // ---- the locker -------------------------------------------------------------------------
  const locker = a.node("locker");
  await a.walkTo(locker.tx, locker.tz, 1);
  await testHook(a, { rawinv: [{ item: "chitin", qty: 12 }] });
  await sleep(120);
  a.send({ t: "bank_deposit", slot: 0, qty: "all" });
  let bank = await a.wait((m) => m.t === "bank", 3000);
  check(bank && a.count("chitin") === 0 && bank.items.find((s) => s.item === "chitin")?.qty === 12, "the whole stack goes into the locker");
  await sleep(120);
  a.send({ t: "bank_withdraw", idx: bank.items.findIndex((s) => s.item === "chitin"), qty: 5 });
  bank = await a.wait((m) => m.t === "bank", 3000);
  check(bank && a.count("chitin") === 5 && slotsOf(a, "chitin") === 1, `withdraw 5 lands as one stack (${a.count("chitin")})`);

  // ---- the shop with stacks ---------------------------------------------------------------
  await testHook(a, { rawinv: [{ item: "chitin", qty: 10 }], credits: 500 });
  let shop = await openShop(a);
  check(!!shop, "shop opens");
  shop = await shopDo(a, { t: "shop_sell", slot: a.slot("chitin"), qty: 4 });
  check(a.count("chitin") === 6 && slotsOf(a, "credits") === 1, `sell 4 from a stack of 10 leaves ${a.count("chitin")}, credits stay one stack`);
  shop = await shopDo(a, { t: "shop_buy", idx: idxOf(shop, "medkit"), qty: 3 });
  check(a.count("medkit") === 3 && slotsOf(a, "medkit") === 1, `buying 3 medkits makes one stack (${slotsOf(a, "medkit")} slot)`);
  a.send({ t: "shop_close" });

  // ---- part-stack trade + anti-dupe -------------------------------------------------------
  const b = new Client("Partner");
  await b.connect();
  await b.until(() => b.count("credits") > 0, 20000);
  await b.walkTo(a.tx, a.tz, 1);
  async function openTrade() {
    a.send({ t: "trade_request", id: b.id });
    await sleep(600);
    b.send({ t: "trade_request", id: a.id });
    return a.wait((m) => m.t === "trade", 5000);
  }
  await testHook(a, { rawinv: [{ item: "chitin", qty: 10 }] });
  const bChit = b.count("chitin");
  let t = await openTrade();
  check(!!t, "trade window opens");
  a.send({ t: "trade_offer", slot: a.slot("chitin"), qty: 4 });
  t = await a.wait((m) => m.t === "trade" && m.mine?.length, 3000);
  check(t?.mine?.[0]?.qty === 4, "offer 4 out of a stack of 10");
  for (const c of [a, b]) c.send({ t: "trade_accept" });
  await sleep(500);
  for (const c of [a, b]) c.send({ t: "trade_accept" });
  let end = await a.wait((m) => m.t === "trade_end", 5000);
  await b.until(() => b.count("chitin") === bChit + 4, 3000);
  check(end?.result === "done" && a.count("chitin") === 6 && b.count("chitin") === bChit + 4 && slotsOf(b, "chitin") === 1,
    `trade moved 4 (A ${a.count("chitin")}, B ${b.count("chitin")} in ${slotsOf(b, "chitin")} slot)`);

  // A offers 5 of their 6, both accept the first screen, then A tries to drop 3 before the final accept.
  await sleep(600);
  t = await openTrade();
  a.send({ t: "trade_offer", slot: a.slot("chitin"), qty: 5 });
  await a.wait((m) => m.t === "trade" && m.mine?.length, 3000);
  for (const c of [a, b]) c.send({ t: "trade_accept" });
  await sleep(500);
  a.send({ t: "drop", slot: a.slot("chitin"), qty: 3 });
  await sleep(400);
  for (const c of [a, b]) c.send({ t: "trade_accept" });
  end = await a.wait((m) => m.t === "trade_end", 5000);
  await sleep(300);
  const total = a.count("chitin") + b.count("chitin");
  const start = 6 + bChit + 4;
  // dropping is blocked while trading, so either the 5 move and nothing else changes, or the trade is called off
  check(end?.result === "done" ? total === start && b.count("chitin") === bChit + 9 : total <= start,
    `items can't slip out of a pending trade: ${end?.result}, no dupe (A ${a.count("chitin")} + B ${b.count("chitin")} = ${total}, started ${start})`);
  b.close();

  // ---- old saves merge on load ------------------------------------------------------------
  await testHook(a, { rawinv: [one("chitin"), one("chitin"), { item: "credits", qty: 7 }, one("pistol"), one("chitin"), { item: "credits", qty: 3 }, one("pistol")] });
  check(slotsOf(a, "chitin") === 3, "(setup) an old-style pack with 3 separate chitin slots");
  const token = a.token;
  a.close();
  await sleep(1500);
  const a2 = new Client("Stacker", token);
  await a2.connect();
  check(a2.count("chitin") === 3 && slotsOf(a2, "chitin") === 1, `old save: 3 chitin slots merged into one (${slotsOf(a2, "chitin")})`);
  check(a2.count("credits") === 10 && slotsOf(a2, "credits") === 1, `old save: credit stacks merged (${a2.count("credits")})`);
  check(slotsOf(a2, "pistol") === 2, "old save: blasters stay one per slot");
  a2.close();
  finish();
}

function finish() {
  console.log(failures ? `\n${failures} FAILED` : "\nALL PASSED");
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
