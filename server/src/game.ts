// Pure game rules for MarsScape: XP curve, items, nodes, recipes, world generation.
// No Cloudflare APIs here, so it is easy to reason about and reuse.

export const TICK_MS = 600;
export const INV_SLOTS = 28;
export const MAX_LEVEL = 99;
export const MAX_XP = 200_000_000;
export const SKILLS = ["mining", "survey", "fabrication", "ranged", "defence", "hitpoints"] as const;
export type Skill = (typeof SKILLS)[number];

export type Slot = { item: string; qty: number } | null;

// ---- XP curve ---------------------------------------------------------------
// Classic old-school MMO curve: xp(L) = floor( sum_{l=1}^{L-1} floor(l + 300 * 2^(l/7)) / 4 ).
// Level 2 = 83 xp, level 99 = 13,034,431 xp.
export const XP_TABLE: number[] = (() => {
  const t = [0, 0]; // index = level; t[1] = 0
  let points = 0;
  for (let l = 1; l < MAX_LEVEL; l++) {
    points += Math.floor(l + 300 * Math.pow(2, l / 7));
    t.push(Math.floor(points / 4));
  }
  return t;
})();

export function levelForXp(xp: number): number {
  let lvl = 1;
  while (lvl < MAX_LEVEL && xp >= XP_TABLE[lvl + 1]) lvl++;
  return lvl;
}

// ---- Resource nodes ---------------------------------------------------------
export type NodeKind = "regolith" | "ice" | "hematite" | "outcrop" | "fabricator" | "rift";

export interface NodeDef {
  skill: Skill;
  level: number;     // level required
  xp: number;        // xp per success
  item: string;      // item granted
  deplete: number;   // chance per success that the node depletes
  respawn: number;   // ticks until a depleted node is back
  verb: string;      // "You mine some regolith."
}

export type GatherKind = Exclude<NodeKind, "fabricator" | "rift">;
export const NODE_DEFS: Record<GatherKind, NodeDef> = {
  regolith: { skill: "mining", level: 1, xp: 18, item: "regolith", deplete: 1 / 8, respawn: 8, verb: "You scoop up some regolith." },
  ice: { skill: "mining", level: 15, xp: 35, item: "ice", deplete: 1 / 5, respawn: 20, verb: "You chip off a chunk of buried ice." },
  hematite: { skill: "mining", level: 30, xp: 65, item: "hematite", deplete: 1 / 4, respawn: 45, verb: "You mine some hematite." },
  outcrop: { skill: "survey", level: 1, xp: 25, item: "sample", deplete: 1 / 6, respawn: 25, verb: "You take a core sample from the outcrop." },
};

/** Ticks between gather attempts. */
export const GATHER_TICKS = 4;

/** Chance of a successful gather attempt. */
export function gatherChance(level: number, req: number, drill: boolean): number {
  const c = 0.3 + (level - req) * 0.015 + (drill ? 0.15 : 0);
  return Math.min(0.95, Math.max(0.05, c));
}

// ---- Items & recipes --------------------------------------------------------
export const ITEMS: Record<string, { name: string; stack: boolean }> = {
  regolith: { name: "Regolith", stack: false },
  ice: { name: "Water ice", stack: false },
  hematite: { name: "Hematite", stack: false },
  sample: { name: "Core sample", stack: false },
  brick: { name: "Regolith brick", stack: false },
  water: { name: "Water flask", stack: false },
  iron_plate: { name: "Iron plate", stack: false },
  drill_bit: { name: "Drill bit", stack: false },
  pistol: { name: "Pistol", stack: false },
  rifle: { name: "Rifle", stack: false },
  plasma_gun: { name: "Plasma gun", stack: false },
  medkit: { name: "Medkit", stack: false },
  demon_core: { name: "Demon core", stack: true },
  bones: { name: "Demon bones", stack: false },
};

export interface Recipe {
  id: string;
  level: number;
  xp: number;
  inputs: Record<string, number>;
  output: string;
  ticks: number;
  text: string;
}

