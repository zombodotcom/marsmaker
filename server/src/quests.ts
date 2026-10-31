// Colonist quests: talk to a colonist, take a job, do it, come back for the reward. Progress is
// part of the character (saved with it, carried into the cave and back).

import type { Player, Room } from "./index";
import type { Role } from "./bots";
import { ITEMS, SKILLS, Skill, lampXp, levelForXp } from "./game";

export type StepKind = "kill" | "gather" | "craft" | "deliver" | "enter";
export interface QuestStep { kind: StepKind; target: string; n: number; text: string }
export interface QuestDef {
  id: string;
  name: string;
  giver: Role;               // falls back to the quartermaster when that role isn't in the room
  qp: number;                // quest points
  req?: Partial<Record<Skill, number>>;
  steps: QuestStep[];
  offer: string;             // what the colonist says when offering it
  thanks: string;            // ...and when you turn it in
  reward: { items: [string, number][]; lamps: number; unlocks?: string[] };
  rewardText: string;
}

/** Per character: quest id -> state. `step` is the current step index, `p` its progress. */
export type QuestState = Record<string, { s: "active" | "done"; step: number; p: number }>;

export const QUESTS: QuestDef[] = [
  {
    id: "skitter_cull", name: "Pest Control", giver: "guard", qp: 1,
    steps: [{ kind: "kill", target: "skitter", n: 5, text: "Kill 5 Void Skitters" }],
    offer: "Skitters keep crawling out of the near rifts. Thin them out: five should do it.",
    thanks: "Nice shooting. The perimeter's quieter already.",
    reward: { items: [["medkit", 1], ["credits", 60]], lamps: 1 }, rewardText: "1 Insight Lamp, 1 medkit, 60 credits",
  },
  {
    id: "brick_drive", name: "Building Blocks", giver: "quartermaster", qp: 1,
    steps: [{ kind: "craft", target: "brick", n: 5, text: "Make 5 regolith bricks at the fabricator" }],
    offer: "The habitat needs bricks. Sinter five at the fabricator and you've earned your keep.",
    thanks: "Good, solid bricks. The colony grows.",
    reward: { items: [["medkit", 1]], lamps: 1 }, rewardText: "1 Insight Lamp, 1 medkit",
  },
  {
    id: "ice_run", name: "Thirsty Colony", giver: "miner", qp: 1, req: { mining: 15 },
    steps: [{ kind: "gather", target: "ice", n: 5, text: "Mine 5 chunks of buried ice" }],
    offer: "Water's running low. Dig up five chunks of ice for the tanks?",
    thanks: "That'll keep the taps running. Take this drill bit, it's spare.",
    reward: { items: [["drill_bit", 1]], lamps: 1 }, rewardText: "1 Insight Lamp, 1 drill bit",
  },
  {
    id: "core_samples", name: "Rock Solid", giver: "surveyor", qp: 1,
    steps: [{ kind: "deliver", target: "sample", n: 3, text: "Bring 3 core samples" }],
    offer: "I need core samples from the pale outcrops. Three of them. Something in the layers is glowing.",
    thanks: "These layers predate the rifts. Mostly. Thanks!",
    reward: { items: [["medkit", 1]], lamps: 1 }, rewardText: "1 Insight Lamp, 1 medkit",
  },
  {
    id: "field_hospital", name: "Field Hospital", giver: "medic", qp: 1, req: { fabrication: 10 },
    steps: [{ kind: "deliver", target: "water", n: 2, text: "Bring 2 water flasks" }],
    offer: "My med bay is dry. Two flasks of water, please: melt some ice at the fabricator.",
    thanks: "Perfect. Here, a couple of medkits for your trouble.",
    reward: { items: [["medkit", 2]], lamps: 1 }, rewardText: "1 Insight Lamp, 2 medkits",
  },
  {
    id: "dust_to_dust", name: "Dust to Dust", giver: "guard", qp: 2, req: { ranged: 15 },
    steps: [{ kind: "kill", target: "stalker", n: 3, text: "Kill 3 Dust Stalkers" }],
    offer: "Dust Stalkers are circling the colony. Put three of them down.",
    thanks: "That's three fewer pairs of eyes in the dust. Here, chitin armour from the last batch.",
    reward: { items: [["chitin_suit", 1], ["credits", 200]], lamps: 2 }, rewardText: "2 Insight Lamps, a chitin suit, 200 credits",
  },
  {
    id: "heavy_metal", name: "Heavy Metal", giver: "guard", qp: 2, req: { ranged: 35 },
    steps: [{ kind: "kill", target: "golem", n: 1, text: "Destroy a Rift Golem" }],
    offer: "A Rift Golem is pulling itself together out past the far rifts. Break it before it finishes.",
    thanks: "You actually did it. Take the crystals: it won't be needing them.",
    reward: { items: [["void_crystal", 5], ["credits", 400]], lamps: 2 }, rewardText: "2 Insight Lamps, 5 void crystals, 400 credits",
  },
  {
    id: "echoes", name: "Echoes Below", giver: "quartermaster", qp: 3, req: { survey: 40 },
    steps: [
      { kind: "enter", target: "cave", n: 1, text: "Climb down the pit into the lava tube" },
      { kind: "gather", target: "ancient_relic", n: 1, text: "Survey a fossil bed in the tube for an ancient relic" },
      { kind: "deliver", target: "ancient_relic", n: 1, text: "Bring the ancient relic to the quartermaster" },
    ],
    offer: "The old Martians went underground, they say. There's a pit near the landing site. Go down and bring me proof.",
    thanks: "A real relic... the glyphs match the ones on the rifts. They didn't seal the void, they were hiding from it. Take these plans: relic armour.",
    reward: { items: [], lamps: 3, unlocks: ["relic_helmet", "relic_suit"] }, rewardText: "3 Insight Lamps, relic armour plans",
  },
];
export const QUEST_BY_ID = new Map(QUESTS.map((q) => [q.id, q]));

