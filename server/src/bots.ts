// Colonists: server-side NPC players that make every room feel lived in and help real players.
// To clients they are ordinary players (same tiles, ticks, gather/craft/combat rules, deaths and
// respawns) whose player entries carry "bot":true and a "role". They are never saved, and they
// never keep a room awake: the tick loop stops when the last human leaves, exactly as before.

import type { Npc, Player, Room } from "./index";
import {
  GatherKind, INV_SLOTS, ITEMS, NODE_DEFS, NPC_DEFS, RECIPES, Recipe, SKILLS, Skill, WEAPONS, WorldNode, XP_TABLE,
  clampToBounds, emptyGear, fnv1a, levelForXp, traderPost,
} from "./game";

/** One shared stand-in socket for every colonist. Room.send() skips it, so bots cost no JSON. */
export const BOT_WS = { send() {}, close() {} } as unknown as WebSocket;

export type Role = "quartermaster" | "guard" | "medic" | "miner" | "surveyor" | "trader";

interface RoleDef { levels: Partial<Record<Skill, number>>; weapon: string | null }
const ROLES: Record<Role, RoleDef> = {
  quartermaster: { levels: { fabrication: 50, hitpoints: 20, defence: 20, ranged: 20 }, weapon: "pistol" },
  guard: { levels: { ranged: 40, defence: 35, hitpoints: 40 }, weapon: "rifle" },
  medic: { levels: { hitpoints: 30, defence: 25, ranged: 20 }, weapon: "pistol" },
  miner: { levels: { mining: 35, fabrication: 30, hitpoints: 15, defence: 10 }, weapon: null },
  surveyor: { levels: { survey: 35, hitpoints: 15, defence: 10 }, weapon: null },
  trader: { levels: { hitpoints: 30, defence: 30, ranged: 25 }, weapon: "pistol" },
};

const NAMES = ["Vega", "Orion", "Nova", "Lyra", "Juno", "Atlas", "Rhea", "Mira", "Echo", "Sol", "Wren", "Flint", "Cinder", "Haze", "Pike", "Rook"];

const FOLLOW_TICKS = 200;      // an escort lasts 2 minutes
const HEAL = 5;
const HEAL_COOLDOWN = 8;       // ticks between medic heals
const GUARD_RANGE = 8;         // guards defend players within this many tiles
const WORK_RADIUS = 45;        // gatherers stay this close to spawn
const RIFT_CLEARANCE = 12;     // ...and this far from rifts
const KIT = ["brick", "brick", "brick", "medkit"];
const KIT_CREDITS = 100;
const SAY_GAP = 4;             // ticks between two colonist chat lines in a room

const cheb = (ax: number, az: number, bx: number, bz: number) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));
const maxHp = (p: Player) => levelForXp(p.skills.hitpoints);
const pick = <T>(a: readonly T[]): T => a[Math.floor(Math.random() * a.length)];

/** Straight 8-neighbour steps from (x,z) until within `stop` tiles of (tx,tz), at most `max` steps. */
function line(x: number, z: number, tx: number, tz: number, stop = 0, max = 64): [number, number][] {
  const out: [number, number][] = [];
  while (cheb(x, z, tx, tz) > stop && out.length < max) {
    x += Math.sign(tx - x);
    z += Math.sign(tz - z);
    out.push([x, z]);
  }
  return out;
}

// creature names come from the game tables, so a re-skin never leaves stale text here
const byLevel = () => Object.values(NPC_DEFS).sort((a, b) => a.level - b.level);
const weakest = () => byLevel()[0].name;
const boss = () => byLevel()[byLevel().length - 1].name;

const TIPS = [
  () => "Tip: right-click a rock to mine it. Ice opens up at mining 15.",
  () => "Tip: two regolith at the fabricator make a brick. Easy fabrication xp.",
  () => "Tip: say 'help' in chat and one of us will cover you for two minutes.",
  () => "Tip: a medkit heals 20. Water plus a core sample makes one.",
  () => "Tip: the trader by the fabricator buys almost anything. Creatures drop credits too.",
  () => `Tip: ${weakest()}s swarm near the rifts. Bring a blaster if you can.`,
  () => "Tip: survey the pale outcrops for core samples. Medkits need them.",
];

