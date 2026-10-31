// MarsScape shop test: credits (starter kit, creature drops), the trader colonist and her stall,
// buying / selling / buyback, the price curve, a randomised no-profit-loop property check,
// unsellable items, adjacency, inventory-space and credit checks, anti-dupe races, trades
// closing the shop, and persistence of credits across a reconnect.
//   npx wrangler dev --port 8787 --var MARSSCAPE_TEST:1     (the test hook is required)
//   node test/shop.mjs [ws://127.0.0.1:8787]
// Against production (no test hook) add --live: runs only the parts that need no test hook.

const BASE = process.argv[2] && !process.argv[2].startsWith("--") ? process.argv[2] : "ws://127.0.0.1:8787";
const LIVE = process.argv.includes("--live");
const SITE = "shop_" + Math.random().toString(36).slice(2, 8);
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

async function main() {
  console.log(`site ${SITE} on ${BASE}${LIVE ? " (live: no test hook)" : ""}`);
  const a = new Client("Ada");
  await a.connect();
  const stall = a.node("stall");
  check(!!stall, `the surface has a trader's stall (at ${stall?.tx},${stall?.tz})`);
  await a.until(() => !!a.colonist("trader"), 5000);
  const trader = a.colonist("trader");
  check(!!trader, `a trader colonist is present (${trader?.name})`);
  check(!!trader && cheb(trader.tx, trader.tz, stall.tx, stall.tz) <= 1, "she stands next to her stall");

  // starter kit: walk to the quartermaster at the fabricator
  const fab = a.nodes[0];
  await a.walkTo(fab.tx, fab.tz, 2);
  await a.until(() => a.count("credits") >= 100, 15000);
  check(a.count("credits") === 100, `the starter kit includes 100 credits (have ${a.count("credits")})`);

  // value works anywhere
  await sleep(150);
  a.send({ t: "value", item: "medkit" });
  const val = await a.wait((m) => m.t === "value", 3000);
  check(!!val && val.buy > 40 && val.sell > 0 && val.sell < val.buy, `value: medkit buy ${val?.buy}, sell ${val?.sell}`);

  // walk away, then shop_open walks us over
  await a.walkTo(stall.tx + 12, stall.tz + 12, 0);
  let shop = await openShop(a);
  check(!!shop && cheb(a.tx, a.tz, trader.tx, trader.tz) <= 2, "shop_open from 12 tiles away walks over and opens the shop");
  check(!!shop && ["medkit", "water", "brick", "iron_plate", "chitin_helmet", "pistol", "xp_lamp"].every((i) => shelf(shop, i)),
    `the shelf has the basics (${shop?.stock.map((s) => `${s.item}x${s.qty}@${s.price}`).join(", ")})`);
  check(shop?.credits === 100, "the window shows your credits");
  check(shop?.stock.every((s) => s.sell < s.price || s.price === 0), "every sell price is below its buy price");

  // buy one medkit
  const mk = shelf(shop, "medkit");
  const before = a.count("medkit");
  shop = await shopDo(a, { t: "shop_buy", idx: idxOf(shop, "medkit"), qty: 1 });
  await sleep(150);
  check(a.count("medkit") === before + 1 && a.count("credits") === 100 - mk.price && shelf(shop, "medkit").qty === mk.qty - 1,
    `buy 1 medkit for ${mk.price}: credits ${a.count("credits")}, shelf ${shelf(shop, "medkit")?.qty}`);
  check(shelf(shop, "medkit").price >= mk.price, "the price doesn't drop as stock falls");

  // can't afford
  const lamp = shelf(shop, "xp_lamp");
  const cr0 = a.count("credits");
  await sleep(120);
  a.send({ t: "shop_buy", idx: idxOf(shop, "xp_lamp"), qty: 1 });
  const poor = await a.wait((m) => m.t === "msg" && /afford/.test(m.text), 3000);
  check(!!poor && a.count("credits") === cr0 && a.count("xp_lamp") === 0, `refused: can't afford an Insight Lamp (${lamp.price})`);

  // sell the medkit back: less than paid, goes to buyback
  const sellFor = shop.sell.medkit;
  shop = await shopDo(a, { t: "shop_sell", slot: a.slot("medkit"), qty: 1 });
  await sleep(150);
  check(sellFor > 0 && sellFor < mk.price && a.count("credits") === cr0 + sellFor, `sell it back for ${sellFor} (< ${mk.price})`);
  check(shop?.buyback[0]?.item === "medkit" && shop.buyback[0].price === sellFor && shop.buyback[0].expires_in > 1700, "it's on the buyback list at the price paid");
  const shelfAfterSell = shelf(shop, "medkit").qty;
  shop = await shopDo(a, { t: "shop_buyback", idx: 0 });
  await sleep(150);
  check(a.count("medkit") === before + 1 && a.count("credits") === cr0 && shop?.buyback.length === 0 && shelf(shop, "medkit").qty === shelfAfterSell - 1,
    "buyback returns it for exactly what you were paid, and takes it off the shelf");

  if (!LIVE) {
    // ---- with the test hook --------------------------------------------------------------
    await testHook(a, { credits: 100000, skills: { ranged: XP99, defence: XP99, hitpoints: XP99 } });
    shop = await openShop(a);
    check(shop?.credits === a.count("credits"), `credits from the test hook (${a.count("credits")})`);

    // price curve: buy all the bricks one at a time; each costs at least the one before
    const prices = [];
    for (;;) {
      const s = shelf(shop, "brick");
      if (!s || s.qty <= 0) break;
      prices.push(s.price);
      shop = await shopDo(a, { t: "shop_buy", idx: idxOf(shop, "brick"), qty: 1 });
      if (!shop) break;
    }
    check(prices.length >= 20 && prices.every((p, i) => i === 0 || p >= prices[i - 1]) && prices.at(-1) > prices[0],
      `price rises as stock falls: ${prices[0]} -> ${prices.at(-1)} over ${prices.length} bricks`);
    // sell them all back at once: priced per unit
    const crB = a.count("credits");
    const nb = a.count("brick");
    shop = await shopDo(a, { t: "shop_sell", slot: a.slot("brick"), qty: "all" });
    await sleep(150);
    const got = a.count("credits") - crB;
    const paid = prices.reduce((x, y) => x + y, 0);
    check(a.count("brick") === 0 && got > 0 && got < paid, `sell ${nb} bricks back for ${got} (paid ${paid})`);

    // randomised no-profit property: buy q then sell q, or sell q then buy q, always loses credits
    let ok = true, trials = 0;
    const items = ["medkit", "water", "iron_plate", "chitin_helmet", "pistol"];
    await testHook(a, { give: ["water", "water", "water", "iron_plate", "iron_plate", "iron_plate", "medkit", "medkit"] });
    shop = await openShop(a);
    for (let t = 0; t < 40; t++) {
      const item = items[Math.floor(Math.random() * items.length)];
      const q = 1 + Math.floor(Math.random() * 3);
      const c0 = a.count("credits"), i0 = a.count(item);
      if (Math.random() < 0.5) {
        shop = await shopDo(a, { t: "shop_buy", idx: idxOf(shop, item), qty: q });
        await sleep(100);
        const bought = a.count(item) - i0;
        if (!bought) continue;
        shop = await shopDo(a, { t: "shop_sell", slot: a.slot(item), qty: bought });
      } else {
        if (i0 === 0) continue;
        const qq = Math.min(q, i0);
        shop = await shopDo(a, { t: "shop_sell", slot: a.slot(item), qty: qq });
        shop = await shopDo(a, { t: "shop_buy", idx: idxOf(shop, item), qty: qq });
      }
      await sleep(100);
      trials++;
      if (a.count(item) === i0 && a.count("credits") >= c0) { ok = false; console.log(`  profit loop? ${item} x${q}: ${c0} -> ${a.count("credits")}`); }
    }
    check(ok && trials >= 15, `no profit from any buy/sell round trip (${trials} random round trips)`);

    // unsellable
    await testHook(a, { give: ["rift_blade"] });
    await sleep(120);
    a.send({ t: "shop_sell", slot: a.slot("rift_blade"), qty: 1 });
    const bound = await a.wait((m) => m.t === "msg" && /bound to you/.test(m.text), 3000);
    check(!!bound && a.count("rift_blade") === 1, "the trader won't buy a Rift Blade");
    shop = await openShop(a);
    check(shop?.sell?.rift_blade === 0, "its sell price shows as 0");

    // inventory full: can't buy an unstackable, nothing charged
    const free = a.inv.filter((s) => s === null).length;
    await testHook(a, { give: Array(free).fill("regolith") });
    await sleep(200);
    const crF = a.count("credits");
    await sleep(120);
    a.send({ t: "shop_buy", idx: idxOf(shop, "medkit"), qty: 1 });
    const full = await a.wait((m) => m.t === "msg" && /space/.test(m.text), 3000);
    check(!!full && a.count("credits") === crF, "a full pack refuses the purchase and charges nothing");
    shop = await shopDo(a, { t: "shop_sell", slot: a.slot("regolith"), qty: "all" });
    await sleep(150);
    check(a.count("regolith") === 0, "sold all the regolith (an item she doesn't normally stock)");
    check(!!shelf(shop, "regolith"), "it joins her shelf");

    // anti-dupe: a burst of sells of one slot; pack + shelf are conserved
    await testHook(a, { give: ["water", "water", "water"] });
    shop = await openShop(a);
    const w0 = a.count("water"), s0 = shelf(shop, "water").qty, slot = a.slot("water");
    for (let i = 0; i < 12; i++) a.ws.send(JSON.stringify({ t: "shop_sell", slot, qty: 1 }));
    await sleep(2500);
    shop = await openShop(a);
    check(a.count("water") + shelf(shop, "water").qty === w0 + s0, `burst of sells conserves water (pack ${a.count("water")} + shelf ${shelf(shop, "water").qty} = ${w0 + s0})`);

    // two players race for the last Insight Lamp
    const b = new Client("Bea");
    await b.connect();
    await testHook(b, { credits: 100000 });
    await openShop(b);
    shop = await openShop(a);
    let lamps = shelf(shop, "xp_lamp").qty;
    while (lamps > 1) { shop = await shopDo(a, { t: "shop_buy", idx: idxOf(shop, "xp_lamp"), qty: 1 }); lamps = shelf(shop, "xp_lamp").qty; }
    const la = a.count("xp_lamp"), lb = b.count("xp_lamp");
    await sleep(150);
    a.ws.send(JSON.stringify({ t: "shop_buy", idx: idxOf(shop, "xp_lamp"), qty: 1 }));
    b.ws.send(JSON.stringify({ t: "shop_buy", idx: idxOf(shop, "xp_lamp"), qty: 1 }));
    await sleep(1500);
    check((a.count("xp_lamp") - la) + (b.count("xp_lamp") - lb) === 1, "two players racing for the last lamp: exactly one gets it");
    const sb = b.events.filter((m) => m.t === "shop").at(-1);
    check(!!sb && shelf(sb, "xp_lamp").qty === 0 && shelf(sb, "xp_lamp").price === 0, "the other window sees it sold out");

    // a trade closes the shop
    a.send({ t: "trade_request", id: b.id });
    await sleep(600);
    b.send({ t: "trade_request", id: a.id });
    const end = await a.wait((m) => m.t === "shop_end", 4000);
    check(!!end && /trade/.test(end.reason), `starting a trade closes the shop ("${end?.reason}")`);
    a.send({ t: "trade_decline" });
    await sleep(500);

    // creature kills drop credits
    const sk = [...a.npcs.values()].filter((n) => n.kind === "skitter" && !n.dead)
      .sort((x, y) => cheb(x.tx, x.tz, a.tx, a.tz) - cheb(y.tx, y.tz, a.tx, a.tz))[0];
    if (sk) {
      const c1 = a.count("credits");
      await a.walkTo(sk.tx, sk.tz, 1);
      a.send({ t: "attack", npc: sk.id });
      const loot = await a.wait((m) => m.t === "msg" && /defeat/.test(m.text), 40000);
      await sleep(300);
      check(!!loot && /credits/.test(loot.text) && a.count("credits") > c1, `creatures drop credits (${loot?.text})`);
    } else check(false, "found a skitter to kill");
    b.close();
  }

  // walking away closes the window; buying is then refused
  shop = await openShop(a);
  await a.walkTo(stall.tx + 8, stall.tz + 8, 0);
  const gone = await a.wait((m) => m.t === "shop_end", 4000);
  check(!!gone, `walking away closes the shop ("${gone?.reason}")`);
  const crW = a.count("credits");
  await sleep(150);
  a.send({ t: "shop_buy", idx: 0, qty: 1 });
  await sleep(600);
  check(a.count("credits") === crW, "a buy after walking away is refused");

  // persistence: credits survive a reconnect
  const keep = a.count("credits");
  a.close();
  await sleep(1500);
  const a2 = new Client("Ada", a.token);
  await a2.connect();
  check(a2.count("credits") === keep, `credits persist across a reconnect (${keep})`);
  a2.close();

  console.log(failures ? `\n${failures} FAILED` : "\nALL PASSED");
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