/** What the client gets in `welcome.quest_defs`. */
export function questDefsPublic() {
  return QUESTS.map((q) => ({
    id: q.id, name: q.name, giver: q.giver, qp: q.qp, req: q.req ?? {},
    steps: q.steps.map((s) => ({ kind: s.kind, target: s.target, n: s.n, text: s.text })),
    reward: q.rewardText,
  }));
}

export function cleanQuests(raw: unknown): QuestState {
  const out: QuestState = {};
  if (!raw || typeof raw !== "object") return out;
  for (const [id, v] of Object.entries(raw as Record<string, any>)) {
    const q = QUEST_BY_ID.get(id);
    if (!q || !v || (v.s !== "active" && v.s !== "done")) continue;
    const step = Math.min(q.steps.length - 1, Math.max(0, Math.floor(Number(v.step) || 0)));
    out[id] = { s: v.s, step, p: Math.max(0, Math.floor(Number(v.p) || 0)) };
  }
  return out;
}

export const questPoints = (qs: QuestState) => QUESTS.reduce((a, q) => a + (qs[q.id]?.s === "done" ? q.qp : 0), 0);

function sendQuest(room: Room, p: Player, q: QuestDef) {
  const st = p.quests[q.id];
  if (!st) return;
  const step = q.steps[st.step];
  room.send(p.ws, { t: "quest", id: q.id, state: st.s, step: st.step, progress: Math.min(st.p, step.n), goal: step.n, qp: questPoints(p.quests) });
}

