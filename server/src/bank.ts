// The colony locker (bank) and player-to-player trading. Both live on the surface room only.
// Every change is written to SQLite in the same synchronous step as the player's inventory, so a
// crash can never leave an item in two places.

import type { Player, Room } from "./index";
import { INV_SLOTS, ITEMS, Slot } from "./game";

export const BANK_SLOTS = 100;
export type Stack = { item: string; qty: number };

const cheb = (ax: number, az: number, bx: number, bz: number) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));

// ---- inventory arithmetic on plain arrays (used for the bank and to dry-run trades) ----------
/** Add `qty` of `item` to an inventory array in place. Returns false (and leaves it untouched) if it won't fit. */
export function addTo(inv: Slot[], item: string, qty: number): boolean {
  if (qty <= 0) return true;
  if (ITEMS[item]?.stack) {
    const s = inv.find((s) => s && s.item === item);
    if (s) { s.qty += qty; return true; }
    const i = inv.indexOf(null);
    if (i < 0) return false;
    inv[i] = { item, qty };
    return true;
  }
  const free: number[] = [];
  inv.forEach((s, i) => { if (s === null) free.push(i); });
  if (free.length < qty) return false;
  for (let k = 0; k < qty; k++) inv[free[k]] = { item, qty: 1 };
  return true;
}

export const count = (inv: Slot[], item: string) => inv.reduce((a, s) => a + (s && s.item === item ? s.qty : 0), 0);

export function takeFrom(inv: Slot[], item: string, qty: number) {
  for (let i = 0; i < inv.length && qty > 0; i++) {
    const s = inv[i];
    if (!s || s.item !== item) continue;
    const t = Math.min(s.qty, qty);
    s.qty -= t;
    qty -= t;
    if (s.qty <= 0) inv[i] = null;
  }
}

const cloneInv = (inv: Slot[]): Slot[] => inv.map((s) => (s ? { ...s } : null));
const qtyArg = (q: unknown, max: number): number =>
  q === "all" ? max : Number.isInteger(q) && (q as number) > 0 ? Math.min(q as number, max) : 0;

// ---- bank -----------------------------------------------------------------------------------
export class Bank {
  private ready = false;
  constructor(private room: Room) {}

  private ensure() {
    if (this.ready) return;
    this.room.sql.exec(`CREATE TABLE IF NOT EXISTS bank (token TEXT PRIMARY KEY, items TEXT NOT NULL)`);
    this.room.sql.exec(`CREATE TABLE IF NOT EXISTS trades (at INTEGER NOT NULL, a TEXT NOT NULL, b TEXT NOT NULL, a_items TEXT NOT NULL, b_items TEXT NOT NULL)`);
    this.ready = true;
  }

  load(token: string): Stack[] {
    this.ensure();
    const r = this.room.sql.exec<{ items: string }>(`SELECT items FROM bank WHERE token = ?`, token).toArray();
    let items: Stack[] = [];
    try { items = r.length ? JSON.parse(r[0].items) : []; } catch { /* empty */ }
    return items.filter((s) => s && ITEMS[s.item] && s.qty > 0).slice(0, BANK_SLOTS);
  }

  private save(p: Player, items: Stack[]) {
    this.ensure();
    this.room.sql.exec(`INSERT INTO bank (token, items) VALUES (?, ?) ON CONFLICT(token) DO UPDATE SET items = excluded.items`,
      p.token, JSON.stringify(items));
    this.room.savePlayer(p); // same synchronous step: committed together
  }

  log(a: Player, b: Player, ai: Stack[], bi: Stack[]) {
    this.ensure();
    this.room.sql.exec(`INSERT INTO trades (at, a, b, a_items, b_items) VALUES (?, ?, ?, ?, ?)`,
      Date.now(), a.token, b.token, JSON.stringify(ai), JSON.stringify(bi));
  }

  private atLocker(p: Player): boolean {
    const l = this.room.nodes.find((n) => n.kind === "locker");
    if (!l || cheb(p.tx, p.tz, l.tx, l.tz) > 1) {
      this.room.send(p.ws, { t: "msg", text: "You need to stand next to the colony locker." });
      return false;
    }
    if (p.trade) { this.room.send(p.ws, { t: "msg", text: "Finish or decline your trade first." }); return false; }
    return true;
  }

  open(p: Player) {
    if (!this.atLocker(p)) return;
    this.room.send(p.ws, { t: "bank", items: this.load(p.token), slots: BANK_SLOTS });
  }

