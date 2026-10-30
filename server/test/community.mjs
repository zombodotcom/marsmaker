// MarsScape colonist test: the room's NPC colonists greet, hand out a one-time starter kit, work
// nodes, escort on "help", fight for the player, heal, scale with the human count, and let the room
// stop ticking when the last human leaves.
//   npx wrangler dev --port 8787 --var MARSSCAPE_TEST:1     (the test hook is needed for the medic)
//   node test/community.mjs [ws://127.0.0.1:8787]
// Needs Node 22+ (global WebSocket). Takes about 2 minutes. Exits non-zero on failure.

const BASE = process.argv[2] ?? "ws://127.0.0.1:8787";
const HTTP = BASE.replace(/^ws/, "http");
const SITE = "community_" + Math.random().toString(36).slice(2, 8);
const BOUNDS = [-200, -200, 200, 200];

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}`);
  if (!ok) failures++;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cheb = (ax, az, bx, bz) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));
const info = () => fetch(`${HTTP}/room/${SITE}/info`).then((r) => r.json());

class Client {
  constructor(name, token = "") {
    this.name = name;
    this.token = token;
    this.events = [];
    this.waiters = [];
    this.roster = new Map(); // id -> {name, bot, role, tx, tz, anim}
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
          this.id = m.id; this.token = m.token; this.welcome = m; this.nodes = m.nodes;
          this.tx = m.you.tx; this.tz = m.you.tz; this.inv = m.you.inv; this.hp = m.you.hp;
          for (const p of m.players) this.roster.set(p.id, { ...p, anim: 0 });
          for (const n of m.npcs) this.npcs.set(n.id, { ...n });
          resolve(m);
        } else if (m.t === "join") this.roster.set(m.id, { ...m, anim: 0 });
        else if (m.t === "leave") this.roster.delete(m.id);
        else if (m.t === "tick") {
          for (const [id, tx, tz, anim] of m.p) {
            if (id === this.id) { this.tx = tx; this.tz = tz; } else if (this.roster.has(id)) Object.assign(this.roster.get(id), { tx, tz, anim });
          }
          for (const [id, tx, tz, anim, hp] of m.m ?? []) Object.assign(this.npcs.get(id), { tx, tz, anim, hp });
        } else if (m.t === "inv") this.inv = m.inv;
        else if (m.t === "hp") this.hp = m.hp;
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

  /** Poll a condition on this client's view of the world. */
  async until(cond, ms) {
    const end = Date.now() + ms;
    while (Date.now() < end) { if (cond()) return true; await sleep(200); }
    return cond();
  }

  bots() { return [...this.roster.values()].filter((p) => p.bot); }
  botChat(re) { return (m) => m.t === "chat" && this.roster.get(m.id)?.bot && re.test(m.msg); }
  count(item) { return this.inv.reduce((a, s) => a + (s && s.item === item ? s.qty : 0), 0); }

  async walkTo(tx, tz, stop = 0, run = true) {
    const path = [];
    let [x, z] = [this.tx, this.tz];
    while (cheb(x, z, tx, tz) > stop) { x += Math.sign(tx - x); z += Math.sign(tz - z); path.push([x, z]); }
    if (!path.length) return true;
    this.send({ t: "walk", path, run });
    return this.until(() => cheb(this.tx, this.tz, tx, tz) <= stop, 600 * (path.length + 6));
  }

  close() { try { this.ws.close(); } catch {} }
}

async function main() {
  console.log(`site ${SITE} on ${BASE}`);

  // ---- one human: four colonists, greeting, tip, starter kit ----------------------------
  const a = new Client("Ada");
  await a.connect();
  const cols = a.welcome.players.filter((p) => p.bot);
  const roles = cols.map((p) => p.role).sort();
  check(cols.length === 4, `welcome lists 4 colonists for 1 human (${cols.map((p) => `${p.name}:${p.role}`).join(", ")})`);
  check(["quartermaster", "guard", "medic"].every((r) => roles.includes(r)) && roles.some((r) => r === "miner" || r === "surveyor"),
    "colonist roles: quartermaster, guard, medic + a miner or surveyor");
  check(new Set(cols.map((p) => p.name)).size === cols.length && cols.every((p) => p.name), "colonists have distinct names");

  const hi = await a.wait(a.botChat(/Welcome/), 8000);
  check(!!hi && hi.msg.includes("Ada"), `a colonist greets the new player ("${hi?.msg}")`);
  const tip = await a.wait(a.botChat(/^Tip:/), 15000);
  check(!!tip, `a colonist gives a tip ("${tip?.msg}")`);

  let kit = await a.wait((m) => m.t === "msg" && /starter kit/.test(m.text), 6000);
  if (!kit) {
    const fab = a.nodes.find((n) => n.kind === "fabricator");
    await a.walkTo(fab.tx, fab.tz, 1);
    kit = await a.wait((m) => m.t === "msg" && /starter kit/.test(m.text), 8000);
  }
  await sleep(300);
  check(!!kit && a.count("brick") === 3 && a.count("medkit") === 1, `quartermaster hands over the starter kit (${a.count("brick")} bricks, ${a.count("medkit")} medkit)`);

  const worker = cols.find((p) => p.role === "miner" || p.role === "surveyor");
  const working = await a.until(() => [3, 4].includes(a.roster.get(worker.id)?.anim), 40000);
  check(working, `the ${worker.role} colonist works a node (anim ${a.roster.get(worker.id)?.anim})`);

  // ---- the kit is one-time per character -----------------------------------------------
  a.close();
  await sleep(500);
  const a2 = new Client("Ada", a.token);
  await a2.connect();
  const back = await a2.wait(a2.botChat(/Welcome back/), 8000);
  check(!!back, `returning player is welcomed back ("${back?.msg}")`);
  const again = await a2.wait((m) => m.t === "msg" && /starter kit/.test(m.text), 6000);
  check(!again && a2.count("brick") === 3, "no second starter kit for the same character");

  // ---- "help": nearest guard escorts and fights --------------------------------------------
  a2.send({ t: "chat", msg: "help" });
  const omw = await a2.wait(a2.botChat(/On my way/), 6000);
  const escortId = omw?.id;
  check(!!omw && a2.roster.get(escortId)?.role === "guard", `"help" -> the guard answers ("${omw?.msg}")`);
  const close = await a2.until(() => {
    const e = a2.roster.get(escortId);
    return e && cheb(e.tx, e.tz, a2.tx, a2.tz) <= 3;
  }, 25000);
  check(close, "the escort comes to the player");

  const rift = a2.nodes.filter((n) => n.kind === "rift").sort((p, q) => cheb(0, 0, p.tx, p.tz) - cheb(0, 0, q.tx, q.tz))[0];
  await a2.walkTo(rift.tx, rift.tz, 2);
  const shot = await a2.wait((m) => m.t === "hit" && m.src === `p:${escortId}` && m.dst.startsWith("n:"), 45000);
  check(!!shot, `the escort fights the creature that attacks the player (${shot ? `hit ${shot.dst} for ${shot.dmg}` : "no hit"})`);

  a2.send({ t: "chat", msg: "ok stop" });
  const bye = await a2.wait((m) => m.t === "chat" && m.id === escortId && /post/.test(m.msg), 6000);
  check(!!bye, `"stop" releases the escort ("${bye?.msg}")`);

  // ---- the medic heals a hurt player --------------------------------------------------------
  await a2.walkTo(0, 0, 1);
  await sleep(1500);
  a2.events = []; // only count heals that come after the hp drop
  a2.send({ t: "test", hp: 1 });
  const set = await a2.wait((m) => (m.t === "hp" && m.hp === 1) || (m.t === "err" && /Unknown/.test(m.text)), 4000);
  if (set?.t === "err") {
    check(false, "test hook unavailable: run wrangler dev with --var MARSSCAPE_TEST:1");
  } else {
    const heal = await a2.wait((m) => m.t === "heal" && m.dst === `p:${a2.id}`, 30000);
    const medic = heal && a2.roster.get(Number(heal.src.slice(2)));
    check(!!heal && heal.amt === 5 && medic?.role === "medic", `the medic heals the hurt player (+${heal?.amt} from ${medic?.name}:${medic?.role})`);
    await sleep(300);
    check(a2.hp >= 6, `hp after the heal: ${a2.hp}`);
  }

  // ---- colonists make room as humans arrive, and come back when they leave -----------------
  const more = [];
  for (const [i, want] of [[0, 3], [1, 2], [2, 1]]) {
    const c = new Client(`Extra ${i + 1}`);
    await c.connect();
    more.push(c);
    const n = c.welcome.players.filter((p) => p.bot).length;
    check(n === want, `${2 + i} humans -> ${want} colonist${want > 1 ? "s" : ""} (welcome lists ${n})`);
  }
  await sleep(1000);
  check(a2.bots().length === 1, `existing players see the colonists leave (${a2.bots().length} left)`);
  for (const c of more) c.close();
  const refilled = await a2.until(() => a2.bots().length === 4, 15000);
  check(refilled, `colonists come back when the extra humans leave (${a2.bots().length})`);
  const rejoin = a2.events.filter((m) => m.t === "join" && m.bot);
  check(rejoin.length >= 3 && rejoin.every((m) => m.role), "colonist joins carry bot:true and a role");

  // ---- the room stops ticking when the last human leaves ------------------------------------
  a2.close();
  await sleep(2000);
  const i1 = await info();
  await sleep(2500);
  const i2 = await info();
  check(i1.players === 0 && i1.ticking === false && i2.tick === i1.tick,
    `room stops ticking with no humans (ticking ${i1.ticking}, tick ${i1.tick} -> ${i2.tick}, bots in memory ${i2.bots})`);

  console.log(failures ? `\n${failures} FAILED` : "\nALL PASSED");
  process.exit(failures ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
