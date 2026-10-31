// MarsScape server: a Cloudflare Worker that routes /room/<site_id> to one Durable Object per
// Mars site, plus /room/<site_id>~cave for the lava tube under it. The room runs a 600 ms tick loop,
// validates all movement/actions, and keeps player progress in the surface room's SQLite storage
// (the cave borrows the character from the surface room over RPC). Protocol: see PROTOCOL.md.

import { DurableObject } from "cloudflare:workers";
import {
  TICK_MS, INV_SLOTS, MAX_XP, SKILLS, Skill, Slot, ITEMS, NODE_DEFS, RECIPES, Recipe, GATHER_TICKS, XP_TABLE,
  Bounds, WorldNode, levelForXp, gatherChance, generateNodes, clampBounds, clampToBounds, inBounds,
  cleanName, cleanChat, GatherKind, HP_START_XP, WEAPONS, LEGACY_ITEMS, mergeStacks, combatLevel, hitChance, maxHit, NPC_DEFS, NpcDef,
  NPC_RESPAWN, AGGRO_RANGE, LEASH_RANGE, WANDER_RANGE, WorldNpc, NpcKind, generateRifts, placeExtras,
  ARMOUR, GEAR_SLOTS, Gear, GearSlot, emptyGear, gearBonus, playerDefRoll, creditDrop,
} from "./game";
import { BOT_WS, Colony } from "./bots";
import { CaveMap, generateCave, isCaveSite, surfaceOf, floorAt, stepOpen } from "./cave";
import { Bank, Trades, Trade, addTo } from "./bank";
import { Shop, Buyback, SHOP_RANGE } from "./shop";
import { QuestState, cleanQuests, questDefsPublic, questEvent, questPoints, talk, choose, rub } from "./quests";
import { Surge } from "./events";

export interface Env {
  ROOMS: DurableObjectNamespace<Room>;
  /** "1" enables the `test` message (local `wrangler dev --var MARSSCAPE_TEST:1` only). */
  MARSSCAPE_TEST?: string;
}

const PROTOCOL_VER = 1;
const MAX_PLAYERS = 100;
const MAX_FRAME = 4096;          // bytes per client frame
const MAX_PATH = 256;            // tiles per walk request
const MSGS_PER_SEC = 20;         // hard flood limit, closes the socket
const IDLE_MS = 20 * 60 * 1000;  // no input (pings don't count) -> logout
const HELLO_MS = 15_000;         // must say hello within this
const FULL_SYNC_TICKS = 10;      // full position list every N ticks
const SAVE_TICKS = 50;           // persist dirty players every N ticks (and on leave/travel)
const TALK_RANGE = 2;            // tiles from a colonist to talk

const REGEN_TICKS = 10;          // +1 hp every N ticks
const MEDKIT_HP = 20;
const TEMP_ID0 = 100_000;        // ids for Rift Surge creatures

const ANIM = { idle: 0, walk: 1, run: 2, mine: 3, survey: 4, craft: 5, attack: 6, dead: 7 } as const;

type Action =
  | { kind: "gather"; node: number; timer: number }
  | { kind: "craft"; recipe: Recipe; timer: number }
  | { kind: "attack"; npc: number }
  | { kind: "use"; node: number }
  | { kind: "talk"; id: number }
  | { kind: "shop"; id: number };

export interface Player {
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
  gear: Gear;
  quests: QuestState;
  trade: Trade | null;
  dialog: { npc: number; quest: string | null; opts: string[] } | null;
  ops: Record<string, number>; // per-message-type rate limit: type -> last time (ms)
  buyback?: Buyback[];          // this session's recent sales to the trader (shop.ts)
}

export interface Npc {
  id: number;
  kind: NpcKind;
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
  temp?: boolean;      // a Rift Surge creature: aggressive to all, removed instead of respawning
}

/** The part of a player that is saved and carried between the surface and the cave. */
export interface CharData {
  name: string;
  skills: Record<Skill, number>;
  inv: Slot[];
  hp: number;
  weapon: string | null;
  gear: Gear;
  quests: QuestState;
}

const dist = (ax: number, az: number, bx: number, bz: number) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));
const hpLevel = (p: Player) => levelForXp(p.skills.hitpoints);
const cbLevel = (p: Player) => combatLevel(levelForXp(p.skills.ranged), levelForXp(p.skills.defence), hpLevel(p));
const TOKEN_RE = /^[a-f0-9]{32}$/;

interface Conn {
  // per-socket bookkeeping that is not game state
  opened: number;
  window: number;
  count: number;
  badJson: number;
  hello?: boolean;
}

interface Attachment {
  token?: string;
  id?: number;
}