  deposit(p: Player, msg: any) {
    if (!this.atLocker(p)) return;
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s) return this.room.send(p.ws, { t: "err", text: "Nothing in that slot." });
    const item = s.item;
    const qty = qtyArg(msg.qty, count(p.inv, item));
    if (!qty) return this.room.send(p.ws, { t: "err", text: "Bad amount." });
    const items = this.load(p.token);
    const e = items.find((x) => x.item === item);
    if (e) e.qty += qty;
    else if (items.length >= BANK_SLOTS) return this.room.send(p.ws, { t: "msg", text: "Your locker is full." });
    else items.push({ item, qty });
    takeFrom(p.inv, item, qty);
    p.dirty = true;
    this.save(p, items);
    this.room.send(p.ws, { t: "inv", inv: p.inv });
    this.room.send(p.ws, { t: "bank", items, slots: BANK_SLOTS });
  }

  withdraw(p: Player, msg: any) {
    if (!this.atLocker(p)) return;
    const items = this.load(p.token);
    const idx = msg.idx;
    const e = Number.isInteger(idx) && idx >= 0 && idx < items.length ? items[idx] : null;
    if (!e) return this.room.send(p.ws, { t: "err", text: "Nothing in that locker slot." });
    let qty = qtyArg(msg.qty, e.qty);
    if (!qty) return this.room.send(p.ws, { t: "err", text: "Bad amount." });
    if (!ITEMS[e.item].stack) qty = Math.min(qty, p.inv.filter((s) => s === null).length);
    if (!qty || !addTo(p.inv, e.item, qty)) return this.room.send(p.ws, { t: "msg", text: "You don't have enough inventory space." });
    e.qty -= qty;
    if (e.qty <= 0) items.splice(idx, 1);
    p.dirty = true;
    this.save(p, items);
    this.room.send(p.ws, { t: "inv", inv: p.inv });
    this.room.send(p.ws, { t: "bank", items, slots: BANK_SLOTS });
  }
}

// ---- trading --------------------------------------------------------------------------------
export const TRADE_RANGE = 10;
const REQUEST_TICKS = 50; // a trade request stays open for 30 s

export interface Trade { a: Player; b: Player; offers: Map<Player, Stack[]>; acc: Set<Player>; stage: 1 | 2 }

export class Trades {
  private requests = new Map<Player, { to: Player; at: number }>();
  private open = new Set<Trade>();
  constructor(private room: Room, private bank: Bank) {}

  private other(t: Trade, p: Player) { return t.a === p ? t.b : t.a; }

  private state(t: Trade) {
    for (const me of [t.a, t.b]) {
      const them = this.other(t, me);
      this.room.send(me.ws, {
        t: "trade", stage: t.stage, with: them.id, name: them.name,
        mine: t.offers.get(me), theirs: t.offers.get(them),
        accepted: { me: t.acc.has(me), them: t.acc.has(them) },
      });
    }
  }

  private end(t: Trade, result: "done" | "cancelled", reason: string) {
    this.open.delete(t);
    t.a.trade = null;
    t.b.trade = null;
    for (const p of [t.a, t.b]) this.room.send(p.ws, { t: "trade_end", result, reason });
  }

  request(p: Player, msg: any) {
    const r = this.room;
    if (p.trade) return r.send(p.ws, { t: "msg", text: "You're already trading." });
    const o = [...r.players.values()].find((x) => x.id === msg.id);
    if (!o || o === p) return r.send(p.ws, { t: "err", text: "You can't trade with that." });
    if (cheb(p.tx, p.tz, o.tx, o.tz) > TRADE_RANGE) return r.send(p.ws, { t: "msg", text: `${o.name} is too far away to trade.` });
    if (o.trade) return r.send(p.ws, { t: "msg", text: `${o.name} is busy trading.` });
    const theirs = this.requests.get(o);
    if (theirs && theirs.to === p && r.tickN - theirs.at <= REQUEST_TICKS) {
      this.requests.delete(o);
      this.requests.delete(p);
      const t: Trade = { a: o, b: p, offers: new Map([[o, []], [p, []]]), acc: new Set(), stage: 1 };
      o.trade = t;
      p.trade = t;
      o.dialog = p.dialog = null;
      this.open.add(t);
      return this.state(t);
    }
    this.requests.set(p, { to: o, at: r.tickN });
    r.send(o.ws, { t: "trade_request", from: p.id, name: p.name });
    r.send(p.ws, { t: "msg", text: `Sending trade offer to ${o.name}...` });
  }