const LORE = [
  () => "The rifts hum louder at night. The old Martians sealed them for a reason.",
  () => "Found another spiral glyph on an outcrop. Same as the ones in Cydonia.",
  () => "Void crystals sing if you hold them long enough. Don't hold them that long.",
  () => `Something big lives past the far rift. ${boss()}, people call it.`,
  () => "They say the ancient Martians never died out. They went underground.",
  () => "Olympus has been rumbling again. Something down there is waking up.",
  () => "Every rift we close, two more crack open. Keep your blaster charged.",
  () => "Phobos looked closer tonight. It's not, right? Right?",
  () => "Dust storm on the horizon. Seal your visor and don't follow the lights.",
  () => "The ice here is older than the dinosaurs. Tastes like rust.",
  () => "Old rover tracks out east. They stop dead at the edge of a rift.",
];

const DEPOT: Record<"miner" | "surveyor", string[]> = {
  miner: ["Another load for the depot. These bricks won't build themselves.", "Depot's filling up. The colony grows."],
  surveyor: ["Core sample logged. Something in it is still glowing.", "These layers are older than the rifts. Mostly."],
};

const DEATH_LINES = ["Ow. Back at the landing site.", "Note to self: those things bite.", "I'm fine. I'm fine. Mostly fine."];

interface Colonist {
  p: Player;
  role: Role;
  follow: Player | null;  // human being escorted after a "help"
  followUntil: number;    // tick
  wait: number;           // tick: idle until
  plan: number;           // tick: next re-plan while moving toward someone
  healReady: number;      // tick (medic)
  rearmAt: number;        // tick: get the role weapon back after dying
  spoke: number;          // tick of this colonist's last remark
}

export class Colony {
  /** The colonists' Player objects; the room treats them like players. */
  list: Player[] = [];
  private cs: Colonist[] = [];
  private byPlayer = new Map<Player, Colonist>();
  private queue: { at: number; c: Colonist; text: string }[] = [];
  private nextSay = 0;
  private nextFlavor = 0;
  private nextHi = 0;
  private helpAt = new Map<Player, number>();
  private giftPending = new Set<string>(); // tokens of online humans still owed a starter kit
  private giftTable = false;
  private safe: WorldNode[] | null = null;
  private posts: [number, number][] | null = null;

  constructor(private room: Room) {}

  has(p: Player): boolean { return this.byPlayer.has(p); }

  /** Extra fields for a player entry in welcome/join: `{bot:true, role}` for colonists. */
  tag(p: Player): { bot?: true; role?: Role } {
    const c = this.byPlayer.get(p);
    return c ? { bot: true, role: c.role } : {};
  }

  /** The colonist with this player id, for talking to. */
  get(id: unknown): { p: Player; role: Role } | null {
    const c = this.cs.find((c) => c.p.id === id);
    return c ? { p: c.p, role: c.role } : null;
  }

  /** The trader minding the stall, if there is one. */
  trader(): Player | null {
    return this.cs.find((c) => c.role === "trader")?.p ?? null;
  }

  /** Roles present in the room right now (quests fall back to the quartermaster). */
  roles(): Set<Role> {
    return new Set(this.cs.map((c) => c.role));
  }

  /** About 4 colonists with one human, one fewer per extra human, never below 1. */
  rebalance(humans: number) {
    if (!this.room.bounds || humans <= 0 || this.room.cave) return; // nobody lives in the lava tube
    // the trader always minds her stall, on top of the rest of the crew
    if (!this.cs.some((c) => c.role === "trader") && this.room.nodes.some((n) => n.kind === "stall")) this.spawn("trader");
    const want = Math.min(4, Math.max(1, 5 - humans));
    const order = this.roleOrder();
    const crew = () => this.cs.filter((c) => c.role !== "trader");
    while (crew().length < want) {
      const role = order.find((r) => !this.cs.some((c) => c.role === r));
      if (!role) break;
      this.spawn(role);
    }
    while (crew().length > want) {
      const victim = crew().sort((a, b) => order.indexOf(b.role) - order.indexOf(a.role)).find((c) => !c.follow);
      if (!victim) break;
      this.despawn(victim);
    }
  }

