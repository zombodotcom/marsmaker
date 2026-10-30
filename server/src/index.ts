// MarsScape server: a Cloudflare Worker that routes /room/<site_id> to one Durable Object per
// Mars site. The room runs a 600 ms tick loop, validates all movement/actions, and keeps player
// progress in the Durable Object's SQLite storage. Protocol: see PROTOCOL.md.

import { DurableObject } from "cloudflare:workers";
import {
  TICK_MS, INV_SLOTS, MAX_XP, SKILLS, Skill, Slot, ITEMS, NODE_DEFS, RECIPES, Recipe, GATHER_TICKS, XP_TABLE,
  Bounds, WorldNode, levelForXp, gatherChance, generateNodes, clampBounds, clampToBounds, inBounds,
  cleanName, cleanChat, GatherKind, HP_START_XP, WEAPONS, combatLevel, hitChance, maxHit, NPC_DEFS, NpcDef,
  NPC_RESPAWN, AGGRO_RANGE, LEASH_RANGE, WANDER_RANGE, WorldNpc, generateRifts,
} from "./game";

export interface Env {
  ROOMS: DurableObjectNamespace<Room>;
}

const PROTOCOL_VER = 1;
const MAX_PLAYERS = 100;
const MAX_FRAME = 4096;          // bytes per client frame
const MAX_PATH = 256;            // tiles per walk request
const MSGS_PER_SEC = 20;         // hard flood limit, closes the socket
const IDLE_MS = 20 * 60 * 1000;  // no input (pings don't count) -> logout
const HELLO_MS = 15_000;         // must say hello within this
const FULL_SYNC_TICKS = 10;      // full position list every N ticks
const SAVE_TICKS = 10;           // persist dirty players every N ticks

const REGEN_TICKS = 10;          // +1 hp every N ticks
const MEDKIT_HP = 20;

const ANIM = { idle: 0, walk: 1, run: 2, mine: 3, survey: 4, craft: 5, attack: 6, dead: 7 } as const;

type Action =
  | { kind: "gather"; node: number; timer: number }
  | { kind: "craft"; recipe: Recipe; timer: number }
  | { kind: "attack"; npc: number };

interface Player {
  ws: WebSocket;
  id: number;
  token: string;
  name: string;
  tx: number;
  tz: number;
  skills: Record<Skill, number>;
  inv: Slot[];
  path: [number, number][];
  run: boolean;
  action: Action | null;
  anim: number;
  sent: string;        // last "tx,tz,anim" broadcast
  dirty: boolean;
  lastChat: number;
  lastInput: number;
  hp: number;
  weapon: string | null;
  nextAttack: number;  // tick
  nextEat: number;     // tick
}

interface Npc {
  id: number;
  kind: WorldNpc["kind"];
  def: NpcDef;
  home: [number, number];
  tx: number;
  tz: number;
  hp: number;
  target: Player | null;
  nextAttack: number;
  respawnAt: number;   // 0 = alive
  leash: boolean;      // walking home after a chase
  anim: number;
  sent: string;
}

const dist = (ax: number, az: number, bx: number, bz: number) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));
const hpLevel = (p: Player) => levelForXp(p.skills.hitpoints);
const cbLevel = (p: Player) => combatLevel(levelForXp(p.skills.ranged), levelForXp(p.skills.defence), hpLevel(p));

interface Conn {
  // per-socket bookkeeping that is not game state
  opened: number;
  window: number;
  count: number;
  badJson: number;
}

interface Attachment {
  token?: string;
  id?: number;
}

function sanitizeSite(s: string): string | null {
  try { s = decodeURIComponent(s); } catch { return null; }
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(s)) return null;
  return s;
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    const cors = { "Access-Control-Allow-Origin": "*" };
    if (url.pathname === "/health" || url.pathname === "/") {
      return new Response(url.pathname === "/" ? "MarsScape server. Connect to /room/<site_id> over WebSocket.\n" : "ok", {
        headers: { "Content-Type": "text/plain", ...cors },
      });
    }
    const m = url.pathname.match(/^\/room\/([^/]+)(\/info)?$/);
    if (!m) return new Response("not found", { status: 404, headers: cors });
    const site = sanitizeSite(m[1]);
    if (!site) return new Response("bad site id", { status: 400, headers: cors });
    const stub = env.ROOMS.get(env.ROOMS.idFromName(site));
    if (m[2]) {
      return stub.fetch(new Request("https://room/info", { headers: { "X-Site": site } }));
    }
    if (req.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected a WebSocket upgrade", { status: 426, headers: cors });
    }
    const fwd = new Request(req.url, req);
    fwd.headers.set("X-Site", site);
    return stub.fetch(fwd);
  },
} satisfies ExportedHandler<Env>;