/** Progress hook: something happened to `p` (kill/gather/craft/enter) that quests may count. */
export function questEvent(room: Room, p: Player, kind: StepKind, target: string, n = 1) {
  if (!p.token) return; // colonists don't do quests
  for (const q of QUESTS) {
    const st = p.quests[q.id];
    if (!st || st.s !== "active") continue;
    const step = q.steps[st.step];
    if (step.kind !== kind || step.target !== target || st.p >= step.n) continue;
    st.p = Math.min(step.n, st.p + n);
    if (st.p >= step.n && st.step < q.steps.length - 1) {
      st.step++;
      st.p = 0;
      room.send(p.ws, { t: "msg", text: `Quest updated: ${q.name}. ${q.steps[st.step].text}.` });
    } else if (st.p >= step.n) {
      room.send(p.ws, { t: "msg", text: `${q.name}: done! Go back and tell them.` });
    }
    p.dirty = true;
    sendQuest(room, p, q);
  }
}

/** Which role hands out a quest in this room: its giver, or the quartermaster if that role is absent. */
function giverIn(roles: Set<Role>, q: QuestDef): Role {
  return roles.has(q.giver) ? q.giver : "quartermaster";
}

function ready(room: Room, p: Player, q: QuestDef): boolean {
  const st = p.quests[q.id];
  if (!st || st.s !== "active" || st.step !== q.steps.length - 1) return false;
  const step = q.steps[st.step];
  return step.kind === "deliver" ? room.countItem(p, step.target) >= step.n : st.p >= step.n;
}

function meetsReq(p: Player, q: QuestDef): string | null {
  for (const [s, lvl] of Object.entries(q.req ?? {})) {
    if (levelForXp(p.skills[s as Skill]) < lvl!) return `${s} ${lvl}`;
  }
  return null;
}

/** Open a dialog with colonist `c` (role `role`). Sets p.dialog to what the options mean. */
export function talk(room: Room, p: Player, c: Player, role: Role, roles: Set<Role>) {
  const mine = QUESTS.filter((q) => giverIn(roles, q) === role);
  const say = (text: string, quest: string | null, options: { id: string; text: string }[]) => {
    p.dialog = { npc: c.id, quest, opts: options.map((o) => o.id) };
    room.send(p.ws, { t: "dialog", npc: c.id, name: c.name, role, text, quest, options });
  };
  if (role === "trader") {
    return say("Credits for chitin, credits for crystals. I buy almost anything and sell what a colonist needs. Want a look?", null,
      [{ id: "shop", text: "Let's trade." }, { id: "bye", text: "Not now." }]);
  }
  const turnIn = mine.find((q) => ready(room, p, q));
  if (turnIn) {
    return say(`Is that it? ${turnIn.name}, all done?`, turnIn.id, [{ id: "turn_in", text: "Here you go." }, { id: "bye", text: "Not yet." }]);
  }
  const active = mine.find((q) => p.quests[q.id]?.s === "active");
  if (active) {
    const st = p.quests[active.id], step = active.steps[st.step];
    const prog = step.kind === "deliver" ? room.countItem(p, step.target) : st.p;
    return say(`${active.name}: ${step.text} (${Math.min(prog, step.n)}/${step.n}). I'm counting on you.`, active.id, [{ id: "bye", text: "On it." }]);
  }
  const open = mine.filter((q) => !p.quests[q.id] && !meetsReq(p, q));
  if (open.length === 1) return offer(room, p, c, role, open[0]);
  if (open.length > 1) {
    return say("I've got work, if you want it.", null,
      open.map((q) => ({ id: `offer:${q.id}`, text: q.name })).concat([{ id: "bye", text: "Not now." }]));
  }
  const locked = mine.find((q) => !p.quests[q.id]);
  if (locked) return say(`I might have work for you once you've got ${meetsReq(p, locked)}.`, locked.id, [{ id: "bye", text: "Okay." }]);
  say(`Nothing for you right now, ${p.name}. Stay clear of the rifts.`, null, [{ id: "bye", text: "Bye." }]);
}

function offer(room: Room, p: Player, c: Player, role: Role, q: QuestDef) {
  p.dialog = { npc: c.id, quest: q.id, opts: ["accept", "decline"] };
  room.send(p.ws, {
    t: "dialog", npc: c.id, name: c.name, role, quest: q.id, text: `${q.offer} (Reward: ${q.rewardText}.)`,
    options: [{ id: "accept", text: "I'll do it." }, { id: "decline", text: "Not now." }],
  });
}