  /** Priority order: the last colonist standing in a busy room is the quartermaster. */
  private roleOrder(): Role[] {
    return ["quartermaster", "guard", "medic", fnv1a(this.room.site) & 1 ? "miner" : "surveyor"];
  }

  private spawn(role: Role) {
    const r = this.room;
    const def = ROLES[role];
    const id = r.nextId++;
    const skills = Object.fromEntries(SKILLS.map((s) => [s, 0])) as Record<Skill, number>;
    for (const [s, lvl] of Object.entries(def.levels)) skills[s as Skill] = XP_TABLE[lvl!];
    skills.hitpoints = Math.max(skills.hitpoints, XP_TABLE[10]);
    const fab = r.nodes[0];
    const stall = r.nodes.find((n) => n.kind === "stall");
    const [cx, cz] = role === "quartermaster" ? [fab.tx, fab.tz] : r.spawn;
    const [tx, tz] = role === "trader" && stall ? traderPost(stall)
      : clampToBounds(r.bounds!, cx + Math.floor(Math.random() * 5) - 2, cz + Math.floor(Math.random() * 5) - 2);
    const weapon = def.weapon && WEAPONS[def.weapon] ? def.weapon : null;
    const p: Player = {
      ws: BOT_WS, id, token: "", name: this.nameFor(role), tx, tz, skills, inv: Array(INV_SLOTS).fill(null),
      path: [], run: false, action: null, anim: 0, sent: `${tx},${tz},0`, dirty: false, lastChat: 0,
      lastInput: Date.now(), hp: levelForXp(skills.hitpoints), weapon, nextAttack: 0, nextEat: 0,
      gear: emptyGear(), quests: {}, trade: null, dialog: null, ops: {},
    };
    const c: Colonist = { p, role, follow: null, followUntil: 0, wait: r.tickN + 3, plan: 0, healReady: 0, rearmAt: 0, spoke: 0 };
    this.cs.push(c);
    this.list.push(p);
    this.byPlayer.set(p, c);
    r.broadcast({ t: "join", id, name: p.name, tx, tz, bot: true, role });
  }

  private despawn(c: Colonist) {
    this.cs.splice(this.cs.indexOf(c), 1);
    this.list.splice(this.list.indexOf(c.p), 1);
    this.byPlayer.delete(c.p);
    this.room.broadcast({ t: "leave", id: c.p.id });
  }

  /** Each room has its own stable cast: the same names come back at the same site. */
  private nameFor(role: Role): string {
    const used = new Set(this.list.map((p) => p.name));
    const h = fnv1a(this.room.site + ":" + role);
    for (let i = 0; i < NAMES.length; i++) {
      const n = NAMES[(h + i) % NAMES.length];
      if (!used.has(n)) return n;
    }
    return "Colonist";
  }

  // ---- hooks called by the room -----------------------------------------------
  onJoin(h: Player) {
    if (!this.gifted(h.token)) this.giftPending.add(h.token);
    if (!this.nextFlavor) this.nextFlavor = this.room.tickN + 150;
    const greeter = this.cs.find((c) => c.role === "quartermaster") ?? this.cs[0];
    if (!greeter) return;
    this.say(greeter, this.giftPending.has(h.token)
      ? `Welcome, ${h.name}! Grab your starter kit from me at the fabricator.`
      : `Welcome back, ${h.name}.`, 2);
    this.say(this.cs.find((c) => c !== greeter) ?? greeter, pick(TIPS)(), 9);
  }

  onLeave(h: Player) {
    this.giftPending.delete(h.token);
    this.helpAt.delete(h);
    for (const c of this.cs) if (c.follow === h) this.release(c);
  }

  onDeath(p: Player) {
    const c = this.byPlayer.get(p);
    if (!c) return;
    const t = this.room.tickN;
    c.rearmAt = t + 15;
    c.wait = t + 5;
    if (t - c.spoke > 30) { this.say(c, pick(DEATH_LINES), 3); c.spoke = t; }
  }

