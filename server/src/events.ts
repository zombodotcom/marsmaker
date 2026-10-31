// Rift Surge: every 20-30 minutes of a room being in use, a big rift tears open near the landing
// site. Waves of creatures pour out, then a Void Leviathan. Everyone who did at least 5% of the
// boss's hp gets a loot roll. Only runs while the room ticks (humans present); when the room goes
// quiet the surge is cleared, so it never keeps an empty room awake.

import type { Npc, Player, Room } from "./index";
import { ITEMS, NpcKind, clampToBounds, fnv1a, mulberry32 } from "./game";

const WARN_TICKS = 200;      // 2 minutes of warning
const WAVE_GAP = 20;         // ticks between waves
const WAVES: [NpcKind, number][][] = [
  [["skitter", 4]],
  [["skitter", 2], ["stalker", 2]],
  [["stalker", 2], ["golem", 1]],
];
const BOSS_TICKS = 600;      // the boss leaves after 6 minutes if nobody kills it
const MIN_SHARE = 0.05;      // damage share needed for a loot roll

export type Phase = "idle" | "warning" | "open" | "boss";

export class Surge {
  phase: Phase = "idle";
  next = 0;                  // tick the next warning starts (0 = not scheduled)
  until = 0;                 // tick the current phase ends
  wave = 0;
  spot: [number, number] = [0, 0];
  boss: Npc | null = null;
  count = 0;
  forcedFast = false;        // dev test: a 3-tick warning
  private dmg = new Map<string, { p: Player; d: number }>(); // token -> damage to the boss
  constructor(private room: Room) {}

  /** Called when the room starts ticking. */
  arm() {
    if (this.phase === "idle" && !this.next) this.next = this.room.tickN + 2000 + Math.floor(Math.random() * 1000);
  }

  /** Called when the room stops ticking: clear everything. */
  reset() {
    for (const n of this.room.npcs.filter((n) => n.temp)) this.room.removeNpc(n);
    this.phase = "idle";
    this.next = 0;
    this.boss = null;
    this.dmg.clear();
  }

  eventMsg() {
    if (this.phase === "idle") return null;
    return { t: "event", kind: "surge", phase: this.phase, tx: this.spot[0], tz: this.spot[1], ends_in: Math.max(0, this.until - this.room.tickN) };
  }

  private announce(text?: string) {
    this.room.broadcast(this.eventMsg() ?? { t: "event", kind: "surge", phase: "ended", tx: this.spot[0], tz: this.spot[1], ends_in: 0 });
    if (text) this.room.broadcast({ t: "msg", text });
  }

  private pickSpot(): [number, number] {
    const r = this.room;
    const rand = mulberry32(fnv1a(`marsscape-surge:${r.site}:${this.count}`));
    const ang = rand() * Math.PI * 2, d = 15 + rand() * 20;
    return clampToBounds(r.bounds!, Math.round(r.spawn[0] + Math.cos(ang) * d), Math.round(r.spawn[1] + Math.sin(ang) * d));
  }

  /** Start the warning now (dev test hook), or with `boss`, skip straight to the boss near `at`. */
  force(mode: "now" | "boss", at?: [number, number]) {
    this.reset();
    if (mode === "now") { this.next = this.room.tickN + 1; this.forcedFast = true; return; }
    this.spot = at ? clampToBounds(this.room.bounds!, at[0] + 3, at[1]) : this.pickSpot();
    this.count++;
    this.spawnBoss();
  }

  private spawnBoss() {
    this.phase = "boss";
    this.until = this.room.tickN + BOSS_TICKS;
    this.dmg.clear();
    this.boss = this.room.spawnTempNpc("leviathan", this.spot);
    this.announce("The rift screams. A Void Leviathan forces its way through!");
  }

  tick() {
    const r = this.room, t = r.tickN;
    switch (this.phase) {
      case "idle":
        if (this.next && t >= this.next) {
          this.spot = this.pickSpot();
          this.count++;
          this.phase = "warning";
          this.until = t + (this.forcedFast ? 3 : WARN_TICKS);
          this.forcedFast = false;
          this.announce("The ground hums. A Rift Surge is building near the landing site!");
        }
        return;
      case "warning":
        if (t >= this.until) {
          this.phase = "open";
          this.wave = 0;
          this.until = t + WAVE_GAP * WAVES.length;
          this.announce("A Rift Surge tears open! Creatures are pouring out.");
          this.spawnWave();
        }
        return;
      case "open":
        if (t >= this.until) return this.spawnBoss();
        if ((this.until - t) % WAVE_GAP === 0) this.spawnWave();
        return;
      case "boss":
        if (!this.boss || !r.npcs.includes(this.boss)) return this.finish(false);
        if (t >= this.until) return this.finish(false);
        return;
    }
  }

  private spawnWave() {
    const w = WAVES[this.wave++];
    if (!w) return;
    for (const [kind, n] of w) {
      for (let i = 0; i < n; i++) {
        const home = clampToBounds(this.room.bounds!, this.spot[0] + Math.floor(Math.random() * 7) - 3, this.spot[1] + Math.floor(Math.random() * 7) - 3);
        this.room.spawnTempNpc(kind, home);
      }
    }
  }

  onDamage(p: Player, n: Npc, dealt: number) {
    if (n !== this.boss || !p.token || dealt <= 0) return;
    const e = this.dmg.get(p.token);
    if (e) { e.d += dealt; e.p = p; } else this.dmg.set(p.token, { p, d: dealt });
  }

  /** Returns true if this kill was the surge boss (its loot is handled here). */
  onKill(n: Npc): boolean {
    if (n !== this.boss) return false;
    this.finish(true);
    return true;
  }

  private finish(killed: boolean) {
    const r = this.room;
    const boss = this.boss;
    if (killed && boss) {
      const total = boss.def.hp;
      const names: string[] = [];
      for (const { p, d } of this.dmg.values()) {
        if (d < total * MIN_SHARE || !r.players.has(p.ws)) continue;
        const drops: string[] = [];
        const crystals = 3 + Math.floor(Math.random() * 6);
        for (let i = 0; i < crystals; i++) drops.push("void_crystal");
        if (Math.random() < 1 / 8) drops.push("rift_blade");
        if (Math.random() < 1 / 5) drops.push("void_cape");
        const got: string[] = [];
        for (const it of drops) if (r.addItem(p, it)) got.push(ITEMS[it].name);
        p.dirty = true;
        r.send(p.ws, { t: "inv", inv: p.inv });
        r.send(p.ws, { t: "msg", text: `Surge loot (${Math.round((100 * d) / total)}% of the damage): ${summarize(got)}.` });
        names.push(p.name);
      }
      this.phase = "idle";
      this.announce(`The Void Leviathan falls! The rift seals. ${names.length ? `Heroes: ${names.join(", ")}.` : ""}`.trim());
    } else {
      this.phase = "idle";
      this.announce("The rift collapses, dragging the creatures back into the void.");
    }
    for (const n of r.npcs.filter((n) => n.temp && n !== boss)) r.removeNpc(n);
    if (boss && r.npcs.includes(boss) && !killed) r.removeNpc(boss);
    this.boss = null;
    this.dmg.clear();
    this.next = r.tickN + 2000 + Math.floor(Math.random() * 1000);
  }
}

function summarize(names: string[]): string {
  const c = new Map<string, number>();
  for (const n of names) c.set(n, (c.get(n) ?? 0) + 1);
  return [...c].map(([n, k]) => (k > 1 ? `${k} x ${n}` : n)).join(", ") || "nothing (inventory full)";
}