  offer(p: Player, msg: any) {
    const t = p.trade;
    if (!t) return;
    if (t.stage !== 1) return this.room.send(p.ws, { t: "err", text: "The offer is locked in; accept or decline." });
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s) return this.room.send(p.ws, { t: "err", text: "Nothing in that slot." });
    const mine = t.offers.get(p)!;
    const e = mine.find((x) => x.item === s.item);
    const left = count(p.inv, s.item) - (e?.qty ?? 0);
    const qty = qtyArg(msg.qty, left);
    if (!qty) return this.room.send(p.ws, { t: "err", text: "You've already offered all of those." });
    if (e) e.qty += qty;
    else if (mine.length >= INV_SLOTS) return;
    else mine.push({ item: s.item, qty });
    t.acc.clear();
    this.state(t);
  }

  remove(p: Player, msg: any) {
    const t = p.trade;
    if (!t || t.stage !== 1) return;
    const mine = t.offers.get(p)!;
    const idx = msg.idx;
    const e = Number.isInteger(idx) && idx >= 0 && idx < mine.length ? mine[idx] : null;
    if (!e) return this.room.send(p.ws, { t: "err", text: "Nothing there." });
    const qty = qtyArg(msg.qty, e.qty);
    if (!qty) return;
    e.qty -= qty;
    if (e.qty <= 0) mine.splice(idx, 1);
    t.acc.clear();
    this.state(t);
  }

  accept(p: Player) {
    const t = p.trade;
    if (!t) return;
    t.acc.add(p);
    if (t.acc.size < 2) return this.state(t);
    if (t.stage === 1) {
      t.stage = 2;
      t.acc.clear();
      return this.state(t);
    }
    this.execute(t);
  }

  decline(p: Player) {
    if (p.trade) this.end(p.trade, "cancelled", `${p.name} declined the trade.`);
  }

  /** Swap atomically: both inventories are checked on copies first, then committed together. */
  private execute(t: Trade) {
    const r = this.room;
    const ao = t.offers.get(t.a)!, bo = t.offers.get(t.b)!;
    for (const [p, off] of [[t.a, ao], [t.b, bo]] as [Player, Stack[]][]) {
      if (off.some((s) => count(p.inv, s.item) < s.qty)) return this.end(t, "cancelled", `${p.name} no longer has the items.`);
    }
    const ai = cloneInv(t.a.inv), bi = cloneInv(t.b.inv);
    for (const s of ao) takeFrom(ai, s.item, s.qty);
    for (const s of bo) takeFrom(bi, s.item, s.qty);
    for (const s of bo) if (!addTo(ai, s.item, s.qty)) return this.end(t, "cancelled", `${t.a.name} doesn't have enough inventory space.`);
    for (const s of ao) if (!addTo(bi, s.item, s.qty)) return this.end(t, "cancelled", `${t.b.name} doesn't have enough inventory space.`);
    t.a.inv.splice(0, INV_SLOTS, ...ai);
    t.b.inv.splice(0, INV_SLOTS, ...bi);
    t.a.dirty = t.b.dirty = true;
    r.savePlayer(t.a);
    r.savePlayer(t.b);
    this.bank.log(t.a, t.b, ao, bo);
    for (const p of [t.a, t.b]) r.send(p.ws, { t: "inv", inv: p.inv });
    this.end(t, "done", "Trade complete.");
  }

  /** Each tick: cancel trades whose players drifted apart or left; expire old requests. */
  tick() {
    const r = this.room;
    for (const t of [...this.open]) {
      if (!r.players.has(t.a.ws) || !r.players.has(t.b.ws)) this.end(t, "cancelled", "The other player left.");
      else if (cheb(t.a.tx, t.a.tz, t.b.tx, t.b.tz) > TRADE_RANGE) this.end(t, "cancelled", "You moved too far apart.");
    }
    for (const [p, q] of this.requests) if (r.tickN - q.at > REQUEST_TICKS) this.requests.delete(p);
  }

  onLeave(p: Player) {
    if (p.trade) this.end(p.trade, "cancelled", `${p.name} left.`);
    this.requests.delete(p);
    for (const [k, q] of this.requests) if (q.to === p) this.requests.delete(k);
  }
}