  onChat(h: Player, text: string) {
    const t = this.room.tickN;
    const s = text.toLowerCase();
    if (/\b(stop|bye|dismiss)\b/.test(s)) {
      const c = this.cs.find((c) => c.follow === h);
      if (c) this.release(c, `Heading back to my post. Holler if you need me, ${h.name}.`);
      return;
    }
    if (/\b(help|follow me)\b/.test(s)) {
      if (t < (this.helpAt.get(h) ?? 0)) return;
      this.helpAt.set(h, t + 8);
      const mine = this.cs.find((c) => c.follow === h);
      if (mine) return this.say(mine, `Right here, ${h.name}.`);
      const free = this.cs.filter((c) => !c.follow && c.role !== "trader");
      if (!free.length) {
        if (this.cs.length) this.say(this.cs[0], `Everyone's on escort duty. Hang in there, ${h.name}!`);
        return;
      }
      const guards = free.filter((c) => c.role === "guard");
      const c = (guards.length ? guards : free).sort((a, b) => cheb(a.p.tx, a.p.tz, h.tx, h.tz) - cheb(b.p.tx, b.p.tz, h.tx, h.tz))[0];
      c.follow = h;
      c.followUntil = t + FOLLOW_TICKS;
      c.plan = 0;
      c.p.action = null;
      c.p.path = [];
      return this.say(c, `On my way, ${h.name}. I'll cover you for two minutes.`, 1);
    }
    if (/^(hi|hello|hey|yo|sup|howdy|greetings)\b/.test(s) && t >= this.nextHi && this.cs.length) {
      this.nextHi = t + 30;
      const c = [...this.cs].sort((a, b) => cheb(a.p.tx, a.p.tz, h.tx, h.tz) - cheb(b.p.tx, b.p.tz, h.tx, h.tz))[0];
      this.say(c, pick([`Hey ${h.name}.`, `Hi ${h.name}. Stay clear of the rifts.`, `${h.name}! Good to see another face out here.`]), 2);
    }
  }

  /** Runs once per room tick, before players move. Cheap: a few small scans per colonist. */
  tick(now: number) {
    const r = this.room;
    if (!r.players.size || !r.bounds) return;
    if (r.tickN % 10 === 0) this.rebalance(r.players.size);
    for (const c of this.cs) {
      c.p.lastInput = now;
      this.think(c);
    }
    if (this.nextFlavor && r.tickN >= this.nextFlavor) {
      this.nextFlavor = r.tickN + 200 + Math.floor(Math.random() * 200);
      if (this.cs.length && !this.queue.length) this.say(pick(this.cs), pick(LORE)());
    }
    this.flush();
  }

  // ---- behaviour ----------------------------------------------------------------
  private think(c: Colonist) {
    const r = this.room, p = c.p, t = r.tickN;
    const weapon = ROLES[c.role].weapon;
    if (!p.weapon && weapon && WEAPONS[weapon] && t >= c.rearmAt) p.weapon = weapon;
    const attacker = this.attackerOf(p);

    if (c.follow) {
      const h = c.follow;
      if (t >= c.followUntil) {
        this.release(c, `Time's up, ${h.name}. Back to my post.`);
      } else {
        const threat = this.attackerOf(h) ?? attacker;
        if (threat) return this.fight(c, threat);
        if (p.action?.kind === "attack") p.action = null;
        return this.moveNear(c, h.tx, h.tz, 2);
      }
    }
    switch (c.role) {
      case "guard": return this.guard(c, attacker);
      case "medic": return this.medic(c, attacker);
      case "quartermaster": return this.quartermaster(c, attacker);
      case "trader": return this.trader_(c, attacker);
      default: return this.gatherer(c, attacker);
    }
  }

  private attackerOf(v: Player): Npc | undefined {
    return this.room.npcs.find((n) => !n.respawnAt && n.target === v);
  }

  private fight(c: Colonist, n: Npc) {
    const p = c.p;
    if (p.action?.kind === "attack" && p.action.npc === n.id) return;
    p.path = [];
    p.action = { kind: "attack", npc: n.id };
  }

  private moveNear(c: Colonist, x: number, z: number, range: number, run = false) {
    const p = c.p, t = this.room.tickN;
    const d = cheb(p.tx, p.tz, x, z);
    if (d <= range) { p.path = []; return; }
    if (p.path.length && t < c.plan) return;
    p.path = line(p.tx, p.tz, x, z, range);
    p.run = run || d > 6;
    c.plan = t + 3;
  }