function sanitizeSite(s: string): string | null {
  try { s = decodeURIComponent(s); } catch { return null; }
  if (!/^[A-Za-z0-9_-]{1,64}(~cave)?$/.test(s)) return null;
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
  tempId = TEMP_ID0;
  tickN = 0;
  timer: ReturnType<typeof setInterval> | null = null;
  colony = new Colony(this); // server-side NPC colonists (bots.ts); never saved, never keep the room awake
  cave: CaveMap | null = null; // set in a "<site>~cave" room
  bank = new Bank(this);
  trades = new Trades(this, this.bank);
  surge = new Surge(this);
  shop = new Shop(this);
  rowsWritten = 0;           // SQLite rows written since this instance started (see /info)
  private surfaceStub: DurableObjectStub<Room> | null = null;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    // count rows written (free-plan budget); write statements are run eagerly
    const exec = this.sql.exec.bind(this.sql);
    (this.sql as any).exec = (q: string, ...args: any[]) => {
      const c = exec(q, ...args);
      if (/^\s*(INSERT|UPDATE|DELETE)/i.test(q)) { c.toArray(); this.rowsWritten += c.rowsWritten; }
      return c;
    };
    this.sql.exec(`CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)`);
    this.sql.exec(`CREATE TABLE IF NOT EXISTS players (
      token TEXT PRIMARY KEY, name TEXT NOT NULL, skills TEXT NOT NULL, inv TEXT NOT NULL,
      tx INTEGER NOT NULL, tz INTEGER NOT NULL, updated INTEGER NOT NULL)`);
    // added later; ignore "duplicate column" on existing rooms
    for (const col of ["hp INTEGER", "weapon TEXT", "gear TEXT", "quests TEXT", "loc TEXT"]) {
      try { this.sql.exec(`ALTER TABLE players ADD COLUMN ${col}`); } catch { /* exists */ }
    }
    const meta = new Map<string, string>();
    for (const r of this.sql.exec<{ k: string; v: string }>(`SELECT k, v FROM meta`)) meta.set(r.k, r.v);
    this.site = meta.get("site") ?? "";
    this.nextId = Number(meta.get("next_id") ?? 1);
    this.tickN = Number(meta.get("tick") ?? 0);
    if (isCaveSite(this.site)) this.initCave();
    else if (meta.has("bounds")) {
      this.bounds = JSON.parse(meta.get("bounds")!);
      this.spawn = JSON.parse(meta.get("spawn")!);
      this.initWorld();
    }
    // Woken from hibernation: rebuild players from the sockets that are still open.
    for (const ws of ctx.getWebSockets()) {
      const a = (ws.deserializeAttachment() ?? {}) as Attachment;
      if (a.id) this.nextId = Math.max(this.nextId, a.id + 1);
      if (this.cave) { try { ws.close(1012, "Please reconnect."); } catch { /* closed */ } continue; }
      this.conns.set(ws, { opened: Date.now(), window: 0, count: 0, badJson: 0, hello: true });
      if (a.token && a.id) {
        const p = this.loadPlayer(ws, a.token, a.id, "");
        if (p) this.players.set(ws, p);
      }
    }
    if (this.players.size) this.startLoop();
  }

  initWorld() {
    this.nodes = generateNodes(this.site, this.bounds!, this.spawn);
    const npcs = generateRifts(this.site, this.bounds!, this.spawn, this.nodes);
    placeExtras(this.site, this.bounds!, this.spawn, this.nodes);
    this.npcs = npcs.map((n) => this.makeNpc(n));
  }

  initCave() {
    this.cave = generateCave(surfaceOf(this.site));
    this.bounds = [0, 0, this.cave.w - 1, this.cave.h - 1];
    this.spawn = this.cave.spawn;
    this.nodes = this.cave.nodes;
    this.npcs = this.cave.npcs.map((n) => this.makeNpc(n));
  }

  makeNpc(n: WorldNpc, temp = false): Npc {
    return {
      id: n.id, kind: n.kind, def: NPC_DEFS[n.kind], home: n.home, tx: n.home[0], tz: n.home[1],
      hp: NPC_DEFS[n.kind].hp, target: null, nextAttack: 0, respawnAt: 0, leash: false, anim: ANIM.idle, sent: "",
      ...(temp ? { temp: true } : {}),
    };
  }

  npcRow(n: Npc) {
    return { id: n.id, kind: n.kind, tx: n.tx, tz: n.tz, hp: n.hp, max: n.def.hp, level: n.def.level, dead: n.respawnAt > 0 };
  }

  /** A Rift Surge creature, announced with npc_spawn. */
  spawnTempNpc(kind: NpcKind, home: [number, number]): Npc {
    const n = this.makeNpc({ id: this.tempId++, kind, home }, true);
    this.npcs.push(n);
    this.broadcast({ t: "npc_spawn", npc: this.npcRow(n) });
    return n;
  }

  removeNpc(n: Npc) {
    const i = this.npcs.indexOf(n);
    if (i < 0) return;
    this.npcs.splice(i, 1);
    for (const p of this.everyone()) if (p.action?.kind === "attack" && p.action.npc === n.id) p.action = null;
    this.broadcast({ t: "npc_despawn", id: n.id });
  }

  setMeta(k: string, v: string) {
    this.sql.exec(`INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v`, k, v);
  }

  async fetch(req: Request): Promise<Response> {
    const site = req.headers.get("X-Site") ?? "";
    if (!this.site && site) {
      this.site = site;
      this.setMeta("site", site);
      if (isCaveSite(site)) this.initCave();
    }
    if (new URL(req.url).pathname === "/info") {
      return Response.json({
        site: this.site, room: this.cave ? "cave" : "surface", players: this.players.size, max: MAX_PLAYERS, bounds: this.bounds,
        bots: this.colony.list.length, ticking: !!this.timer, tick: this.tickN, surge: this.surge.phase, rows_written: this.rowsWritten,
      }, {
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
    if (ws === BOT_WS) return;
    try { ws.send(JSON.stringify(msg)); } catch { /* closed */ }
  }

  /** Humans and colonists: everyone who walks, fights and shows up in ticks. */
  everyone(): Player[] {
    return [...this.players.values(), ...this.colony.list];
  }

  present(p: Player): boolean {
    return this.players.has(p.ws) || this.colony.has(p);
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

  /** Take a player out of the room (no saving). */
  removePlayer(p: Player) {
    if (!this.players.delete(p.ws)) return;
    for (const n of this.npcs) if (n.target === p) n.target = null;
    this.trades.onLeave(p);
    this.shop.onLeave(p);
    this.broadcast({ t: "leave", id: p.id });
    this.colony.onLeave(p);
  }

  dropSocket(ws: WebSocket) {
    const p = this.players.get(ws);
    this.conns.delete(ws);
    if (!p) return;
    this.removePlayer(p);
    if (this.cave) this.surface().caveSave(p.token, this.charOf(p), true, "pit").catch(() => {});
    else this.savePlayer(p);
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
      if (msg.t === "hello") {
        if (conn.hello) return;
        conn.hello = true;
        return this.cave ? this.onCaveHello(ws, msg) : this.onHello(ws, msg);
      }
      if (msg.t === "ping") return this.send(ws, { t: "pong" });
      return this.send(ws, { t: "err", text: "Say hello first." });
    }
    if (msg.t !== "ping") p.lastInput = now;
    const surface = !this.cave;
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
      case "unequip": return this.onUnequip(p, msg);
      case "eat": return this.onEat(p, msg);
      case "rub": if (this.tooFast(p, "rub", 300) || this.busy(p)) return; return rub(this, p, msg);
      case "talk": if (this.tooFast(p, "talk", 400)) return; return this.onTalk(p, msg);
      case "dialog_choose": return choose(this, p, String(msg.option ?? ""));
      case "bank_deposit": if (!surface || this.tooFast(p, "bank", 100)) return; return this.bank.deposit(p, msg);
      case "bank_withdraw": if (!surface || this.tooFast(p, "bank", 100)) return; return this.bank.withdraw(p, msg);
      case "trade_request": if (!surface) return this.send(ws, { t: "msg", text: "There's no room to trade down here." }); if (this.tooFast(p, "trade", 500)) return; return this.trades.request(p, msg);
      case "trade_offer": return this.trades.offer(p, msg);
      case "trade_remove": return this.trades.remove(p, msg);
      case "trade_accept": return this.trades.accept(p);
      case "trade_decline": return this.trades.decline(p);
      case "shop_open": if (!surface) return this.send(ws, { t: "msg", text: "There's no trader down here." }); if (this.tooFast(p, "shop", 300)) return; return this.onShopOpen(p, msg);
      case "shop_close": return this.shop.close(p);
      case "shop_buy": if (!surface || this.tooFast(p, "shop", 100)) return; return this.shop.buy(p, msg);
      case "shop_sell": if (!surface || this.tooFast(p, "shop", 100)) return; return this.shop.sell(p, msg);
      case "shop_buyback": if (!surface || this.tooFast(p, "shop", 100)) return; return this.shop.buyback(p, msg);
      case "value": if (this.tooFast(p, "value", 100)) return; return this.shop.value(p, msg);
      case "test": if (this.env.MARSSCAPE_TEST === "1") return this.onTest(p, msg); // else: unknown
      default: return this.send(ws, { t: "err", text: `Unknown message type "${String(msg.t).slice(0, 20)}".` });
    }
  }

  /** Per-type rate limit; extra messages inside the window are refused with "Slow down." */
  tooFast(p: Player, key: string, ms: number): boolean {
    const now = Date.now();
    if (now - (p.ops[key] ?? 0) < ms) { this.send(p.ws, { t: "err", text: "Slow down." }); return true; }
    p.ops[key] = now;
    return false;
  }

  /** While trading, nothing may take items out of your inventory. */
  busy(p: Player): boolean {
    if (!p.trade) return false;
    this.send(p.ws, { t: "msg", text: "Finish or decline your trade first." });
    return true;
  }

  // ---- persistence ----------------------------------------------------------
  /** Read a saved character (surface room). */
  readChar(token: string): (CharData & { tx: number; tz: number; loc: string | null }) | null {
    const rows = this.sql.exec<{ name: string; skills: string; inv: string; tx: number; tz: number; hp: number | null; weapon: string | null; gear: string | null; quests: string | null; loc: string | null }>(
      `SELECT name, skills, inv, tx, tz, hp, weapon, gear, quests, loc FROM players WHERE token = ?`, token).toArray();
    if (!rows.length) return null;
    const r = rows[0];
    const parse = (s: string | null) => { try { return s ? JSON.parse(s) : null; } catch { return null; } };
    return { ...this.cleanChar({ name: r.name, skills: parse(r.skills), inv: parse(r.inv), hp: r.hp ?? 0, weapon: r.weapon, gear: parse(r.gear), quests: parse(r.quests) }), tx: r.tx, tz: r.tz, loc: r.loc };
  }

  /** Validate a character from storage or RPC: unknown items dropped, old item ids mapped. */
  cleanChar(d: any): CharData {
    const skills = Object.fromEntries(SKILLS.map((s) => [s, 0])) as Record<Skill, number>;
    if (d?.skills && typeof d.skills === "object") {
      for (const s of SKILLS) skills[s] = Math.min(MAX_XP, Math.max(0, Math.floor(Number(d.skills[s]) || 0)));
    }
    skills.hitpoints = Math.max(HP_START_XP, skills.hitpoints);
    const raw: any[] = Array.isArray(d?.inv) ? d.inv : [];
    const inv: Slot[] = Array.from({ length: INV_SLOTS }, (_, i) => {
      const s = raw[i];
      const item = s && typeof s.item === "string" ? LEGACY_ITEMS[s.item] ?? s.item : "";
      return s && ITEMS[item] && s.qty > 0 ? { item, qty: Math.floor(s.qty) } : null;
    });
    mergeStacks(inv); // saves from before stacking kept one item per slot
    const gear = emptyGear();
    if (d?.gear && typeof d.gear === "object") {
      for (const slot of GEAR_SLOTS) { const it = d.gear[slot]; if (typeof it === "string" && ARMOUR[it]?.slot === slot) gear[slot] = it; }
    }
    const weapon = typeof d?.weapon === "string" && WEAPONS[d.weapon] && d.weapon !== "none" ? d.weapon : null;
    return { name: typeof d?.name === "string" ? d.name : "", skills, inv, hp: Math.floor(Number(d?.hp) || 0), weapon, gear, quests: cleanQuests(d?.quests) };
  }

  charOf(p: Player): CharData {
    return { name: p.name, skills: p.skills, inv: p.inv, hp: p.hp, weapon: p.weapon, gear: p.gear, quests: p.quests };
  }

  newPlayer(ws: WebSocket, id: number, token: string, c: CharData, tx: number, tz: number): Player {
    return this.fixHp({
      ws, id, token, name: c.name, tx, tz, skills: c.skills, inv: c.inv, path: [], run: false, action: null,
      anim: ANIM.idle, sent: "", dirty: false, lastChat: 0, lastInput: Date.now(),
      hp: c.hp, weapon: c.weapon, nextAttack: 0, nextEat: 0, gear: c.gear, quests: c.quests,
      trade: null, dialog: null, ops: {},
    });
  }

  loadPlayer(ws: WebSocket, token: string, id: number, name: string): Player | null {
    const c = this.readChar(token);
    if (!c) return null;
    if (name) c.name = name;
    return this.newPlayer(ws, id, token, c, c.tx, c.tz);
  }

  fixHp(p: Player): Player {
    const max = hpLevel(p);
    if (!(p.hp > 0) || p.hp > max) p.hp = max;
    return p;
  }

  writeChar(token: string, c: CharData) {
    this.sql.exec(
      `UPDATE players SET name = ?, skills = ?, inv = ?, hp = ?, weapon = ?, gear = ?, quests = ?, updated = ? WHERE token = ?`,
      c.name, JSON.stringify(c.skills), JSON.stringify(c.inv), c.hp, c.weapon, JSON.stringify(c.gear), JSON.stringify(c.quests), Date.now(), token);
  }

  savePlayer(p: Player) {
    p.dirty = false;
    if (!p.token) return; // colonists are never saved
    if (this.cave) {
      this.surface().caveSave(p.token, this.charOf(p), false, null).catch(() => {});
      return;
    }
    this.sql.exec(
      `INSERT INTO players (token, name, skills, inv, tx, tz, updated, hp, weapon, gear, quests) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(token) DO UPDATE SET name = excluded.name, skills = excluded.skills, inv = excluded.inv,
       tx = excluded.tx, tz = excluded.tz, updated = excluded.updated, hp = excluded.hp, weapon = excluded.weapon,
       gear = excluded.gear, quests = excluded.quests`,
      p.token, p.name, JSON.stringify(p.skills), JSON.stringify(p.inv), p.tx, p.tz, Date.now(), p.hp, p.weapon,
      JSON.stringify(p.gear), JSON.stringify(p.quests));
  }

  // ---- surface <-> cave (RPC between the two Durable Objects) ----------------
  /** The surface room for this cave (one stub, so calls arrive in order). */
  surface(): DurableObjectStub<Room> {
    this.surfaceStub ??= this.env.ROOMS.get(this.env.ROOMS.idFromName(surfaceOf(this.site)));
    return this.surfaceStub;
  }

  caveRoom(): DurableObjectStub<Room> {
    return this.env.ROOMS.get(this.env.ROOMS.idFromName(this.site + "~cave"));
  }

  /** Where you come back up: next to the pit, or the landing site. */
  surfaceTile(where: "pit" | "spawn" | null): [number, number] {
    const pit = this.nodes.find((n) => n.kind === "pit");
    return where === "pit" && pit && this.bounds ? clampToBounds(this.bounds, pit.tx + 1, pit.tz) : this.spawn;
  }

  /** RPC, surface side: a cave borrows this character. Kicks any surface session first. */
  async caveEnter(token: string): Promise<CharData | null> {
    if (!TOKEN_RE.test(token)) return null;
    for (const [ws, p] of [...this.players]) if (p.token === token) this.kick(ws, "You climb down into the dark.", 1000);
    const c = this.readChar(token);
    if (!c) return null;
    this.sql.exec(`UPDATE players SET loc = 'cave' WHERE token = ?`, token);
    const { tx: _x, tz: _z, loc: _l, ...data } = c;
    return data;
  }

  /** RPC, surface side: the cave saves a borrowed character, and with `release` hands it back. */
  async caveSave(token: string, data: CharData, release: boolean, where: "pit" | "spawn" | null): Promise<boolean> {
    const r = this.sql.exec<{ loc: string | null }>(`SELECT loc FROM players WHERE token = ?`, token).toArray();
    if (!r.length || r[0].loc !== "cave") return false; // stale: the character was taken back already
    this.writeChar(token, this.cleanChar(data));
    if (release) {
      const [tx, tz] = this.surfaceTile(where);
      this.sql.exec(`UPDATE players SET loc = NULL, tx = ?, tz = ? WHERE token = ?`, tx, tz, token);
    }
    return true;
  }

  /** RPC, cave side: the player logged in on the surface; give the character back and drop them. */
  async evict(token: string): Promise<CharData | null> {
    const p = [...this.players.values()].find((x) => x.token === token);
    if (!p) return null;
    this.removePlayer(p);
    this.send(p.ws, { t: "err", text: "Logged in from another location." });
    try { p.ws.close(1000, "Logged in from another location."); } catch { /* closed */ }
    this.conns.delete(p.ws);
    return this.charOf(p);
  }

  /** Surface: climb down the pit. Save, then tell the client to reconnect to the cave room. */
  usePit(p: Player) {
    if (this.busy(p)) return;
    this.savePlayer(p);
    this.removePlayer(p);
    this.send(p.ws, { t: "travel", room: this.site + "~cave" });
    try { p.ws.close(1000, "travel"); } catch { /* closed */ }
    this.conns.delete(p.ws);
  }

  /** Cave: hand the character back to the surface, then send the client up. */
  async leaveCave(p: Player, where: "pit" | "spawn") {
    this.removePlayer(p);
    try { await this.surface().caveSave(p.token, this.charOf(p), true, where); } catch { /* the surface re-syncs on login */ }
    this.send(p.ws, { t: "travel", room: surfaceOf(this.site) });
    try { p.ws.close(1000, "travel"); } catch { /* closed */ }
    this.conns.delete(p.ws);
  }

  // ---- handlers ---------------------------------------------------------------
  welcome(ws: WebSocket, p: Player) {
    this.send(ws, {
      t: "welcome", id: p.id, token: p.token, tick_ms: TICK_MS,
      you: {
        name: p.name, tx: p.tx, tz: p.tz, skills: p.skills, inv: p.inv, weapon: p.weapon, hp: p.hp, max_hp: hpLevel(p), combat: cbLevel(p),
        gear: p.gear, quests: p.quests, quest_points: questPoints(p.quests),
      },
      players: this.everyone().filter((o) => o !== p).map((o) => ({ id: o.id, name: o.name, tx: o.tx, tz: o.tz, ...this.colony.tag(o) })),
      nodes: this.nodes.map((n) => ({ id: n.id, kind: n.kind, tx: n.tx, tz: n.tz, up: !this.nodeUp.has(n.id) })),
      npcs: this.npcs.map((n) => this.npcRow(n)),
      // additive extras for the client
      bounds: this.bounds, spawn: this.spawn, tick: this.tickN, xp_table: XP_TABLE,
      recipes: RECIPES.map((r) => ({ id: r.id, level: r.level, xp: r.xp, inputs: r.inputs, output: r.output, ...(r.quest ? { quest: r.quest } : {}) })),
      items: ITEMS, weapons: WEAPONS, armour: ARMOUR, quest_defs: questDefsPublic(),
      creatures: Object.fromEntries(Object.entries(NPC_DEFS).map(([k, d]) => [k, { name: d.name, level: d.level, hp: d.hp, max_hit: d.maxHit, range: d.range }])),
      room: this.cave ? "cave" : "surface",
      event: this.surge.eventMsg(),
      ...(this.cave ? { cave: { w: this.cave.w, h: this.cave.h, rle: this.cave.rle }, exit: this.cave.entrance } : {}),
    });
  }

  async onHello(ws: WebSocket, msg: any) {
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

    let token = typeof msg.token === "string" && TOKEN_RE.test(msg.token) ? msg.token : "";
    // Still down in the cave (another tab, or a dropped connection there)? Take the character back.
    if (token) {
      const r = this.sql.exec<{ loc: string | null }>(`SELECT loc FROM players WHERE token = ?`, token).toArray();
      if (r.length && r[0].loc === "cave") {
        let data: CharData | null = null;
        try { data = await this.caveRoom().evict(token); } catch { /* cave unreachable: use the last save */ }
        if (data) this.writeChar(token, this.cleanChar(data));
        const [tx, tz] = this.surfaceTile("pit");
        this.sql.exec(`UPDATE players SET loc = NULL, tx = ?, tz = ? WHERE token = ?`, tx, tz, token);
        if (!this.conns.has(ws)) return;
      }
    }
    const id = this.nextId++;
    // Same account already online here: the new login wins (kick first so its state is saved).
    for (const [ows, op] of [...this.players]) {
      if (token && op.token === token) this.kick(ows, "Logged in from another location.", 1000);
    }
    let p = token ? this.loadPlayer(ws, token, id, "") : null;
    if (!p) {
      token = crypto.randomUUID().replace(/-/g, "");
      p = this.newPlayer(ws, id, token, this.cleanChar({ hp: 10 }), this.spawn[0], this.spawn[1]);
      p.dirty = true;
    }
    const name = cleanName(msg.name) || p.name || `Rover${id}`;
    if (name !== p.name) { p.name = name; p.dirty = true; }
    if (!inBounds(bounds, p.tx, p.tz)) [p.tx, p.tz] = this.spawn;
    p.sent = `${p.tx},${p.tz},${p.anim}`;
    if (p.dirty) this.savePlayer(p);
    ws.serializeAttachment({ token, id } satisfies Attachment);
    this.colony.rebalance(this.players.size + 1); // so the welcome already lists this room's colonists

    this.welcome(ws, p);
    this.players.set(ws, p);
    this.broadcast({ t: "join", id, name: p.name, tx: p.tx, tz: p.tz }, ws);
    this.send(ws, { t: "msg", text: `Welcome to MarsScape, ${p.name}.` });
    this.colony.onJoin(p);
    if (!this.timer) this.startLoop();
  }

  /** Cave rooms: the character is borrowed from the surface room; you can only arrive with a token. */
  async onCaveHello(ws: WebSocket, msg: any) {
    if (msg.ver !== PROTOCOL_VER) return this.kick(ws, `Protocol version ${PROTOCOL_VER} required.`, 1002);
    if (this.players.size >= MAX_PLAYERS) return this.kick(ws, "The tube is too crowded. Try again later.", 1013);
    const token = typeof msg.token === "string" && TOKEN_RE.test(msg.token) ? msg.token : "";
    if (!token) return this.kick(ws, "Climb down through the pit on the surface first.", 1008);
    let data: CharData | null = null;
    const old = [...this.players.values()].find((x) => x.token === token);
    if (old) {
      // already down here on another socket: take that session over, keeping its newest state
      data = this.charOf(old);
      this.removePlayer(old);
      this.send(old.ws, { t: "err", text: "Logged in from another location." });
      try { old.ws.close(1000, "Logged in from another location."); } catch { /* closed */ }
      this.conns.delete(old.ws);
    } else {
      try { data = await this.surface().caveEnter(token); } catch { return this.kick(ws, "The tunnel is blocked. Try again in a moment.", 1011); }
      if (data) data = this.cleanChar(data);
    }
    if (!data) return this.kick(ws, "Climb down through the pit on the surface first.", 1008);
    if (!this.conns.has(ws)) {
      this.surface().caveSave(token, data, true, "pit").catch(() => {});
      return;
    }
    const id = this.nextId++;
    const p = this.newPlayer(ws, id, token, data, this.spawn[0], this.spawn[1]);
    p.sent = `${p.tx},${p.tz},${p.anim}`;
    ws.serializeAttachment({ token, id } satisfies Attachment);
    this.welcome(ws, p);
    this.players.set(ws, p);
    this.broadcast({ t: "join", id, name: p.name, tx: p.tx, tz: p.tz }, ws);
    this.send(ws, { t: "msg", text: "You climb down into the lava tube. The walls glow faintly, and something skitters in the dark." });
    questEvent(this, p, "enter", "cave");
    if (!this.timer) this.startLoop();
  }

  /** Can a creature or player step from (x,z) to (nx,nz)? Walls in the cave, bounds everywhere. */
  canStep(x: number, z: number, nx: number, nz: number): boolean {
    if (this.cave) return stepOpen(this.cave, x, z, nx, nz);
    return inBounds(this.bounds!, nx, nz);
  }

  onWalk(p: Player, msg: any) {
    const path = msg.path;
    if (!Array.isArray(path) || path.length === 0) return this.send(p.ws, { t: "err", text: "Empty path." });
    if (path.length > MAX_PATH) return this.send(p.ws, { t: "err", text: "Path too long." });
    const bounds = this.bounds!;
    const steps: [number, number][] = [];
    let [cx, cz] = [p.tx, p.tz];
    const blocked = () => this.send(p.ws, { t: "err", text: "You can't walk there." });
    for (let i = 0; i < path.length; i++) {
      const s = path[i];
      if (!Array.isArray(s) || s.length !== 2) return this.send(p.ws, { t: "err", text: "Bad path step." });
      const tx = Math.floor(Number(s[0])), tz = Math.floor(Number(s[1]));
      if (!Number.isFinite(tx) || !Number.isFinite(tz) || !inBounds(bounds, tx, tz)) return blocked();
      const d = Math.max(Math.abs(tx - cx), Math.abs(tz - cz));
      if (d === 0) continue; // a step onto where we already are
      // The client may be up to one tile ahead of the server; bridge a 2-tile gap on the first step.
      if (d === 2 && steps.length === 0) {
        const mid: [number, number] = [cx + Math.sign(tx - cx), cz + Math.sign(tz - cz)];
        if (!this.canStep(cx, cz, mid[0], mid[1])) return blocked();
        steps.push(mid);
        [cx, cz] = mid;
      } else if (d !== 1) {
        return this.send(p.ws, { t: "err", text: "Path steps must be adjacent tiles." });
      }
      if (!this.canStep(cx, cz, tx, tz)) return blocked();
      steps.push([tx, tz]);
      [cx, cz] = [tx, tz];
    }
    p.path = steps;
    p.run = msg.run === true;
    p.action = null;
    p.dialog = null;
  }

  onAct(p: Player, msg: any) {
    const node = this.nodes.find((n) => n.id === msg.node);
    if (!node) return this.send(p.ws, { t: "err", text: "Nothing there." });
    if (node.kind === "fabricator") return this.send(p.ws, { t: "msg", text: "Use the fabricator by choosing something to craft." });
    if (node.kind === "rift") return this.send(p.ws, { t: "msg", text: "The rift hums with hostile energy. Something is coming through." });
    if (node.kind === "stall") return this.onShopOpen(p, {});
    if (node.kind === "pit" || node.kind === "locker" || node.kind === "ladder") {
      p.action = { kind: "use", node: node.id }; // runs when the queued walk ends next to it
      return;
    }
    const def = NODE_DEFS[node.kind];
    if (levelForXp(p.skills[def.skill]) < def.level) {
      return this.send(p.ws, { t: "msg", text: `You need a ${def.skill} level of ${def.level} to work this ${node.kind.replace("_", " ")}.` });
    }
    // Walk first if a path is queued; the gather starts when it ends next to the node.
    p.action = { kind: "gather", node: node.id, timer: 1 };
  }

  onCraft(p: Player, msg: any) {
    if (this.busy(p)) return;
    if (this.cave) return this.send(p.ws, { t: "msg", text: "There's no fabricator down here." });
    const recipe = RECIPES.find((r) => r.id === msg.recipe);
    if (!recipe) return this.send(p.ws, { t: "err", text: "Unknown recipe." });
    if (recipe.quest && p.quests[recipe.quest]?.s !== "done") {
      return this.send(p.ws, { t: "msg", text: "You don't know how to make that yet. Maybe a colonist can teach you." });
    }
    if (levelForXp(p.skills.fabrication) < recipe.level) {
      return this.send(p.ws, { t: "msg", text: `You need a fabrication level of ${recipe.level} to make that.` });
    }
    if (!this.hasInputs(p, recipe)) return this.send(p.ws, { t: "msg", text: "You don't have the materials for that." });
    p.action = { kind: "craft", recipe, timer: recipe.ticks };
  }

  onDrop(p: Player, msg: any) {
    if (this.busy(p)) return;
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s) return this.send(p.ws, { t: "err", text: "Nothing in that slot." });
    // qty: a count or "all"; without it the whole slot goes (the original message form)
    const q = msg.qty === undefined || msg.qty === "all" ? s.qty : Number.isInteger(msg.qty) && msg.qty > 0 ? Math.min(msg.qty, s.qty) : 0;
    if (!q) return this.send(p.ws, { t: "err", text: "Bad amount." });
    s.qty -= q;
    if (s.qty <= 0) p.inv[i] = null;
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
    this.colony.onChat(p, text);
  }

  /** Walk up to the trader (by colonist id, or any id when omitted), then open the shop. */
  onShopOpen(p: Player, msg: any) {
    const t = this.colony.trader();
    if (!t) return this.send(p.ws, { t: "msg", text: "There's nobody minding the stall right now." });
    if (msg.id !== undefined && msg.id !== t.id) return this.send(p.ws, { t: "err", text: "They don't sell anything." });
    p.dialog = null;
    p.action = { kind: "shop", id: t.id };
  }

  onTalk(p: Player, msg: any) {
    const c = this.colony.get(msg.id);
    if (!c) return this.send(p.ws, { t: "err", text: "They don't have anything to say." });
    p.dialog = null;
    p.action = { kind: "talk", id: c.p.id }; // walks over first if needed
  }

  /** Dev-only (MARSSCAPE_TEST=1): set hp or skills, get items, force a surge, sample hit rates. */
  onTest(p: Player, msg: any) {
    if (Number.isInteger(msg.hp)) {
      p.hp = Math.min(hpLevel(p), Math.max(1, msg.hp));
      this.send(p.ws, { t: "hp", hp: p.hp, max: hpLevel(p) });
    }
    if (msg.skills && typeof msg.skills === "object") {
      for (const s of SKILLS) if (Number.isInteger(msg.skills[s])) p.skills[s] = Math.min(MAX_XP, Math.max(0, msg.skills[s]));
      p.skills.hitpoints = Math.max(HP_START_XP, p.skills.hitpoints);
      p.hp = hpLevel(p);
      p.dirty = true;
      this.send(p.ws, { t: "hp", hp: p.hp, max: hpLevel(p) });
    }
    if (Array.isArray(msg.give)) {
      for (const it of msg.give.slice(0, 28)) if (ITEMS[it]) this.addItem(p, it);
      p.dirty = true;
      this.send(p.ws, { t: "inv", inv: p.inv });
    }
    if (Array.isArray(msg.rawinv)) { // an exact pack, unmerged: stands in for a save from before stacking
      p.inv = Array.from({ length: INV_SLOTS }, (_, i) => {
        const s = msg.rawinv[i];
        return s && ITEMS[s.item] && Number.isInteger(s.qty) && s.qty > 0 ? { item: s.item, qty: s.qty } : null;
      });
      this.savePlayer(p);
      this.send(p.ws, { t: "inv", inv: p.inv });
    }
    if (Number.isInteger(msg.credits) && msg.credits > 0) {
      this.addItems(p, "credits", Math.min(1_000_000, msg.credits));
      p.dirty = true;
      this.send(p.ws, { t: "inv", inv: p.inv });
    }
    if (msg.surge === "now" || msg.surge === "boss") this.surge.force(msg.surge, [p.tx, p.tz]);
    if (typeof msg.defsim === "string" && (NPC_DEFS as any)[msg.defsim]) {
      const def = (NPC_DEFS as Record<string, NpcDef>)[msg.defsim];
      const n = Math.min(100_000, Math.max(1, Number(msg.n) || 10_000));
      let hits = 0;
      for (let i = 0; i < n; i++) if (this.npcHits(def, p)) hits++;
      this.send(p.ws, { t: "test", hit_rate: hits / n, bonus: gearBonus(p.gear) });
    }
  }

  onAttack(p: Player, msg: any) {
    const n = this.npcs.find((n) => n.id === msg.npc);
    if (!n || n.respawnAt) return this.send(p.ws, { t: "err", text: "Nothing to attack there." });
    p.action = { kind: "attack", npc: n.id };
  }

  gearMsg(p: Player) {
    return { t: "gear", weapon: p.weapon, ...p.gear, bonus: gearBonus(p.gear) };
  }

  onEquip(p: Player, msg: any) {
    if (this.busy(p)) return;
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (s && ARMOUR[s.item]) {
      const a = ARMOUR[s.item];
      if (levelForXp(p.skills.defence) < a.level) {
        return this.send(p.ws, { t: "msg", text: `You need a defence level of ${a.level} to wear that.` });
      }
      const old = p.gear[a.slot];
      p.gear[a.slot] = s.item;
      p.inv[i] = old ? { item: old, qty: 1 } : null;
      p.dirty = true;
      this.send(p.ws, this.gearMsg(p));
      this.send(p.ws, { t: "inv", inv: p.inv });
      return;
    }
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
    this.send(p.ws, this.gearMsg(p));
    this.send(p.ws, { t: "inv", inv: p.inv });
  }

  onUnequip(p: Player, msg: any) {
    if (this.busy(p)) return;
    const slot = msg?.gear;
    if (slot === undefined || slot === "weapon") {
      if (!p.weapon) return;
      if (!this.addItem(p, p.weapon)) return this.send(p.ws, { t: "msg", text: "You have no room to put that away." });
      p.weapon = null;
      p.dirty = true;
      this.send(p.ws, { t: "equip", weapon: null });
    } else {
      if (!GEAR_SLOTS.includes(slot)) return this.send(p.ws, { t: "err", text: "No such gear slot." });
      const it = p.gear[slot as GearSlot];
      if (!it) return;
      if (!this.addItem(p, it)) return this.send(p.ws, { t: "msg", text: "You have no room to put that away." });
      p.gear[slot as GearSlot] = null;
      p.dirty = true;
    }
    this.send(p.ws, this.gearMsg(p));
    this.send(p.ws, { t: "inv", inv: p.inv });
  }

  onEat(p: Player, msg: any) {
    if (this.busy(p)) return;
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s || s.item !== "medkit") return this.send(p.ws, { t: "err", text: "You can't use that." });
    if (this.tickN < p.nextEat) return;
    if (--s.qty <= 0) p.inv[i] = null;
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
  /** One step from (x,z) toward (tx,tz): straight, else along one axis; stays put if walled in. */
  stepToward(x: number, z: number, tx: number, tz: number): [number, number] {
    const dx = Math.sign(tx - x), dz = Math.sign(tz - z);
    for (const [nx, nz] of [[x + dx, z + dz], [x + dx, z], [x, z + dz]] as [number, number][]) {
      if ((nx !== x || nz !== z) && this.canStep(x, z, nx, nz)) return [nx, nz];
    }
    return [x, z];
  }

  /** Does a creature's attack land on this player? Armour raises the defence roll. */
  npcHits(def: NpcDef, t: Player): boolean {
    const att = (def.att + 9) * 64;
    return Math.random() < hitChance(att, playerDefRoll(levelForXp(t.skills.defence), gearBonus(t.gear)));
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
    this.surge.onDamage(p, n, dealt);
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
    n.respawnAt = this.tickN + (n.temp ? 5 : NPC_RESPAWN);
    n.target = null;
    n.anim = ANIM.dead;
    this.broadcast({ t: "death", who: `n:${n.id}` });
    for (const o of this.everyone()) if (o.action?.kind === "attack" && o.action.npc === n.id) o.action = null;
    questEvent(this, killer, "kill", n.kind);
    if (this.surge.onKill(n)) return; // the surge boss: shared loot for everyone who helped
    const drops: string[] = ["chitin"];
    const cores = n.def.core >= 1 ? n.def.core : Math.random() < n.def.core ? 1 : 0;
    for (let i = 0; i < cores; i++) drops.push("void_crystal");
    for (const [item, chance] of n.def.rare) if (Math.random() < chance) drops.push(item);
    const got: string[] = [];
    for (const d of drops) if (this.addItem(killer, d)) got.push(ITEMS[d].name);
    const cr = creditDrop(n.def.level);
    drops.push("credits");
    if (this.addItems(killer, "credits", cr)) got.push(`${cr} credits`);
    killer.dirty = true;
    this.send(killer.ws, { t: "inv", inv: killer.inv });
    const lost = got.length < drops.length ? " (some loot was lost: inventory full)" : "";
    this.send(killer.ws, { t: "msg", text: `You defeat the ${n.def.name}. Loot: ${got.join(", ") || "nothing"}${lost}.` });
  }

  killPlayer(p: Player) {
    this.broadcast({ t: "death", who: `p:${p.id}` });
    for (const n of this.npcs) if (n.target === p) { n.target = null; n.leash = true; }
    if (p.weapon) {
      this.send(p.ws, { t: "msg", text: `Your ${ITEMS[p.weapon].name.toLowerCase()} is lost in the dust.` });
      p.weapon = null;
      this.send(p.ws, { t: "equip", weapon: null });
      this.send(p.ws, this.gearMsg(p));
    }
    p.path = [];
    p.action = null;
    p.hp = hpLevel(p);
    p.dirty = true;
    this.send(p.ws, { t: "hp", hp: p.hp, max: p.hp });
    if (this.cave && p.token) {
      this.send(p.ws, { t: "msg", text: "The dark closes in. You wake up back at the landing site." });
      void this.leaveCave(p, "spawn");
      return;
    }
    [p.tx, p.tz] = this.spawn;
    this.send(p.ws, { t: "msg", text: "You were overwhelmed. You wake up back at the landing site." });
    this.colony.onDeath(p);
  }

  tickNpc(n: Npc, all: Player[]) {
    if (n.respawnAt) {
      if (this.tickN >= n.respawnAt) {
        if (n.temp) return this.removeNpc(n);
        n.respawnAt = 0; n.hp = n.def.hp; [n.tx, n.tz] = n.home; n.anim = ANIM.idle; n.leash = false;
      } else n.anim = ANIM.dead;
      return;
    }
    n.anim = ANIM.idle;
    if (n.leash) {
      if (n.tx === n.home[0] && n.tz === n.home[1]) { n.leash = false; n.hp = n.def.hp; return; }
      const [nx, nz] = this.stepToward(n.tx, n.tz, n.home[0], n.home[1]);
      if (nx === n.tx && nz === n.tz) { [n.tx, n.tz] = n.home; n.leash = false; n.hp = n.def.hp; return; } // stuck: blink home
      [n.tx, n.tz] = [nx, nz];
      n.anim = ANIM.walk;
      return;
    }
    // aggression: nearest eligible player within range
    if (!n.target) {
      let best: Player | null = null, bd = AGGRO_RANGE + 1;
      for (const p of all) {
        const d = dist(p.tx, p.tz, n.tx, n.tz);
        if (d >= bd) continue;
        // surge creatures go for anyone; otherwise single-way combat: one creature per player at a time
        if (n.temp || (cbLevel(p) < 2 * n.def.level && !this.npcs.some((o) => o.target === p))) { best = p; bd = d; }
      }
      n.target = best;
    }
    const t = n.target;
    if (t) {
      if (!this.present(t) || dist(n.tx, n.tz, n.home[0], n.home[1]) > LEASH_RANGE) {
        n.target = null; n.leash = true; return;
      }
      if (dist(n.tx, n.tz, t.tx, t.tz) > n.def.range) {
        [n.tx, n.tz] = this.stepToward(n.tx, n.tz, t.tx, t.tz);
        n.anim = ANIM.walk;
        return;
      }
      if (this.tickN < n.nextAttack) return;
      n.nextAttack = this.tickN + n.def.speed;
      const dmg = this.npcHits(n.def, t) ? Math.floor(Math.random() * (n.def.maxHit + 1)) : 0;
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
      if ((nx !== n.tx || nz !== n.tz) && dist(nx, nz, n.home[0], n.home[1]) <= WANDER_RANGE && this.canStep(n.tx, n.tz, nx, nz)) {
        [n.tx, n.tz] = [nx, nz];
        n.anim = ANIM.walk;
      }
    }
  }

  // ---- game helpers -----------------------------------------------------------
  hasInputs(p: Player, r: Recipe): boolean {
    return Object.entries(r.inputs).every(([item, n]) => this.countItem(p, item) >= n);
  }

  /** Run `fn` on p's inventory; if it returns false, put the inventory back exactly as it was. */
  withInv(p: Player, fn: () => boolean): boolean {
    const before = p.inv.map((s) => (s ? { ...s } : null));
    if (fn()) return true;
    p.inv.splice(0, p.inv.length, ...before);
    return false;
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

  /** Add `n` of an item (n > 1 only makes sense for stackables). */
  addItems(p: Player, item: string, n: number): boolean {
    return addTo(p.inv, item, n); // all or nothing
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
    if (!this.cave) this.surge.arm();
    this.timer = setInterval(() => {
      try { this.tick(); } catch (e) { console.error("tick failed", e); }
    }, TICK_MS);
  }

  stopLoop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    this.surge.reset();
    this.shop.persist();
    // the only routine meta writes: once per stop instead of every few ticks
    this.setMeta("tick", String(this.tickN));
    this.setMeta("next_id", String(this.nextId));
  }

  tick() {
    const now = Date.now();
    this.tickN++;

    // sockets that never said hello
    for (const [ws, c] of this.conns) {
      if (!this.players.has(ws) && !c.hello && now - c.opened > HELLO_MS) this.kick(ws, "No hello received.", 1002);
    }

    for (const [id, at] of this.nodeUp) {
      if (this.tickN >= at) {
        this.nodeUp.delete(id);
        this.broadcast({ t: "node", id, up: true });
      }
    }

    this.colony.tick(now); // colonists decide before everyone moves
    if (!this.cave && this.players.size) this.surge.tick();
    const all = this.everyone();
    for (const p of all) {
      if (!this.present(p)) continue; // left during this tick (travel, kick)
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
      } else if (p.action?.kind === "use") {
        anim = this.tickUse(p, p.action);
      } else if (p.action?.kind === "shop") {
        anim = this.tickShop(p, p.action);
      } else if (p.action?.kind === "talk") {
        anim = this.tickTalk(p, p.action);
      }
      p.anim = anim;
      if (this.tickN % REGEN_TICKS === 0 && p.hp < hpLevel(p)) {
        p.hp++;
        this.send(p.ws, { t: "hp", hp: p.hp, max: hpLevel(p) });
      }
    }
    this.trades.tick();
    if (!this.cave) this.shop.tick();
    const alive = this.everyone();
    if (this.players.size) for (const n of [...this.npcs]) this.tickNpc(n, alive);

    const full = this.tickN % FULL_SYNC_TICKS === 0;
    const rows: [number, number, number, number][] = [];
    for (const p of alive) {
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
    }
    if (!this.players.size && !this.conns.size) this.stopLoop(); // let the room hibernate
  }

  /** Lockers, pits and ladders: act when the walk ends next to them. */
  tickUse(p: Player, a: { node: number }): number {
    const node = this.nodes.find((n) => n.id === a.node);
    p.action = null;
    if (!node) return ANIM.idle;
    if (dist(p.tx, p.tz, node.tx, node.tz) > 1) {
      this.send(p.ws, { t: "msg", text: "You need to stand next to that." });
      return ANIM.idle;
    }
    if (node.kind === "locker") this.bank.open(p);
    else if (node.kind === "pit") this.usePit(p);
    else if (node.kind === "ladder" && this.cave) void this.leaveCave(p, "pit");
    return ANIM.idle;
  }

  /** Walk up to the trader, then open the shop window. */
  tickShop(p: Player, a: { id: number }): number {
    const t = this.colony.trader();
    if (!t || t.id !== a.id) { p.action = null; return ANIM.idle; }
    if (dist(p.tx, p.tz, t.tx, t.tz) > SHOP_RANGE) {
      [p.tx, p.tz] = this.stepToward(p.tx, p.tz, t.tx, t.tz);
      p.dirty = true;
      return ANIM.walk;
    }
    p.action = null;
    this.shop.openFor(p);
    return ANIM.idle;
  }

  /** Walk up to a colonist, then open the dialog. */
  tickTalk(p: Player, a: { id: number }): number {
    const c = this.colony.get(a.id);
    if (!c) { p.action = null; return ANIM.idle; }
    if (dist(p.tx, p.tz, c.p.tx, c.p.tz) > TALK_RANGE) {
      [p.tx, p.tz] = this.stepToward(p.tx, p.tz, c.p.tx, c.p.tz);
      p.dirty = true;
      return ANIM.walk;
    }
    p.action = null;
    talk(this, p, c.p, c.role, this.colony.roles());
    return ANIM.idle;
  }

  tickGather(p: Player, a: { node: number; timer: number }): number {
    const node = this.nodes.find((n) => n.id === a.node)!;
    const def = NODE_DEFS[node.kind as GatherKind];
    const stop = (text?: string) => { p.action = null; if (text) this.send(p.ws, { t: "msg", text }); return ANIM.idle; };
    const label = node.kind.replace("_", " ");
    if (Math.max(Math.abs(p.tx - node.tx), Math.abs(p.tz - node.tz)) > 1) return stop("You need to stand next to that.");
    if (this.nodeUp.has(node.id)) return stop(`This ${label} has been worked out for now.`);
    if (p.inv.indexOf(null) < 0 && !(ITEMS[def.item].stack && this.countItem(p, def.item) > 0)) return stop("Your inventory is too full to hold any more.");
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
    questEvent(this, p, "gather", def.item);
    if (Math.random() < def.deplete) {
      this.nodeUp.set(node.id, this.tickN + def.respawn);
      this.broadcast({ t: "node", id: node.id, up: false });
      for (const o of this.everyone()) {
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
    if (!this.hasInputs(p, r) || p.trade) { p.action = null; return ANIM.idle; }
    const ok = this.withInv(p, () => {
      for (const [item, n] of Object.entries(r.inputs)) this.removeItem(p, item, n);
      return this.addItem(p, r.output); // a part-used stack frees no slot, so this can fail
    });
    if (!ok) {
      p.action = null;
      this.send(p.ws, { t: "msg", text: "You don't have room in your pack for that." });
      return ANIM.idle;
    }
    p.dirty = true;
    this.send(p.ws, { t: "inv", inv: p.inv });
    this.send(p.ws, { t: "msg", text: r.text });
    this.addXp(p, "fabrication", r.xp);
    questEvent(this, p, "craft", r.output);
    // keep making more while materials last ("make all")
    if (this.hasInputs(p, r)) { a.timer = r.ticks; return ANIM.craft; }
    p.action = null;
    return ANIM.idle;
  }
}
