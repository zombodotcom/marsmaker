// MarsScape feature test: lava-tube cave (travel, walls, character carried down and back, eviction),
// colony locker (bank), trading (full trade, cancels, anti-dup, space check), colonist quests
// (accept -> progress -> turn in, the 3-step cave story, quest-locked recipes, Insight Lamps),
// armour (equip, defence effect on creature hit rate) and a forced Rift Surge with a 3-player kill.
//   npx wrangler dev --port 8787 --var MARSSCAPE_TEST:1     (the test hook is required)
//   node test/features.mjs [ws://127.0.0.1:8787]
// Needs Node 22+ (global WebSocket). Takes 3-5 minutes. Exits non-zero on failure.

const BASE = process.argv[2] ?? "ws://127.0.0.1:8787";
const SITE = "features_" + Math.random().toString(36).slice(2, 8);
const BOUNDS = [-200, -200, 200, 200];
const XP99 = 13034431, XP70 = 737627, XP60 = 273742, XP45 = 61512, XP40 = 37224;

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

async function main() {
  console.log(`site ${SITE} on ${BASE}`);
  const a = new Client("Ada");
  await a.connect();
  const w = a.welcome;
  check(w.room === "surface" && !!a.node("pit") && !!a.node("locker"), `surface welcome has room, a pit and a locker (pit at ${a.node("pit")?.tx},${a.node("pit")?.tz})`);
  check(Array.isArray(w.quest_defs) && w.quest_defs.length >= 8 && w.armour?.iron_suit && w.you.gear && w.you.quest_points === 0,
    `welcome carries quest_defs (${w.quest_defs?.length}), armour, you.gear, quest points`);
  await testHook(a, { skills: { survey: XP40, mining: XP45, ranged: XP60, defence: XP99, hitpoints: XP99, fabrication: XP70 } });

  // ---- bank ---------------------------------------------------------------------------------------
  await testHook(a, { give: ["chitin", "chitin", "void_crystal", "void_crystal"] });
  const locker = a.node("locker");
  a.send({ t: "bank_deposit", slot: a.slot("chitin"), qty: "all" });
  const far = await a.wait((m) => m.t === "msg" && /locker/.test(m.text), 3000);
  check(!!far && a.count("chitin") === 2, "bank refuses a deposit away from the locker");
  await a.walkTo(locker.tx, locker.tz, 1);
  a.send({ t: "act", node: locker.id });
  const opened = await a.wait((m) => m.t === "bank", 5000);
  check(!!opened && opened.items.length === 0 && opened.slots === 100, "act on the locker opens an empty 100-slot bank");
  a.send({ t: "bank_deposit", slot: a.slot("chitin"), qty: "all" });
  let bank = await a.wait((m) => m.t === "bank", 3000);
  await sleep(200);
  check(bank?.items.find((s) => s.item === "chitin")?.qty === 2 && a.count("chitin") === 0, "deposit all chitin: 2 in the bank, none carried");
  a.send({ t: "bank_deposit", slot: a.slot("void_crystal"), qty: 1 });
  bank = await a.wait((m) => m.t === "bank", 3000);
  await sleep(200);
  check(bank?.items.find((s) => s.item === "void_crystal")?.qty === 1 && a.count("void_crystal") === 1, "deposit 1 of a stack");
  a.send({ t: "bank_withdraw", idx: bank.items.findIndex((s) => s.item === "chitin"), qty: 1 });
  bank = await a.wait((m) => m.t === "bank", 3000);
  await sleep(200);
  check(bank?.items.find((s) => s.item === "chitin")?.qty === 1 && a.count("chitin") === 1, "withdraw 1 chitin");
  a.send({ t: "bank_withdraw", idx: 99, qty: 1 });
  check(!!(await a.wait((m) => m.t === "err", 3000)), "withdraw from an empty locker slot is refused");

  // persistence: reconnect and reopen
  a.close();
  await sleep(600);
  const a1 = new Client("Ada", a.token);
  await a1.connect();
  await a1.walkTo(locker.tx, locker.tz, 1);
  a1.send({ t: "act", node: locker.id });
  bank = await a1.wait((m) => m.t === "bank", 5000);
  check(bank?.items.find((s) => s.item === "chitin")?.qty === 1 && bank?.items.find((s) => s.item === "void_crystal")?.qty === 1,
    "the bank survives a reconnect");

  // ---- trading ------------------------------------------------------------------------------------
  const b = new Client("Bea");
  await b.connect();
  await testHook(b, { give: ["brick", "brick"] });
  await b.walkTo(a1.tx + 2, a1.tz, 0);
  a1.send({ t: "trade_request", id: b.id });
  const req = await b.wait((m) => m.t === "trade_request", 3000);
  check(req?.from === a1.id, "trade request reaches the other player");
  b.send({ t: "trade_request", id: a1.id });
  const ta = await a1.wait((m) => m.t === "trade", 3000), tb = await b.wait((m) => m.t === "trade", 3000);
  check(ta?.stage === 1 && tb?.stage === 1 && ta.with === b.id, "requesting back opens the trade window for both");
  a1.send({ t: "trade_offer", slot: a1.slot("chitin"), qty: 1 });
  await a1.wait((m) => m.t === "trade" && m.mine.length === 1, 3000);
  a1.send({ t: "trade_offer", slot: a1.slot("chitin"), qty: 1 });
  check(!!(await a1.wait((m) => m.t === "err" && /already offered/.test(m.text), 3000)), "can't offer more than you have (anti-dup)");
  a1.send({ t: "drop", slot: a1.slot("chitin") });
  check(!!(await a1.wait((m) => m.t === "msg" && /trade first/.test(m.text), 3000)) && a1.count("chitin") === 1, "dropping during a trade is blocked");
  a1.send({ t: "trade_accept" });
  await a1.wait((m) => m.t === "trade" && m.accepted.me, 3000);
  b.send({ t: "trade_offer", slot: b.slot("brick"), qty: 2 });
  const reset = await a1.wait((m) => m.t === "trade" && m.theirs.length === 1, 3000);
  check(reset && !reset.accepted.me && !reset.accepted.them, "a change to the offer resets both accepts");
  a1.send({ t: "trade_accept" }); b.send({ t: "trade_accept" });
  const conf = await a1.wait((m) => m.t === "trade" && m.stage === 2, 3000);
  check(!!conf, "both accept -> confirm screen (stage 2)");
  b.send({ t: "trade_offer", slot: b.slot("brick"), qty: 1 });
  check(!!(await b.wait((m) => m.t === "err" && /locked/.test(m.text), 3000)), "offers are locked on the confirm screen");
  const bricks0 = a1.count("brick"), bBricks0 = b.count("brick"); // starter kits may have added some
  a1.send({ t: "trade_accept" }); b.send({ t: "trade_accept" });
  const done = await a1.wait((m) => m.t === "trade_end", 3000);
  await sleep(300);
  check(done?.result === "done" && a1.count("brick") === bricks0 + 2 && a1.count("chitin") === 0 && b.count("chitin") === 1 && b.count("brick") === bBricks0 - 2,
    `trade completes: Ada has ${a1.count("brick")} bricks (${bricks0} before), Bea has ${b.count("chitin")} chitin`);

  // decline
  await sleep(600); a1.send({ t: "trade_request", id: b.id }); await sleep(200); b.send({ t: "trade_request", id: a1.id });
  await a1.wait((m) => m.t === "trade", 3000);
  b.send({ t: "trade_decline" });
  const dec = await a1.wait((m) => m.t === "trade_end", 3000);
  check(dec?.result === "cancelled" && /declined/.test(dec.reason), `decline cancels ("${dec?.reason}")`);
  // walking apart
  await sleep(600); a1.send({ t: "trade_request", id: b.id }); await sleep(200); b.send({ t: "trade_request", id: a1.id });
  await a1.wait((m) => m.t === "trade", 3000);
  b.send({ t: "walk", path: Array.from({ length: 14 }, (_, i) => [b.tx + i + 1, b.tz]), run: true });
  const apart = await a1.wait((m) => m.t === "trade_end", 12000);
  check(apart?.result === "cancelled" && /apart/.test(apart.reason), `walking apart cancels ("${apart?.reason}")`);
  await sleep(1500); // let the run finish so the client's position is current
  if (!(await b.walkTo(a1.tx + 2, a1.tz, 0))) { await sleep(1000); await b.walkTo(a1.tx + 2, a1.tz, 0); }
  // inventory space check
  await testHook(b, { give: Array(26).fill("regolith") });
  await testHook(a1, { give: ["sample", "sample", "sample", "sample"] });
  await sleep(600); a1.send({ t: "trade_request", id: b.id }); await sleep(200); b.send({ t: "trade_request", id: a1.id });
  await a1.wait((m) => m.t === "trade", 3000);
  a1.send({ t: "trade_offer", slot: a1.slot("sample"), qty: "all" });
  await a1.wait((m) => m.t === "trade" && m.mine.length === 1, 3000);
  for (const c of [a1, b]) c.send({ t: "trade_accept" });
  await a1.wait((m) => m.t === "trade" && m.stage === 2, 3000);
  for (const c of [a1, b]) c.send({ t: "trade_accept" });
  const full = await a1.wait((m) => m.t === "trade_end", 3000);
  if (!full) dump(a1), dump(b);
  await sleep(200);
  check(full?.result === "cancelled" && /space/.test(full.reason) && a1.count("sample") === 4, `a trade that won't fit is cancelled, nothing moves ("${full?.reason}")`);
  // disconnect
  await sleep(600); a1.send({ t: "trade_request", id: b.id }); await sleep(200); b.send({ t: "trade_request", id: a1.id });
  await a1.wait((m) => m.t === "trade", 3000);
  b.close();
  const gone = await a1.wait((m) => m.t === "trade_end", 5000);
  check(gone?.result === "cancelled", `the other player leaving cancels ("${gone?.reason}")`);

  // ---- quests -------------------------------------------------------------------------------------
  let d = await talkTo(a1, "quartermaster");
  const menu = d?.options.map((o) => o.id) ?? [];
  check(menu.includes("offer:brick_drive") && menu.includes("offer:echoes"), `the quartermaster lists open quests (${menu.join(", ")})`);
  a1.send({ t: "dialog_choose", option: "bye" });
  await a1.wait((m) => m.t === "dialog_end", 3000);
  d = await takeQuest(a1, "quartermaster", "brick_drive");
  check(!!d && /bricks/.test(d.text), `accepting starts "Building Blocks" ("${d?.text?.slice(0, 60)}...")`);
  // quest-locked recipe before the story
  a1.send({ t: "craft", recipe: "relic_helmet" });
  check(!!(await a1.wait((m) => m.t === "msg" && /don't know how/.test(m.text), 3000)), "relic armour can't be made before the quest");
  await testHook(a1, { give: Array(6).fill("regolith") });
  const fab = a1.node("fabricator");
  await a1.walkTo(fab.tx, fab.tz, 1);
  a1.send({ t: "craft", recipe: "brick" });
  const crafted = await a1.wait((m) => m.t === "quest" && m.id === "brick_drive" && m.progress === 3, 30000);
  check(!!crafted, `crafting counts toward the quest (${crafted?.progress}/${crafted?.goal})`);
  await testHook(a1, { give: Array(4).fill("regolith") });
  a1.send({ t: "craft", recipe: "brick" });
  check(!!(await a1.wait((m) => m.t === "quest" && m.id === "brick_drive" && m.progress === 5, 30000)), "5/5 bricks");
  d = await talkTo(a1, "quartermaster");
  check(d?.options.some((o) => o.id === "turn_in"), "the quartermaster asks for the turn-in");
  a1.send({ t: "dialog_choose", option: "turn_in" });
  const fin = await a1.wait((m) => m.t === "quest" && m.id === "brick_drive" && m.state === "done", 3000);
  await sleep(300);
  check(!!fin && fin.qp === 1 && a1.count("xp_lamp") === 1, `quest complete: ${a1.count("xp_lamp")} lamp, ${fin?.qp} quest point`);
  a1.send({ t: "dialog_choose", option: "turn_in" });
  check(!!(await a1.wait((m) => m.t === "err" && /isn't open/.test(m.text), 3000)), "a stale dialog option is refused (no double turn-in)");
  a1.send({ t: "rub", slot: a1.slot("xp_lamp"), skill: "survey" });
  const lamp = await a1.wait((m) => m.t === "xp" && m.skill === "survey", 3000);
  check(lamp?.gain === 100 + 25 * 40 && a1.count("xp_lamp") === 0, `rubbing the lamp: +${lamp?.gain} survey xp`);

  d = await takeQuest(a1, "quartermaster", "echoes");
  check(!!d, `the story quest "Echoes Below" starts ("${d?.text?.slice(0, 60)}...")`);

  // ---- armour -------------------------------------------------------------------------------------
  a1.send({ t: "test", defsim: "golem", n: 40000 });
  const bare = await a1.wait((m) => m.t === "test", 5000);
  await testHook(a1, { give: ["iron_helmet", "iron_suit", "void_shield"] });
  for (const it of ["iron_helmet", "iron_suit", "void_shield"]) {
    a1.send({ t: "equip", slot: a1.slot(it) });
    await a1.wait((m) => m.t === "gear" && Object.values(m).includes(it), 3000);
  }
  a1.send({ t: "test", defsim: "golem", n: 40000 });
  const armed = await a1.wait((m) => m.t === "test", 5000);
  check(armed?.bonus === 67 && armed.hit_rate < bare.hit_rate - 0.05,
    `armour (+${armed?.bonus}) lowers a Rift Golem's hit rate: ${(bare?.hit_rate * 100).toFixed(1)}% -> ${(armed?.hit_rate * 100).toFixed(1)}%`);
  a1.send({ t: "unequip", gear: "helmet" });
  const g = await a1.wait((m) => m.t === "gear" && m.helmet === null, 3000);
  await sleep(200);
  check(!!g && a1.count("iron_helmet") === 1, "unequip the helmet back into the pack");

  // ---- cave -----------------------------------------------------------------------------------------
  const before = { inv: JSON.stringify(a1.inv), xp: a1.welcome.you.skills };
  const pit = a1.node("pit");
  await a1.walkTo(pit.tx, pit.tz, 1);
  const invBefore = JSON.stringify(a1.inv);
  a1.send({ t: "act", node: pit.id });
  const travel = await a1.wait((m) => m.t === "travel", 5000);
  check(travel?.room === `${SITE}~cave`, `the pit sends you to ${travel?.room}`);
  await a1.until(() => a1.closed, 3000);
  const cv = new Client("Ada", a1.token, travel.room);
  await cv.connect();
  check(cv.welcome.room === "cave" && cv.grid?.total === cv.welcome.cave.w * cv.welcome.cave.h && cv.grid.floor(cv.tx, cv.tz),
    `cave welcome: ${cv.welcome.cave.w}x${cv.welcome.cave.h} map, you stand on floor at ${cv.tx},${cv.tz}`);
  check(cv.nodes[0].kind === "ladder" && cv.nodes.some((n) => n.kind === "fossil") && cv.nodes.some((n) => n.kind === "void_vein")
    && cv.welcome.npcs.some((n) => n.kind === "glowworm") && cv.welcome.npcs.some((n) => n.kind === "crystal_horror"),
    "cave has a ladder, fossils, void veins, glowworms and crystal horrors");
  check(JSON.stringify(cv.inv) === invBefore && cv.welcome.you.gear.suit === "iron_suit" && cv.welcome.you.skills.survey >= XP40,
    "inventory, gear and skills came down with you");
  const q1 = await cv.wait((m) => m.t === "quest" && m.id === "echoes", 3000);
  check(q1?.step === 1, `entering the cave advances "Echoes Below" to step ${q1?.step}`);
  // walls
  let wall = null;
  for (const [dx, dz] of [[-1, 0], [0, -1], [0, 1], [1, 1], [-1, -1], [1, -1], [-1, 1]]) {
    if (!cv.grid.floor(cv.tx + dx, cv.tz + dz)) { wall = [cv.tx + dx, cv.tz + dz]; break; }
  }
  if (!wall) { const p = bfs(cv.grid, [cv.tx, cv.tz], (x, z) => [[1, 0], [-1, 0], [0, 1], [0, -1]].some(([dx, dz]) => !cv.grid.floor(x + dx, z + dz))); await cv.walkTo(...p.at(-1), 0); }
  for (const [dx, dz] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) if (!wall && !cv.grid.floor(cv.tx + dx, cv.tz + dz)) wall = [cv.tx + dx, cv.tz + dz];
  cv.send({ t: "walk", path: [wall] });
  const blocked = await cv.wait((m) => m.t === "err" && /can't walk/.test(m.text), 3000);
  check(!!blocked, `walking into a wall at ${wall} is refused`);
  // survey a fossil
  const fos = cv.nodes.filter((n) => n.kind === "fossil").map((n) => ({ n, p: bfs(cv.grid, [cv.tx, cv.tz], (x, z) => cheb(x, z, n.tx, n.tz) <= 1) }))
    .filter((x) => x.p).sort((x, y) => x.p.length - y.p.length)[0];
  await cv.walkTo(fos.n.tx, fos.n.tz, 1);
  cv.send({ t: "act", node: fos.n.id });
  const relic = await cv.until(() => cv.count("ancient_relic") > 0, 90000);
  check(relic, `surveying a fossil (${fos.p.length} tiles away) gives an ancient relic`);
  const q2 = await cv.wait((m) => m.t === "quest" && m.id === "echoes" && m.step === 2, 3000);
  check(!!q2, "the relic advances the story to its deliver step");
  // ladder back up
  const ladder = cv.nodes[0];
  cv.events = [];
  await cv.walkTo(ladder.tx, ladder.tz, 1);
  cv.send({ t: "act", node: ladder.id });
  const up = await cv.wait((m) => m.t === "travel", 5000);
  check(up?.room === SITE, "the ladder sends you back up");
  await cv.until(() => cv.closed, 3000);
  const a2 = new Client("Ada", cv.token);
  await a2.connect();
  check(a2.count("ancient_relic") >= 1 && cheb(a2.tx, a2.tz, pit.tx, pit.tz) <= 1, `back on the surface next to the pit (${a2.tx},${a2.tz}) with the relic`);
  d = await talkTo(a2, "quartermaster");
  check(d?.options.some((o) => o.id === "turn_in"), "the quartermaster asks for the relic");
  a2.send({ t: "dialog_choose", option: "turn_in" });
  const story = await a2.wait((m) => m.t === "quest" && m.id === "echoes" && m.state === "done", 3000);
  await sleep(300);
  check(story?.qp === 4 && a2.count("xp_lamp") === 3, `"Echoes Below" complete: ${story?.qp} quest points, ${a2.count("xp_lamp")} lamps`);
  await testHook(a2, { give: ["ancient_relic", "iron_plate", "iron_plate", "void_crystal", "void_crystal", "void_crystal", "void_crystal"] });
  await a2.walkTo(fab.tx, fab.tz, 1);
  a2.send({ t: "craft", recipe: "relic_helmet" });
  const helm = await a2.until(() => a2.count("relic_helmet") === 1, 15000);
  check(helm, "the quest unlocks relic armour at the fabricator");

  // eviction: in the cave on one socket, then log in on the surface
  await a2.walkTo(pit.tx, pit.tz, 1);
  a2.send({ t: "act", node: pit.id });
  await a2.wait((m) => m.t === "travel", 5000);
  const cv2 = new Client("Ada", a2.token, `${SITE}~cave`);
  await cv2.connect();
  await testHook(cv2, { give: ["chitin"] });
  const chitinDown = cv2.count("chitin");
  const a3 = new Client("Ada", a2.token);
  await a3.connect();
  const kicked = await cv2.until(() => cv2.closed, 4000);
  check(kicked && a3.count("chitin") === chitinDown && cheb(a3.tx, a3.tz, pit.tx, pit.tz) <= 1,
    "logging in on the surface takes the character back from the cave, with what was picked up down there");
  a3.close();

  // ---- Rift Surge -----------------------------------------------------------------------------------
  const team = [new Client("Kai"), new Client("Lux"), new Client("Max")];
  for (const c of team) await c.connect();
  for (const c of team) await testHook(c, { skills: { ranged: XP99, hitpoints: XP99, defence: XP99 }, give: ["plasma_gun"] });
  for (const c of team) { c.send({ t: "equip", slot: c.slot("plasma_gun") }); await c.wait((m) => m.t === "equip", 3000); }
  const [k] = team;
  k.send({ t: "test", surge: "now" });
  const warn = await k.wait((m) => m.t === "event" && m.phase === "warning", 5000);
  const open = await team[1].wait((m) => m.t === "event" && m.phase === "open", 8000);
  const wave = await team[2].wait((m) => m.t === "npc_spawn", 8000);
  check(!!warn && !!open && !!wave, `surge: warning -> open, and creatures pour out (${wave?.npc.kind} at ${wave?.npc.tx},${wave?.npc.tz})`);
  k.send({ t: "test", surge: "boss" });
  const bossEv = await k.wait((m) => m.t === "event" && m.phase === "boss", 5000);
  const spawn = await k.wait((m) => m.t === "npc_spawn" && m.npc.kind === "leviathan", 5000);
  check(!!bossEv && !!spawn, `the Void Leviathan arrives (id ${spawn?.npc.id})`);
  for (const c of team) c.send({ t: "attack", npc: spawn.npc.id });
  const end = await k.wait((m) => m.t === "event" && m.phase === "ended", 120000);
  const loots = await Promise.all(team.map((c) => c.wait((m) => m.t === "msg" && /Surge loot/.test(m.text), 5000)));
  check(!!end && loots.every(Boolean), `three players kill the boss and all three get a loot roll (${loots.map((l) => l?.text.match(/\((\d+)%/)?.[1] + "%").join(", ")})`);
  await sleep(300);
  check(team.every((c) => c.count("void_crystal") >= 3), `surge loot includes void crystals (${team.map((c) => c.count("void_crystal")).join(", ")})`);
  const guardHit = k.events.some((m) => m.t === "hit" && m.dst === `n:${spawn.npc.id}` && [...k.roster.values()].some((p) => p.bot && `p:${p.id}` === m.src));
  check(guardHit, "a colonist guard joined the boss fight");

  for (const c of [...team, a1, a2, a3, cv, cv2]) c.close();
  console.log(failures ? `\n${failures} FAILED` : "\nALL PASSED");
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