  private release(c: Colonist, text?: string) {
    c.follow = null;
    c.p.action = null;
    c.p.path = [];
    c.wait = this.room.tickN + 3;
    if (text) this.say(c, text);
  }

  /**
   * Guards defend anyone within 8 tiles from creatures that jump them. A creature the player is
   * fighting on purpose is left alone, so guards never steal a kill (say "help" for full backup).
   */
  private guard(c: Colonist, attacker: Npc | undefined) {
    const r = this.room, p = c.p, t = r.tickN;
    // a Rift Surge boss is everyone's problem
    const boss = r.surge.boss;
    if (boss && !boss.respawnAt && r.npcs.includes(boss) && cheb(p.tx, p.tz, boss.tx, boss.tz) <= 40) {
      if (!(p.action?.kind === "attack" && p.action.npc === boss.id) && t - c.spoke > 50) { this.say(c, "Surge! Everyone on the rift!"); c.spoke = t; }
      return this.fight(c, boss);
    }
    let threat: Npc | undefined;
    for (const n of r.npcs) {
      const v = n.target;
      if (!v || n.respawnAt || v === p || cheb(p.tx, p.tz, v.tx, v.tz) > GUARD_RANGE) continue;
      if (v.action?.kind === "attack" && v.action.npc === n.id) continue;
      threat = n;
      break;
    }
    if (threat) {
      const v = threat.target!;
      if (!this.byPlayer.has(v) && t - c.spoke > 50) { this.say(c, pick([`Hold on, ${v.name}!`, `Behind me, ${v.name}!`])); c.spoke = t; }
      return this.fight(c, threat);
    }
    if (attacker) return this.fight(c, attacker);
    if (p.action?.kind === "attack") p.action = null;
    if (p.path.length || t < c.wait) return;
    // patrol: the landing site and lookouts a few tiles short of the nearest rifts
    const posts = this.lookouts();
    const [x, z] = pick(posts.filter(([x, z]) => cheb(p.tx, p.tz, x, z) > 2).concat([posts[0]]));
    p.path = line(p.tx, p.tz, x, z);
    p.run = false;
    c.wait = t + p.path.length + 10 + Math.floor(Math.random() * 15);
  }

  private lookouts(): [number, number][] {
    if (this.posts) return this.posts;
    const r = this.room, [sx, sz] = r.spawn;
    const rifts = r.nodes.filter((n) => n.kind === "rift" && cheb(n.tx, n.tz, sx, sz) <= 60)
      .sort((a, b) => cheb(a.tx, a.tz, sx, sz) - cheb(b.tx, b.tz, sx, sz)).slice(0, 3);
    this.posts = [[sx, sz] as [number, number]].concat(rifts.map((n) => {
      const d = cheb(n.tx, n.tz, sx, sz), k = Math.max(0, d - 7) / d;
      return clampToBounds(r.bounds!, Math.round(sx + (n.tx - sx) * k), Math.round(sz + (n.tz - sz) * k));
    }));
    return this.posts;
  }

  private medic(c: Colonist, attacker: Npc | undefined) {
    const r = this.room, p = c.p, t = r.tickN;
    if (attacker) return this.fight(c, attacker);
    if (p.action?.kind === "attack") p.action = null;
    let patient: Player | null = null, worst = 0.5;
    for (const v of [...r.players.values(), ...this.list]) {
      if (v === p || cheb(p.tx, p.tz, v.tx, v.tz) > 40) continue;
      const f = v.hp / maxHp(v);
      if (f < worst) { worst = f; patient = v; }
    }
    if (!patient) {
      const [sx, sz] = r.spawn;
      if (cheb(p.tx, p.tz, sx, sz) > 6) return this.moveNear(c, sx, sz, 3);
      if (!p.path.length && t >= c.wait) {
        p.path = line(p.tx, p.tz, sx + Math.floor(Math.random() * 7) - 3, sz + Math.floor(Math.random() * 7) - 3);
        c.wait = t + 20 + Math.floor(Math.random() * 20);
      }
      return;
    }
    if (cheb(p.tx, p.tz, patient.tx, patient.tz) > 1) return this.moveNear(c, patient.tx, patient.tz, 1, true);
    p.path = [];
    if (t < c.healReady) return;
    const max = maxHp(patient), amt = Math.min(HEAL, max - patient.hp);
    if (amt <= 0) return;
    patient.hp += amt;
    patient.dirty = true;
    c.healReady = t + HEAL_COOLDOWN;
    r.broadcast({ t: "heal", src: `p:${p.id}`, dst: `p:${patient.id}`, amt });
    r.send(patient.ws, { t: "hp", hp: patient.hp, max });
    r.send(patient.ws, { t: "msg", text: `${p.name} patches you up. (+${amt} hp)` });
    if (!this.byPlayer.has(patient) && t - c.spoke > 60) { this.say(c, `Hold still, ${patient.name}. This might sting.`); c.spoke = t; }
  }