/** The player picked a dialog option. */
export function choose(room: Room, p: Player, option: string) {
  const d = p.dialog;
  p.dialog = null;
  if (!d || !d.opts.includes(option)) return room.send(p.ws, { t: "err", text: "That option isn't open." });
  if (option.startsWith("offer:")) {
    const c = room.colony.get(d.npc);
    const q = QUEST_BY_ID.get(option.slice(6));
    if (c && q && !p.quests[q.id] && !meetsReq(p, q)) return offer(room, p, c.p, c.role, q);
    return room.send(p.ws, { t: "dialog_end" });
  }
  room.send(p.ws, { t: "dialog_end" });
  if (option === "shop") return room.shop.openFor(p);
  const q = d.quest ? QUEST_BY_ID.get(d.quest) : undefined;
  if (!q) return;
  if (option === "accept") {
    if (p.quests[q.id] || meetsReq(p, q)) return;
    p.quests[q.id] = { s: "active", step: 0, p: 0 };
    p.dirty = true;
    room.send(p.ws, { t: "msg", text: `Quest started: ${q.name}. ${q.steps[0].text}.` });
    return sendQuest(room, p, q);
  }
  if (option === "turn_in") {
    if (!ready(room, p, q)) return room.send(p.ws, { t: "msg", text: "You don't have everything yet." });
    const items: string[] = [];
    const stacks: [string, number][] = [];
    for (const [item, n] of q.reward.items) {
      if (ITEMS[item]?.stack) stacks.push([item, n]);
      else for (let i = 0; i < n; i++) items.push(item);
    }
    for (let i = 0; i < q.reward.lamps; i++) items.push("xp_lamp");
    const step = q.steps[q.steps.length - 1];
    const freed = step.kind === "deliver" && !ITEMS[step.target]?.stack ? step.n : 0;
    const need = items.length + stacks.filter(([i]) => !room.countItem(p, i)).length;
    if (p.inv.filter((s) => s === null).length + freed < need) {
      return room.send(p.ws, { t: "msg", text: "Make some room in your pack first." });
    }
    if (step.kind === "deliver") room.removeItem(p, step.target, step.n);
    for (const i of items) room.addItem(p, i);
    for (const [i, n] of stacks) room.addItems(p, i, n);
    p.quests[q.id] = { s: "done", step: q.steps.length - 1, p: step.n };
    p.dirty = true;
    room.send(p.ws, { t: "inv", inv: p.inv });
    room.send(p.ws, { t: "msg", text: `Quest complete: ${q.name}! You get ${q.rewardText}. (+${q.qp} quest point${q.qp > 1 ? "s" : ""})` });
    if (q.reward.unlocks?.length) room.send(p.ws, { t: "msg", text: "New fabricator recipes unlocked." });
    room.broadcast({ t: "msg", text: `${p.name} completed ${q.name}.` }, p.ws);
    return sendQuest(room, p, q);
  }
}

/** Rub an Insight Lamp on a skill. */
export function rub(room: Room, p: Player, msg: any) {
  const i = msg.slot;
  const s = Number.isInteger(i) && i >= 0 && i < p.inv.length ? p.inv[i] : null;
  if (!s || s.item !== "xp_lamp") return room.send(p.ws, { t: "err", text: "That isn't a lamp." });
  if (!SKILLS.includes(msg.skill)) return room.send(p.ws, { t: "err", text: "Pick a skill." });
  const skill = msg.skill as Skill;
  const xp = lampXp(levelForXp(p.skills[skill]));
  p.inv[i] = null;
  p.dirty = true;
  room.send(p.ws, { t: "inv", inv: p.inv });
  room.send(p.ws, { t: "msg", text: `The lamp flares and memories that aren't yours flood in. (+${xp} ${skill} xp)` });
  room.addXp(p, skill, xp);
}