export const RECIPES: Recipe[] = [
  { id: "brick", level: 1, xp: 12, inputs: { regolith: 2 }, output: "brick", ticks: 3, text: "You sinter the regolith into a brick." },
  { id: "water", level: 10, xp: 22, inputs: { ice: 1 }, output: "water", ticks: 3, text: "You melt the ice into a flask of water." },
  { id: "iron_plate", level: 25, xp: 45, inputs: { hematite: 2 }, output: "iron_plate", ticks: 3, text: "You smelt the hematite into an iron plate." },
  { id: "medkit", level: 15, xp: 30, inputs: { water: 1, sample: 1 }, output: "medkit", ticks: 3, text: "You pack a medkit." },
  { id: "pistol", level: 20, xp: 60, inputs: { iron_plate: 2 }, output: "pistol", ticks: 4, text: "You assemble a pistol." },
  { id: "rifle", level: 40, xp: 140, inputs: { iron_plate: 4, drill_bit: 1 }, output: "rifle", ticks: 5, text: "You assemble a rifle." },
  { id: "plasma_gun", level: 60, xp: 300, inputs: { iron_plate: 3, demon_core: 5 }, output: "plasma_gun", ticks: 6, text: "You bind the demon cores into a plasma gun. It hums." },
  { id: "drill_bit", level: 35, xp: 90, inputs: { sample: 1, iron_plate: 1 }, output: "drill_bit", ticks: 5, text: "You machine a drill bit. Mining feels easier with it." },
];