  private quartermaster(c: Colonist, attacker: Npc | undefined) {
    const r = this.room, p = c.p, t = r.tickN;
    if (attacker) return this.fight(c, attacker);
    if (p.action?.kind === "attack") p.action = null;
    if (this.giftPending.size) {
      for (const h of r.players.values()) {
        if (this.giftPending.has(h.token) && cheb(p.tx, p.tz, h.tx, h.tz) <= 4) this.giveKit(c, h);
      }
    }
    const fab = r.nodes[0];
    if (cheb(p.tx, p.tz, fab.tx, fab.tz) > 2) return this.moveNear(c, fab.tx, fab.tz, 1);
    if (!p.path.length && t >= c.wait) {
      // pace around the fabricator
      const [x, z] = clampToBounds(r.bounds!, fab.tx + Math.floor(Math.random() * 5) - 2, fab.tz + Math.floor(Math.random() * 5) - 2);
      if (x !== fab.tx || z !== fab.tz) p.path = line(p.tx, p.tz, x, z);
      c.wait = t + 15 + Math.floor(Math.random() * 25);
    }
  }

  /** The trader stays at her stall; she only shoots back if something comes for her. */
  private trader_(c: Colonist, attacker: Npc | undefined) {
    const r = this.room, p = c.p;
    if (attacker) return this.fight(c, attacker);
    if (p.action?.kind === "attack") p.action = null;
    const stall = r.nodes.find((n) => n.kind === "stall");
    if (!stall) return;
    const [x, z] = traderPost(stall);
    if (p.tx !== x || p.tz !== z) this.moveNear(c, x, z, 0);
  }

  private giveKit(c: Colonist, h: Player) {
    const r = this.room, t = r.tickN;
    const kit = KIT.filter((i) => ITEMS[i]);
    if (h.inv.filter((s) => s === null).length < kit.length + (r.countItem(h, "credits") ? 0 : 1)) {
      if (t - c.spoke > 100) { this.say(c, `Your pack's full, ${h.name}. Make some room and come see me.`); c.spoke = t; }
      return;
    }
    for (const i of kit) r.addItem(h, i);
    r.addItems(h, "credits", KIT_CREDITS);
    h.dirty = true;
    this.giftPending.delete(h.token);
    this.markGifted(h.token);
    r.send(h.ws, { t: "inv", inv: h.inv });
    r.send(h.ws, { t: "msg", text: `${c.p.name} hands you a starter kit: 3 regolith bricks, a medkit and ${KIT_CREDITS} credits.` });
    this.say(c, `There you go, ${h.name}. Bricks, a medkit, and credits for the trader.`);
  }