export class Room extends DurableObject<Env> {
  sql: SqlStorage;
  site = "";
  bounds: Bounds | null = null;
  spawn: [number, number] = [0, 0];
  nodes: WorldNode[] = [];
  nodeUp = new Map<number, number>(); // node id -> tick when it respawns (absent = up)
  npcs: Npc[] = [];
  players = new Map<WebSocket, Player>();
  conns = new Map<WebSocket, Conn>();
  nextId = 1;
  tickN = 0;
  timer: ReturnType<typeof setInterval> | null = null;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)`);
    this.sql.exec(`CREATE TABLE IF NOT EXISTS players (
      token TEXT PRIMARY KEY, name TEXT NOT NULL, skills TEXT NOT NULL, inv TEXT NOT NULL,
      tx INTEGER NOT NULL, tz INTEGER NOT NULL, updated INTEGER NOT NULL)`);
    // added with combat; ignore "duplicate column" on existing rooms
    try { this.sql.exec(`ALTER TABLE players ADD COLUMN hp INTEGER`); } catch { /* exists */ }
    try { this.sql.exec(`ALTER TABLE players ADD COLUMN weapon TEXT`); } catch { /* exists */ }
    const meta = new Map<string, string>();
    for (const r of this.sql.exec<{ k: string; v: string }>(`SELECT k, v FROM meta`)) meta.set(r.k, r.v);
    this.site = meta.get("site") ?? "";
    this.nextId = Number(meta.get("next_id") ?? 1);
    this.tickN = Number(meta.get("tick") ?? 0);
    if (meta.has("bounds")) {
      this.bounds = JSON.parse(meta.get("bounds")!);
      this.spawn = JSON.parse(meta.get("spawn")!);
      this.initWorld();
    }
    // Woken from hibernation: rebuild players from the sockets that are still open.
    for (const ws of ctx.getWebSockets()) {
      const a = (ws.deserializeAttachment() ?? {}) as Attachment;
      this.conns.set(ws, { opened: Date.now(), window: 0, count: 0, badJson: 0 });
      if (a.token && a.id) {
        const p = this.loadPlayer(ws, a.token, a.id, "");
        if (p) this.players.set(ws, p);
      }
    }
    if (this.players.size) this.startLoop();
  }

  initWorld() {
    this.nodes = generateNodes(this.site, this.bounds!, this.spawn);
    this.npcs = generateRifts(this.site, this.bounds!, this.spawn, this.nodes).map((n) => ({
      id: n.id, kind: n.kind, def: NPC_DEFS[n.kind], home: n.home, tx: n.home[0], tz: n.home[1],
      hp: NPC_DEFS[n.kind].hp, target: null, nextAttack: 0, respawnAt: 0, leash: false, anim: ANIM.idle, sent: "",
    }));
  }

  npcRow(n: Npc) {
    return { id: n.id, kind: n.kind, tx: n.tx, tz: n.tz, hp: n.hp, max: n.def.hp, level: n.def.level, dead: n.respawnAt > 0 };
  }

  setMeta(k: string, v: string) {
    this.sql.exec(`INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v`, k, v);
  }

  async fetch(req: Request): Promise<Response> {
    const site = req.headers.get("X-Site") ?? "";
    if (!this.site && site) { this.site = site; this.setMeta("site", site); }
    if (new URL(req.url).pathname === "/info") {
      return Response.json({ site: this.site, players: this.players.size, max: MAX_PLAYERS, bounds: this.bounds }, {
        headers: { "Access-Control-Allow-Origin": "*" },
      });
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment({} satisfies Attachment);
    this.conns.set(server, { opened: Date.now(), window: 0, count: 0, badJson: 0 });
    if (!this.timer) this.startLoop(); // also enforces the hello timeout
    return new Response(null, { status: 101, webSocket: client });
  }

  // ---- socket plumbing ------------------------------------------------------
  send(ws: WebSocket, msg: object) {
    try { ws.send(JSON.stringify(msg)); } catch { /* closed */ }
  }

  broadcast(msg: object, except?: WebSocket) {
    const s = JSON.stringify(msg);
    for (const ws of this.players.keys()) {
      if (ws === except) continue;
      try { ws.send(s); } catch { /* closed */ }
    }
  }

  kick(ws: WebSocket, text: string, code = 1008) {
    this.send(ws, { t: "err", text });
    try { ws.close(code, text.slice(0, 100)); } catch { /* already closed */ }
    this.dropSocket(ws);
  }

  dropSocket(ws: WebSocket) {
    const p = this.players.get(ws);
    this.conns.delete(ws);
    if (!p) return;
    this.players.delete(ws);
    for (const n of this.npcs) if (n.target === p) n.target = null;
    this.savePlayer(p);
    this.broadcast({ t: "leave", id: p.id });
  }

  async webSocketClose(ws: WebSocket) {
    this.dropSocket(ws);
    try { ws.close(1000, "bye"); } catch { /* already closed */ }
  }

  async webSocketError(ws: WebSocket) {
    this.dropSocket(ws);
  }

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer) {
    let conn = this.conns.get(ws);
    if (!conn) { conn = { opened: Date.now(), window: 0, count: 0, badJson: 0 }; this.conns.set(ws, conn); }
    const now = Date.now();
    if (now - conn.window > 1000) { conn.window = now; conn.count = 0; }
    if (++conn.count > MSGS_PER_SEC) return this.kick(ws, "Too many messages.");
    if (typeof raw !== "string") return this.kick(ws, "Binary frames are not supported.", 1003);
    if (raw.length > MAX_FRAME) return this.kick(ws, "Message too large.", 1009);
    let msg: any;
    try { msg = JSON.parse(raw); } catch { msg = null; }
    if (!msg || typeof msg !== "object" || typeof msg.t !== "string") {
      if (++conn.badJson > 5) return this.kick(ws, "Malformed messages.");
      return this.send(ws, { t: "err", text: "Malformed message." });
    }
    const p = this.players.get(ws);
    if (!p) {
      if (msg.t === "hello") return this.onHello(ws, msg);
      if (msg.t === "ping") return this.send(ws, { t: "pong" });
      return this.send(ws, { t: "err", text: "Say hello first." });
    }
    if (msg.t !== "ping") p.lastInput = now;
    switch (msg.t) {
      case "ping": return this.send(ws, { t: "pong", n: this.tickN });
      case "hello": return this.send(ws, { t: "err", text: "Already logged in." });
      case "walk": return this.onWalk(p, msg);
      case "act": return this.onAct(p, msg);
      case "craft": return this.onCraft(p, msg);
      case "drop": return this.onDrop(p, msg);
      case "chat": return this.onChat(p, msg);
      case "attack": return this.onAttack(p, msg);
      case "equip": return this.onEquip(p, msg);
      case "unequip": return this.onUnequip(p);
      case "eat": return this.onEat(p, msg);
      default: return this.send(ws, { t: "err", text: `Unknown message type "${String(msg.t).slice(0, 20)}".` });
    }
  }

  // ---- persistence ----------------------------------------------------------
  loadPlayer(ws: WebSocket, token: string, id: number, name: string): Player | null {
    const rows = this.sql.exec<{ name: string; skills: string; inv: string; tx: number; tz: number; hp: number | null; weapon: string | null }>(
      `SELECT name, skills, inv, tx, tz, hp, weapon FROM players WHERE token = ?`, token).toArray();
    if (!rows.length) return null;
    const r = rows[0];
    const skills = Object.fromEntries(SKILLS.map((s) => [s, 0])) as Record<Skill, number>;
    try { Object.assign(skills, JSON.parse(r.skills)); } catch { /* keep zeros */ }
    skills.hitpoints = Math.max(HP_START_XP, skills.hitpoints);
    let inv: Slot[] = [];
    try { inv = JSON.parse(r.inv); } catch { /* empty */ }
    inv = Array.from({ length: INV_SLOTS }, (_, i) => {
      const s = inv[i];
      return s && typeof s.item === "string" && ITEMS[s.item] && s.qty > 0 ? { item: s.item, qty: Math.floor(s.qty) } : null;
    });
    return this.fixHp({
      ws, id, token, name: name || r.name, tx: r.tx, tz: r.tz, skills, inv, path: [], run: false, action: null,
      anim: ANIM.idle, sent: "", dirty: false, lastChat: 0, lastInput: Date.now(),
      hp: r.hp ?? 0, weapon: r.weapon && WEAPONS[r.weapon] ? r.weapon : null, nextAttack: 0, nextEat: 0,
    });
  }

  fixHp(p: Player): Player {
    const max = hpLevel(p);
    if (!(p.hp > 0) || p.hp > max) p.hp = max;
    return p;
  }

  savePlayer(p: Player) {
    this.sql.exec(
      `INSERT INTO players (token, name, skills, inv, tx, tz, updated, hp, weapon) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(token) DO UPDATE SET name = excluded.name, skills = excluded.skills, inv = excluded.inv,
       tx = excluded.tx, tz = excluded.tz, updated = excluded.updated, hp = excluded.hp, weapon = excluded.weapon`,
      p.token, p.name, JSON.stringify(p.skills), JSON.stringify(p.inv), p.tx, p.tz, Date.now(), p.hp, p.weapon);
    p.dirty = false;
  }

  // ---- handlers ---------------------------------------------------------------
  onHello(ws: WebSocket, msg: any) {
    if (msg.ver !== PROTOCOL_VER) return this.kick(ws, `Protocol version ${PROTOCOL_VER} required.`, 1002);
    if (this.players.size >= MAX_PLAYERS) return this.kick(ws, "This site is full. Try another one.", 1013);

    // The first visitor's bounds and spawn fix the room's world for its lifetime.
    if (!this.bounds) {
      const b = clampBounds(msg.bounds);
      if (!b) return this.kick(ws, "Missing or bad bounds.", 1002);
      const sp = Array.isArray(msg.spawn) ? msg.spawn.map((v: unknown) => Math.floor(Number(v))) : [];
      const spawn = sp.length === 2 && sp.every(Number.isFinite)
        ? clampToBounds(b, sp[0], sp[1])
        : [Math.floor((b[0] + b[2]) / 2), Math.floor((b[1] + b[3]) / 2)] as [number, number];
      this.bounds = b;
      this.spawn = spawn;
      this.initWorld();
      this.setMeta("bounds", JSON.stringify(b));
      this.setMeta("spawn", JSON.stringify(spawn));
    }
    const bounds = this.bounds!;

    let token = typeof msg.token === "string" && /^[a-f0-9]{32}$/.test(msg.token) ? msg.token : "";
    const id = this.nextId++;
    this.setMeta("next_id", String(this.nextId));
    // Same account already online here: the new login wins (kick first so its state is saved).
    for (const [ows, op] of [...this.players]) {
      if (token && op.token === token) this.kick(ows, "Logged in from another location.", 1000);
    }
    let p = token ? this.loadPlayer(ws, token, id, "") : null;
    if (!p) {
      token = crypto.randomUUID().replace(/-/g, "");
      p = {
        ws, id, token, name: "", tx: this.spawn[0], tz: this.spawn[1],
        skills: Object.fromEntries(SKILLS.map((s) => [s, 0])) as Record<Skill, number>,
        inv: Array(INV_SLOTS).fill(null), path: [], run: false, action: null, anim: ANIM.idle,
        sent: "", dirty: true, lastChat: 0, lastInput: Date.now(),
        hp: 10, weapon: null, nextAttack: 0, nextEat: 0,
      };
      p.skills.hitpoints = HP_START_XP;
    }
    p.name = cleanName(msg.name) || p.name || `Rover${id}`;
    if (!inBounds(bounds, p.tx, p.tz)) [p.tx, p.tz] = this.spawn;
    p.sent = `${p.tx},${p.tz},${p.anim}`;
    this.savePlayer(p);
    ws.serializeAttachment({ token, id } satisfies Attachment);

    this.send(ws, {
      t: "welcome", id, token, tick_ms: TICK_MS,
      you: { name: p.name, tx: p.tx, tz: p.tz, skills: p.skills, inv: p.inv, weapon: p.weapon, hp: p.hp, max_hp: hpLevel(p), combat: cbLevel(p) },
      players: [...this.players.values()].map((o) => ({ id: o.id, name: o.name, tx: o.tx, tz: o.tz })),
      nodes: this.nodes.map((n) => ({ id: n.id, kind: n.kind, tx: n.tx, tz: n.tz, up: !this.nodeUp.has(n.id) })),
      npcs: this.npcs.map((n) => this.npcRow(n)),
      // additive extras for the client
      bounds, spawn: this.spawn, tick: this.tickN, xp_table: XP_TABLE,
      recipes: RECIPES.map((r) => ({ id: r.id, level: r.level, xp: r.xp, inputs: r.inputs, output: r.output })),
      items: ITEMS, weapons: WEAPONS,
    });
    this.players.set(ws, p);
    this.broadcast({ t: "join", id, name: p.name, tx: p.tx, tz: p.tz }, ws);
    this.send(ws, { t: "msg", text: `Welcome to MarsScape, ${p.name}.` });
    if (!this.timer) this.startLoop();
  }

  onWalk(p: Player, msg: any) {
    const path = msg.path;
    if (!Array.isArray(path) || path.length === 0) return this.send(p.ws, { t: "err", text: "Empty path." });
    if (path.length > MAX_PATH) return this.send(p.ws, { t: "err", text: "Path too long." });
    const bounds = this.bounds!;
    const steps: [number, number][] = [];
    let [cx, cz] = [p.tx, p.tz];
    for (let i = 0; i < path.length; i++) {
      const s = path[i];
      if (!Array.isArray(s) || s.length !== 2) return this.send(p.ws, { t: "err", text: "Bad path step." });
      const tx = Math.floor(Number(s[0])), tz = Math.floor(Number(s[1]));
      if (!Number.isFinite(tx) || !Number.isFinite(tz) || !inBounds(bounds, tx, tz)) {
        return this.send(p.ws, { t: "err", text: "You can't walk there." });
      }
      const d = Math.max(Math.abs(tx - cx), Math.abs(tz - cz));
      if (d === 0) continue; // a step onto where we already are
      // The client may be up to one tile ahead of the server; bridge a 2-tile gap on the first step.
      if (d === 2 && steps.length === 0) {
        steps.push([cx + Math.sign(tx - cx), cz + Math.sign(tz - cz)]);
      } else if (d !== 1) {
        return this.send(p.ws, { t: "err", text: "Path steps must be adjacent tiles." });
      }
      steps.push([tx, tz]);
      [cx, cz] = [tx, tz];
    }
    p.path = steps;
    p.run = msg.run === true;
    p.action = null;
  }

  onAct(p: Player, msg: any) {
    const node = this.nodes.find((n) => n.id === msg.node);
    if (!node) return this.send(p.ws, { t: "err", text: "Nothing there." });
    if (node.kind === "fabricator") return this.send(p.ws, { t: "msg", text: "Use the fabricator by choosing something to craft." });
    if (node.kind === "rift") return this.send(p.ws, { t: "msg", text: "The rift hums with hostile energy. Something is coming through." });
    const def = NODE_DEFS[node.kind];
    if (levelForXp(p.skills[def.skill]) < def.level) {
      return this.send(p.ws, { t: "msg", text: `You need a ${def.skill} level of ${def.level} to work this ${node.kind}.` });
    }
    // Walk first if a path is queued; the gather starts when it ends next to the node.
    p.action = { kind: "gather", node: node.id, timer: 1 };
  }

  onCraft(p: Player, msg: any) {
    const recipe = RECIPES.find((r) => r.id === msg.recipe);
    if (!recipe) return this.send(p.ws, { t: "err", text: "Unknown recipe." });
    if (levelForXp(p.skills.fabrication) < recipe.level) {
      return this.send(p.ws, { t: "msg", text: `You need a fabrication level of ${recipe.level} to make that.` });
    }
    if (!this.hasInputs(p, recipe)) return this.send(p.ws, { t: "msg", text: "You don't have the materials for that." });
    p.action = { kind: "craft", recipe, timer: recipe.ticks };
  }

  onDrop(p: Player, msg: any) {
    const i = msg.slot;
    if (!Number.isInteger(i) || i < 0 || i >= INV_SLOTS || !p.inv[i]) return this.send(p.ws, { t: "err", text: "Nothing in that slot." });
    p.inv[i] = null;
    p.dirty = true;
    this.send(p.ws, { t: "inv", inv: p.inv });
  }

  onChat(p: Player, msg: any) {
    const now = Date.now();
    if (now - p.lastChat < 1000) return this.send(p.ws, { t: "err", text: "You're talking too fast." });
    const text = cleanChat(msg.msg);
    if (!text) return;
    p.lastChat = now;
    this.broadcast({ t: "chat", id: p.id, name: p.name, msg: text });
  }

  onAttack(p: Player, msg: any) {
    const n = this.npcs.find((n) => n.id === msg.npc);
    if (!n || n.respawnAt) return this.send(p.ws, { t: "err", text: "Nothing to attack there." });
    p.action = { kind: "attack", npc: n.id };
  }

  onEquip(p: Player, msg: any) {
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s || !WEAPONS[s.item] || s.item === "none") return this.send(p.ws, { t: "err", text: "You can't equip that." });
    const need = WEAPONS[s.item].level;
    if (levelForXp(p.skills.ranged) < need) {
      return this.send(p.ws, { t: "msg", text: `You need a ranged level of ${need} to use that.` });
    }
    const old = p.weapon;
    p.weapon = s.item;
    p.inv[i] = old ? { item: old, qty: 1 } : null;
    p.dirty = true;
    this.send(p.ws, { t: "equip", weapon: p.weapon });
    this.send(p.ws, { t: "inv", inv: p.inv });
  }

  onUnequip(p: Player) {
    if (!p.weapon) return;
    if (!this.addItem(p, p.weapon)) return this.send(p.ws, { t: "msg", text: "You have no room to put that away." });
    p.weapon = null;
    p.dirty = true;
    this.send(p.ws, { t: "equip", weapon: null });
    this.send(p.ws, { t: "inv", inv: p.inv });
  }

  onEat(p: Player, msg: any) {
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s || s.item !== "medkit") return this.send(p.ws, { t: "err", text: "You can't use that." });
    if (this.tickN < p.nextEat) return;
    p.inv[i] = null;
    p.nextEat = this.tickN + 2;
    p.nextAttack = Math.max(p.nextAttack, this.tickN + 2); // patching up costs you a shot
    const max = hpLevel(p);
    p.hp = Math.min(max, p.hp + MEDKIT_HP);
    p.dirty = true;
    this.send(p.ws, { t: "inv", inv: p.inv });
    this.send(p.ws, { t: "hp", hp: p.hp, max });
    this.send(p.ws, { t: "msg", text: "You patch yourself up with the medkit." });
  }

  // ---- combat -------------------------------------------------------------------
  /** One step from (x,z) toward (tx,tz), staying in bounds. */
  stepToward(x: number, z: number, tx: number, tz: number): [number, number] {
    return clampToBounds(this.bounds!, x + Math.sign(tx - x), z + Math.sign(tz - z));
  }

  tickPlayerAttack(p: Player, a: { npc: number }): number {
    const n = this.npcs.find((n) => n.id === a.npc);
    if (!n || n.respawnAt) { p.action = null; return ANIM.idle; }
    const wpn = WEAPONS[p.weapon ?? "none"];
    if (dist(p.tx, p.tz, n.tx, n.tz) > wpn.range) {
      // follow the target until it is in range
      [p.tx, p.tz] = this.stepToward(p.tx, p.tz, n.tx, n.tz);
      p.dirty = true;
      return ANIM.walk;
    }
    if (this.tickN < p.nextAttack) return ANIM.idle;
    p.nextAttack = this.tickN + wpn.speed;
    const lvl = levelForXp(p.skills.ranged);
    const att = (lvl + 8) * (wpn.acc + 64);
    const def = (n.def.def + 9) * 64;
    const dmg = Math.random() < hitChance(att, def) ? Math.floor(Math.random() * (maxHit(lvl, wpn.str) + 1)) : 0;
    const dealt = Math.min(dmg, n.hp);
    n.hp -= dealt;
    this.broadcast({ t: "hit", src: `p:${p.id}`, dst: `n:${n.id}`, dmg: dealt });
    if (dealt > 0) {
      this.addXp(p, "ranged", dealt * 4);
      this.addXp(p, "hitpoints", Math.round(dealt * 1.33));
      this.addXp(p, "defence", dealt); // additive: so defence trains too
    }
    if (!n.leash && !n.target) n.target = p; // it fights back
    if (n.hp <= 0) this.killNpc(n, p);
    return ANIM.attack;
  }

  killNpc(n: Npc, killer: Player) {
    n.respawnAt = this.tickN + NPC_RESPAWN;
    n.target = null;
    n.anim = ANIM.dead;
    this.broadcast({ t: "death", who: `n:${n.id}` });
    for (const o of this.players.values()) if (o.action?.kind === "attack" && o.action.npc === n.id) o.action = null;
    const drops: string[] = ["bones"];
    const cores = n.def.core >= 1 ? n.def.core : Math.random() < n.def.core ? 1 : 0;
    for (let i = 0; i < cores; i++) drops.push("demon_core");
    for (const [item, chance] of n.def.rare) if (Math.random() < chance) drops.push(item);
    const got: string[] = [];
    for (const d of drops) if (this.addItem(killer, d)) got.push(ITEMS[d].name);
    killer.dirty = true;
    this.send(killer.ws, { t: "inv", inv: killer.inv });
    const lost = got.length < drops.length ? " (some loot was lost: inventory full)" : "";
    this.send(killer.ws, { t: "msg", text: `You defeat the ${n.kind.replace("_", " ")}. Loot: ${got.join(", ") || "nothing"}${lost}.` });
  }

  killPlayer(p: Player) {
    this.broadcast({ t: "death", who: `p:${p.id}` });
    for (const n of this.npcs) if (n.target === p) { n.target = null; n.leash = true; }
    if (p.weapon) {
      this.send(p.ws, { t: "msg", text: `Your ${ITEMS[p.weapon].name.toLowerCase()} is lost in the dust.` });
      p.weapon = null;
      this.send(p.ws, { t: "equip", weapon: null });
    }
    [p.tx, p.tz] = this.spawn;
    p.path = [];
    p.action = null;
    p.hp = hpLevel(p);
    p.dirty = true;
    this.send(p.ws, { t: "hp", hp: p.hp, max: p.hp });
    this.send(p.ws, { t: "msg", text: "You were overwhelmed. You wake up back at the landing site." });
  }

  tickNpc(n: Npc) {
    if (n.respawnAt) {
      if (this.tickN >= n.respawnAt) {
        n.respawnAt = 0; n.hp = n.def.hp; [n.tx, n.tz] = n.home; n.anim = ANIM.idle; n.leash = false;
      } else n.anim = ANIM.dead;
      return;
    }
    n.anim = ANIM.idle;
    if (n.leash) {
      if (n.tx === n.home[0] && n.tz === n.home[1]) { n.leash = false; n.hp = n.def.hp; return; }
      [n.tx, n.tz] = this.stepToward(n.tx, n.tz, n.home[0], n.home[1]);
      n.anim = ANIM.walk;
      return;
    }
    // aggression: nearest eligible player within range
    if (!n.target) {
      let best: Player | null = null, bd = AGGRO_RANGE + 1;
      for (const p of this.players.values()) {
        const d = dist(p.tx, p.tz, n.tx, n.tz);
        // single-way combat: only one demon picks on a given player at a time
        if (d < bd && cbLevel(p) < 2 * n.def.level && !this.npcs.some((o) => o.target === p)) { best = p; bd = d; }
      }
      n.target = best;
    }
    const t = n.target;
    if (t) {
      if (!this.players.has(t.ws) || dist(n.tx, n.tz, n.home[0], n.home[1]) > LEASH_RANGE) {
        n.target = null; n.leash = true; return;
      }
      if (dist(n.tx, n.tz, t.tx, t.tz) > n.def.range) {
        [n.tx, n.tz] = this.stepToward(n.tx, n.tz, t.tx, t.tz);
        n.anim = ANIM.walk;
        return;
      }
      if (this.tickN < n.nextAttack) return;
      n.nextAttack = this.tickN + n.def.speed;
      const att = (n.def.att + 9) * 64;
      const def = (levelForXp(t.skills.defence) + 9) * 64;
      const dmg = Math.random() < hitChance(att, def) ? Math.floor(Math.random() * (n.def.maxHit + 1)) : 0;
      const dealt = Math.min(dmg, t.hp);
      t.hp -= dealt;
      t.dirty = true;
      n.anim = ANIM.attack;
      this.broadcast({ t: "hit", src: `n:${n.id}`, dst: `p:${t.id}`, dmg: dealt });
      this.send(t.ws, { t: "hp", hp: t.hp, max: hpLevel(t) });
      if (t.hp <= 0) this.killPlayer(t);
      return;
    }
    // wander near home
    if (Math.random() < 0.12) {
      const nx = n.tx + Math.floor(Math.random() * 3) - 1, nz = n.tz + Math.floor(Math.random() * 3) - 1;
      if (dist(nx, nz, n.home[0], n.home[1]) <= WANDER_RANGE && inBounds(this.bounds!, nx, nz)) {
        [n.tx, n.tz] = [nx, nz];
        n.anim = ANIM.walk;
      }
    }
  }

  // ---- game helpers -----------------------------------------------------------
  hasInputs(p: Player, r: Recipe): boolean {
    return Object.entries(r.inputs).every(([item, n]) => this.countItem(p, item) >= n);
  }

  countItem(p: Player, item: string): number {
    return p.inv.reduce((a, s) => a + (s && s.item === item ? s.qty : 0), 0);
  }

  removeItem(p: Player, item: string, n: number) {
    for (let i = 0; i < INV_SLOTS && n > 0; i++) {
      const s = p.inv[i];
      if (!s || s.item !== item) continue;
      const take = Math.min(s.qty, n);
      s.qty -= take;
      n -= take;
      if (s.qty <= 0) p.inv[i] = null;
    }
  }

  addItem(p: Player, item: string): boolean {
    if (ITEMS[item]?.stack) {
      const s = p.inv.find((s) => s && s.item === item);
      if (s) { s.qty++; return true; }
    }
    const i = p.inv.indexOf(null);
    if (i < 0) return false;
    p.inv[i] = { item, qty: 1 };
    return true;
  }

  addXp(p: Player, skill: Skill, gain: number) {
    const before = levelForXp(p.skills[skill]);
    p.skills[skill] = Math.min(MAX_XP, p.skills[skill] + gain);
    this.send(p.ws, { t: "xp", skill, xp: p.skills[skill], gain });
    const after = levelForXp(p.skills[skill]);
    if (after > before) {
      this.send(p.ws, { t: "level", skill, level: after });
      this.send(p.ws, { t: "msg", text: `Your ${skill} level is now ${after}!` });
      if (after % 10 === 0 || after === 99) {
        this.broadcast({ t: "msg", text: `${p.name} reached ${skill} level ${after}.` }, p.ws);
      }
    }
  }

  // ---- tick loop --------------------------------------------------------------
  startLoop() {
    if (this.timer) return;
    this.timer = setInterval(() => {
      try { this.tick(); } catch (e) { console.error("tick failed", e); }
    }, TICK_MS);
  }

  stopLoop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  tick() {
    const now = Date.now();
    this.tickN++;

    // sockets that never said hello
    for (const [ws, c] of this.conns) {
      if (!this.players.has(ws) && now - c.opened > HELLO_MS) this.kick(ws, "No hello received.", 1002);
    }

    for (const [id, at] of this.nodeUp) {
      if (this.tickN >= at) {
        this.nodeUp.delete(id);
        this.broadcast({ t: "node", id, up: true });
      }
    }

    for (const p of [...this.players.values()]) {
      if (now - p.lastInput > IDLE_MS) { this.kick(p.ws, "You were logged out for being idle.", 1000); continue; }
      let anim: number = ANIM.idle;
      if (p.path.length) {
        const n = p.run ? 2 : 1;
        for (let i = 0; i < n && p.path.length; i++) [p.tx, p.tz] = p.path.shift()!;
        anim = p.run ? ANIM.run : ANIM.walk;
        p.dirty = true;
      } else if (p.action?.kind === "gather") {
        anim = this.tickGather(p, p.action);
      } else if (p.action?.kind === "craft") {
        anim = this.tickCraft(p, p.action);
      } else if (p.action?.kind === "attack") {
        anim = this.tickPlayerAttack(p, p.action);
      }
      p.anim = anim;
      if (this.tickN % REGEN_TICKS === 0 && p.hp < hpLevel(p)) {
        p.hp++;
        this.send(p.ws, { t: "hp", hp: p.hp, max: hpLevel(p) });
      }
    }
    if (this.players.size) for (const n of this.npcs) this.tickNpc(n);

    const full = this.tickN % FULL_SYNC_TICKS === 0;
    const rows: [number, number, number, number][] = [];
    for (const p of this.players.values()) {
      const key = `${p.tx},${p.tz},${p.anim}`;
      if (full || key !== p.sent) rows.push([p.id, p.tx, p.tz, p.anim]);
      p.sent = key;
    }
    const mrows: [number, number, number, number, number][] = [];
    for (const n of this.npcs) {
      const key = `${n.tx},${n.tz},${n.anim},${n.hp}`;
      if (full || key !== n.sent) mrows.push([n.id, n.tx, n.tz, n.anim, n.hp]);
      n.sent = key;
    }
    const msg: any = { t: "tick", n: this.tickN, p: rows, m: mrows };
    if (full) msg.full = true;
    this.broadcast(msg);

    if (this.tickN % SAVE_TICKS === 0) {
      for (const p of this.players.values()) if (p.dirty) this.savePlayer(p);
      this.setMeta("tick", String(this.tickN));
    }
    if (!this.players.size && !this.conns.size) {
      this.setMeta("tick", String(this.tickN));
      this.stopLoop(); // let the room hibernate
    }
  }

  tickGather(p: Player, a: { node: number; timer: number }): number {
    const node = this.nodes.find((n) => n.id === a.node)!;
    const def = NODE_DEFS[node.kind as GatherKind];
    const stop = (text?: string) => { p.action = null; if (text) this.send(p.ws, { t: "msg", text }); return ANIM.idle; };
    if (Math.max(Math.abs(p.tx - node.tx), Math.abs(p.tz - node.tz)) > 1) return stop("You need to stand next to that.");
    if (this.nodeUp.has(node.id)) return stop(`This ${node.kind} has been worked out for now.`);
    if (p.inv.indexOf(null) < 0 && !ITEMS[def.item].stack) return stop("Your inventory is too full to hold any more.");
    const anim = def.skill === "survey" ? ANIM.survey : ANIM.mine;
    if (--a.timer > 0) return anim;
    a.timer = GATHER_TICKS;
    const drill = def.skill === "mining" && this.countItem(p, "drill_bit") > 0;
    if (Math.random() >= gatherChance(levelForXp(p.skills[def.skill]), def.level, drill)) return anim;
    this.addItem(p, def.item);
    p.dirty = true;
    this.send(p.ws, { t: "msg", text: def.verb });
    this.addXp(p, def.skill, def.xp);
    this.send(p.ws, { t: "inv", inv: p.inv });
    if (Math.random() < def.deplete) {
      this.nodeUp.set(node.id, this.tickN + def.respawn);
      this.broadcast({ t: "node", id: node.id, up: false });
      for (const o of this.players.values()) {
        if (o.action?.kind === "gather" && o.action.node === node.id) { o.action = null; if (o !== p) o.anim = ANIM.idle; }
      }
      return ANIM.idle;
    }
    return anim;
  }

  tickCraft(p: Player, a: { recipe: Recipe; timer: number }): number {
    const fab = this.nodes[0];
    if (Math.max(Math.abs(p.tx - fab.tx), Math.abs(p.tz - fab.tz)) > 1) {
      p.action = null;
      this.send(p.ws, { t: "msg", text: "You need to stand next to the fabricator." });
      return ANIM.idle;
    }
    if (--a.timer > 0) return ANIM.craft;
    const r = a.recipe;
    if (!this.hasInputs(p, r)) { p.action = null; return ANIM.idle; }
    for (const [item, n] of Object.entries(r.inputs)) this.removeItem(p, item, n);
    this.addItem(p, r.output); // inputs freed at least one slot
    p.dirty = true;
    this.send(p.ws, { t: "inv", inv: p.inv });
    this.send(p.ws, { t: "msg", text: r.text });
    this.addXp(p, "fabrication", r.xp);
    // keep making more while materials last ("make all")
    if (this.hasInputs(p, r)) { a.timer = r.ticks; return ANIM.craft; }
    p.action = null;
    return ANIM.idle;
  }
}