// ---- Deterministic RNG ------------------------------------------------------
export function fnv1a(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ---- World generation -------------------------------------------------------
export type Bounds = [number, number, number, number]; // minTx, minTz, maxTx, maxTz (inclusive)

export interface WorldNode {
  id: number;
  kind: NodeKind;
  tx: number;
  tz: number;
}

export const MAX_SIDE = 4096;
export const MAX_NODES = 600;

/** Clamp client-supplied bounds to sane integers, at most MAX_SIDE tiles per side. */
export function clampBounds(b: unknown): Bounds | null {
  if (!Array.isArray(b) || b.length !== 4) return null;
  const n = b.map((v) => Math.floor(Number(v)));
  if (n.some((v) => !Number.isFinite(v) || Math.abs(v) > 1e7)) return null;
  let [x0, z0, x1, z1] = n;
  if (x1 < x0) [x0, x1] = [x1, x0];
  if (z1 < z0) [z0, z1] = [z1, z0];
  // keep the centre, shrink to MAX_SIDE
  if (x1 - x0 + 1 > MAX_SIDE) { const c = Math.floor((x0 + x1) / 2); x0 = c - MAX_SIDE / 2; x1 = x0 + MAX_SIDE - 1; }
  if (z1 - z0 + 1 > MAX_SIDE) { const c = Math.floor((z0 + z1) / 2); z0 = c - MAX_SIDE / 2; z1 = z0 + MAX_SIDE - 1; }
  return [x0, z0, x1, z1];
}

export function inBounds(b: Bounds, tx: number, tz: number): boolean {
  return tx >= b[0] && tx <= b[2] && tz >= b[1] && tz <= b[3];
}

export function clampToBounds(b: Bounds, tx: number, tz: number): [number, number] {
  return [Math.min(b[2], Math.max(b[0], tx)), Math.min(b[3], Math.max(b[1], tz))];
}

/**
 * Deterministic node layout for a site. A quarter of the nodes cluster within ~40 tiles of the
 * room spawn so new players have something to do right away; the rest spread over the bounds.
 * The fabricator always has id 0 and sits a few tiles from spawn.
 */
export function generateNodes(siteId: string, bounds: Bounds, spawn: [number, number]): WorldNode[] {
  const rand = mulberry32(fnv1a("marsscape:" + siteId));
  const w = bounds[2] - bounds[0] + 1;
  const h = bounds[3] - bounds[1] + 1;
  const count = Math.max(12, Math.min(MAX_NODES, Math.floor((w * h) / 400)));
  const used = new Set<string>();
  const nodes: WorldNode[] = [];
  const [fx, fz] = clampToBounds(bounds, spawn[0] + 3, spawn[1] + 3);
  nodes.push({ id: 0, kind: "fabricator", tx: fx, tz: fz });
  // keep the fabricator's neighbourhood clear
  for (let dx = -1; dx <= 1; dx++) for (let dz = -1; dz <= 1; dz++) used.add(`${fx + dx},${fz + dz}`);
  used.add(`${spawn[0]},${spawn[1]}`);

  // guarantee one of each kind close to spawn, then weighted random kinds
  const near: NodeKind[] = ["regolith", "regolith", "regolith", "outcrop", "outcrop", "ice", "hematite"];
  const pickKind = (): NodeKind => {
    const r = rand();
    return r < 0.4 ? "regolith" : r < 0.65 ? "outcrop" : r < 0.85 ? "ice" : "hematite";
  };
  const nearCount = Math.floor(count / 4);
  let tries = 0;
  while (nodes.length - 1 < count && tries < count * 50) {
    tries++;
    const i = nodes.length - 1;
    let tx: number, tz: number;
    if (i < nearCount) {
      const ang = rand() * Math.PI * 2;
      const r = 4 + rand() * 36;
      tx = Math.round(spawn[0] + Math.cos(ang) * r);
      tz = Math.round(spawn[1] + Math.sin(ang) * r);
    } else {
      tx = bounds[0] + Math.floor(rand() * w);
      tz = bounds[1] + Math.floor(rand() * h);
    }
    const kind = i < near.length ? near[i] : pickKind();
    if (!inBounds(bounds, tx, tz)) continue;
    const key = `${tx},${tz}`;
    if (used.has(key)) continue;
    used.add(key);
    nodes.push({ id: nodes.length, kind, tx, tz });
  }
  return nodes;
}

// ---- Text hygiene -----------------------------------------------------------
const BAD_WORDS = ["fuck", "shit", "cunt", "nigger", "nigga", "faggot", "fag", "retard", "bitch", "whore", "slut", "kike", "spic", "chink"];

export function filterWords(s: string): string {
  let out = s;
  for (const w of BAD_WORDS) {
    const re = new RegExp(w.split("").join("[\\W_]*"), "gi");
    out = out.replace(re, (m) => "*".repeat(m.length));
  }
  return out;
}

export function cleanName(raw: unknown): string {
  let s = typeof raw === "string" ? raw : "";
  s = s.replace(/[^A-Za-z0-9 _-]/g, "").replace(/\s+/g, " ").trim().slice(0, 12).trim();
  if (filterWords(s) !== s) s = "";
  return s;
}

export function cleanChat(raw: unknown): string {
  let s = typeof raw === "string" ? raw : "";
  // strip control chars (incl. bidi overrides) and collapse whitespace
  s = s.replace(/[\u0000-\u001f\u007f-\u009f​-‏‪-‮⁦-⁩]/g, "").replace(/\s+/g, " ").trim();
  return filterWords(s.slice(0, 80));
}

// ---- Combat -----------------------------------------------------------------
export const HP_START_XP = 1154; // hitpoints level 10

export interface WeaponDef { level: number; speed: number; acc: number; str: number; range: number }
/** null key = bare hands (a 1-tile punch). */
export const WEAPONS: Record<string, WeaponDef> = {
  none: { level: 1, speed: 4, acc: 10, str: 50, range: 1 },
  pistol: { level: 1, speed: 4, acc: 30, str: 60, range: 7 },
  rifle: { level: 20, speed: 5, acc: 50, str: 100, range: 7 },
  plasma_gun: { level: 40, speed: 6, acc: 80, str: 150, range: 7 },
};

/** Old-school combat level from ranged, defence and hitpoints (no melee/magic/prayer here). */
export function combatLevel(ranged: number, defence: number, hitpoints: number): number {
  const base = 0.25 * (defence + hitpoints);
  const range = 0.325 * Math.floor(ranged * 1.5);
  return Math.floor(base + range);
}

/** Classic accuracy roll comparison. */
export function hitChance(attRoll: number, defRoll: number): number {
  return attRoll > defRoll ? 1 - (defRoll + 2) / (2 * (attRoll + 1)) : attRoll / (2 * (defRoll + 1));
}

export function maxHit(level: number, strBonus: number): number {
  return Math.floor(0.5 + ((level + 8) * (strBonus + 64)) / 640);
}

export type NpcKind = "imp" | "hellhound" | "brute" | "rift_lord";
export interface NpcDef {
  level: number; hp: number; att: number; def: number; maxHit: number; speed: number; range: number;
  core: number;    // chance of a demon_core drop (>=1 means always, value = count)
  rare: [string, number][]; // [item, chance]
}
export const NPC_DEFS: Record<NpcKind, NpcDef> = {
  imp: { level: 5, hp: 5, att: 1, def: 1, maxHit: 1, speed: 4, range: 1, core: 0.2, rare: [["pistol", 1 / 25]] },
  hellhound: { level: 20, hp: 30, att: 18, def: 15, maxHit: 5, speed: 4, range: 1, core: 0.5, rare: [["pistol", 1 / 15], ["rifle", 1 / 80]] },
  brute: { level: 45, hp: 70, att: 40, def: 38, maxHit: 10, speed: 6, range: 1, core: 1, rare: [["rifle", 1 / 30]] },
  rift_lord: { level: 90, hp: 250, att: 85, def: 80, maxHit: 20, speed: 5, range: 4, core: 5, rare: [["plasma_gun", 1 / 4]] },
};
export const NPC_RESPAWN = 50;
export const AGGRO_RANGE = 5;
export const LEASH_RANGE = 15;
export const WANDER_RANGE = 4;

export interface WorldNpc { id: number; kind: NpcKind; home: [number, number] }

/**
 * Place rifts (as extra nodes) and their demons. The rift nearest spawn hosts imps; rifts further
 * away host tougher demons; the farthest rift also hosts a single rift lord.
 */
export function generateRifts(siteId: string, bounds: Bounds, spawn: [number, number], nodes: WorldNode[]): WorldNpc[] {
  const rand = mulberry32(fnv1a("marsscape-rifts:" + siteId));
  const w = bounds[2] - bounds[0] + 1;
  const h = bounds[3] - bounds[1] + 1;
  const count = Math.max(3, Math.min(40, Math.floor((w * h) / 40000)));
  const used = new Set(nodes.map((n) => `${n.tx},${n.tz}`));
  const rifts: WorldNode[] = [];
  let tries = 0;
  while (rifts.length < count && tries++ < count * 100) {
    let tx: number, tz: number;
    if (rifts.length < 2) {
      // two starter rifts 25-40 tiles out, out of aggro range of the spawn
      const ang = rand() * Math.PI * 2, r = 25 + rand() * 15;
      tx = Math.round(spawn[0] + Math.cos(ang) * r);
      tz = Math.round(spawn[1] + Math.sin(ang) * r);
    } else {
      tx = bounds[0] + Math.floor(rand() * w);
      tz = bounds[1] + Math.floor(rand() * h);
    }
    if (!inBounds(bounds, tx, tz) || used.has(`${tx},${tz}`)) continue;
    if (Math.max(Math.abs(tx - spawn[0]), Math.abs(tz - spawn[1])) < 20) continue;
    used.add(`${tx},${tz}`);
    rifts.push({ id: nodes.length + rifts.length, kind: "rift", tx, tz });
  }
  nodes.push(...rifts);
  const dist = (n: WorldNode) => Math.hypot(n.tx - spawn[0], n.tz - spawn[1]);
  const far = rifts.reduce((a, b) => (dist(b) > dist(a) ? b : a), rifts[0]);
  const npcs: WorldNpc[] = [];
  rifts.forEach((rf, i) => {
    const d = dist(rf);
    const kind: NpcKind = i < 2 || d < 80 ? "imp" : d < 250 ? (rand() < 0.7 ? "hellhound" : "imp") : rand() < 0.6 ? "brute" : "hellhound";
    const n = kind === "imp" ? 4 : kind === "hellhound" ? 3 : 2;
    for (let k = 0; k < n; k++) {
      npcs.push({ id: npcs.length + 1, kind, home: clampToBounds(bounds, rf.tx + Math.round(rand() * 4 - 2), rf.tz + Math.round(rand() * 4 - 2)) });
    }
    if (rf === far && rifts.length >= 3) npcs.push({ id: npcs.length + 1, kind: "rift_lord", home: [rf.tx, rf.tz] });
  });
  return npcs;
}