  /** Miners gather ore and turn it into bricks/water/plates; surveyors pull core samples. */
  private gatherer(c: Colonist, attacker: Npc | undefined) {
    const r = this.room, p = c.p, t = r.tickN;
    if (attacker) {
      if (p.hp * 10 < maxHp(p) * 4) { p.action = null; return this.moveNear(c, r.spawn[0], r.spawn[1], 1, true); }
      return this.fight(c, attacker);
    }
    if (p.action?.kind === "attack") p.action = null;
    if (p.action || p.path.length || t < c.wait) return;
    const fab = r.nodes[0];
    if (p.inv.filter(Boolean).length >= 6 || p.inv.indexOf(null) < 0) {
      if (cheb(p.tx, p.tz, fab.tx, fab.tz) > 1) { p.path = line(p.tx, p.tz, fab.tx, fab.tz, 1); p.run = false; return; }
      if (c.role === "miner") {
        const fabLvl = levelForXp(p.skills.fabrication);
        const rec = ["iron_plate", "water", "brick"].map((id) => RECIPES.find((x) => x.id === id))
          .find((x): x is Recipe => !!x && fabLvl >= x.level && r.hasInputs(p, x));
        if (rec) { p.action = { kind: "craft", recipe: rec, timer: rec.ticks }; return; }
      }
      p.inv.fill(null); // into the colony depot
      c.wait = t + 5;
      if (Math.random() < 0.3 && (c.role === "miner" || c.role === "surveyor")) this.say(c, pick(DEPOT[c.role]));
      return;
    }
    const node = this.pickNode(c);
    if (!node) { c.wait = t + 20; return; }
    p.path = line(p.tx, p.tz, node.tx, node.tz, 1);
    p.run = false;
    p.action = { kind: "gather", node: node.id, timer: 1 };
  }

  private pickNode(c: Colonist): WorldNode | null {
    const r = this.room, p = c.p;
    const kinds: GatherKind[] = c.role === "surveyor" ? ["outcrop"]
      : (["regolith", "ice", "hematite"] as GatherKind[]).filter((k) => levelForXp(p.skills.mining) >= NODE_DEFS[k].level);
    const busy = new Set<number>();
    for (const o of [...r.players.values(), ...this.list]) if (o !== p && o.action?.kind === "gather") busy.add(o.action.node);
    const cands = this.safeNodes().filter((n) => kinds.includes(n.kind as GatherKind) && !r.nodeUp.has(n.id) && !busy.has(n.id));
    if (!cands.length) return null;
    cands.sort((a, b) => cheb(p.tx, p.tz, a.tx, a.tz) - cheb(p.tx, p.tz, b.tx, b.tz));
    return cands[Math.floor(Math.random() * Math.min(3, cands.length))];
  }

  /** Gather nodes near spawn and well away from the rifts (computed once per room). */
  private safeNodes(): WorldNode[] {
    if (this.safe) return this.safe;
    const r = this.room, [sx, sz] = r.spawn;
    const rifts = r.nodes.filter((n) => n.kind === "rift");
    const near = r.nodes.filter((n) => n.kind in NODE_DEFS && cheb(n.tx, n.tz, sx, sz) <= WORK_RADIUS);
    const safe = near.filter((n) => rifts.every((rf) => cheb(n.tx, n.tz, rf.tx, rf.tz) >= RIFT_CLEARANCE));
    this.safe = safe.length ? safe : near;
    return this.safe;
  }

  // ---- chat -----------------------------------------------------------------------
  private say(c: Colonist, text: string, delay = 0) {
    if (this.queue.length < 6) this.queue.push({ at: this.room.tickN + delay, c, text: text.slice(0, 80) });
  }

  /** At most one colonist line every SAY_GAP ticks per room; stale lines are dropped. */
  private flush() {
    const t = this.room.tickN;
    if (t < this.nextSay || !this.queue.length) return;
    this.queue = this.queue.filter((q) => this.byPlayer.has(q.c.p) && t - q.at < 40);
    const i = this.queue.findIndex((q) => q.at <= t);
    if (i < 0) return;
    const [q] = this.queue.splice(i, 1);
    this.room.broadcast({ t: "chat", id: q.c.p.id, name: q.c.p.name, msg: q.text });
    this.nextSay = t + SAY_GAP;
  }

  // ---- starter-kit bookkeeping (kept in the room's SQLite, so it survives restarts) ---------
  private gifted(token: string): boolean {
    this.ensureGiftTable();
    return this.room.sql.exec(`SELECT 1 FROM colony_gifts WHERE token = ?`, token).toArray().length > 0;
  }

  private markGifted(token: string) {
    this.ensureGiftTable();
    this.room.sql.exec(`INSERT OR IGNORE INTO colony_gifts (token, at) VALUES (?, ?)`, token, Date.now());
  }

  private ensureGiftTable() {
    if (this.giftTable) return;
    this.room.sql.exec(`CREATE TABLE IF NOT EXISTS colony_gifts (token TEXT PRIMARY KEY, at INTEGER NOT NULL)`);
    this.giftTable = true;
  }
}
