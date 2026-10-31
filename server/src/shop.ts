// The trader's shop: a shared shelf per surface room, buy / sell / buyback for credits.
// Every transaction is checked on copies first and committed in one synchronous step together
// with the player's save, like the bank, so nothing can be duplicated or lost.

import type { Player, Room } from "./index";
import {
  INV_SLOTS, ITEMS, Slot, VALUE, UNSELLABLE, SHOP_STOCK, SHOP_NORMAL, SHOP_MAX, RESTOCK_TICKS,
  buyPrice, sellPrice,
} from "./game";
import { addTo, count, takeFrom } from "./bank";

export const SHOP_RANGE = 2;          // tiles from the trader
const BUYBACK_SLOTS = 10;
const BUYBACK_MS = 30 * 60 * 1000;    // sold items can be bought back for 30 minutes (until logout)
const MAX_QTY = 1000;                 // per transaction

export type Shelf = { item: string; qty: number };
export type Buyback = { item: string; qty: number; price: number; at: number }; // price = total paid out

const cheb = (ax: number, az: number, bx: number, bz: number) => Math.max(Math.abs(ax - bx), Math.abs(az - bz));
const cloneInv = (inv: Slot[]): Slot[] => inv.map((s) => (s ? { ...s } : null));
const qtyArg = (q: unknown, max: number): number =>
  q === "all" ? max : Number.isInteger(q) && (q as number) > 0 ? Math.min(q as number, max, MAX_QTY) : 0;

export class Shop {
  shelf: Shelf[] = [];
  private loaded = false;
  private dirty = false;
  /** Players with the shop window open: they get the shelf again when it changes. */
  private open = new Set<Player>();

  constructor(private room: Room) {}

  private ensure() {
    if (this.loaded) return;
    this.loaded = true;
    const sql = this.room.sql;
    sql.exec(`CREATE TABLE IF NOT EXISTS shop_stock (item TEXT PRIMARY KEY, qty INTEGER NOT NULL)`);
    const rows = sql.exec<{ item: string; qty: number }>(`SELECT item, qty FROM shop_stock`).toArray();
    const saved = new Map(rows.filter((r) => ITEMS[r.item] && r.qty >= 0).map((r) => [r.item, Math.min(SHOP_MAX, r.qty)]));
    this.shelf = SHOP_STOCK.map(([item, n]) => ({ item, qty: saved.get(item) ?? n }));
    for (const [item, qty] of saved) if (!SHOP_NORMAL[item] && qty > 0) this.shelf.push({ item, qty });
  }

  /** Written when the room stops (and occasionally while busy), not on every sale. */
  persist() {
    if (!this.dirty) return;
    this.dirty = false;
    const sql = this.room.sql;
    sql.exec(`DELETE FROM shop_stock`);
    for (const s of this.shelf) sql.exec(`INSERT INTO shop_stock (item, qty) VALUES (?, ?)`, s.item, s.qty);
    this.room.rowsWritten += this.shelf.length + 1;
  }

  private qtyOf(item: string) { return this.shelf.find((s) => s.item === item)?.qty ?? 0; }

  private setQty(item: string, qty: number) {
    const s = this.shelf.find((x) => x.item === item);
    if (s) s.qty = qty;
    else if (qty > 0) this.shelf.push({ item, qty });
    // items she doesn't normally stock leave the shelf when they run out
    this.shelf = this.shelf.filter((x) => x.qty > 0 || SHOP_NORMAL[x.item] !== undefined);
    this.dirty = true;
  }

  /** Total cost of buying `qty` one at a time (each unit makes the next a little dearer). */
  private costToBuy(item: string, qty: number): number {
    let have = this.qtyOf(item), total = 0;
    for (let i = 0; i < qty; i++) total += buyPrice(item, have--);
    return total;
  }

  /** Total credits for selling `qty` one at a time. */
  private payForSale(item: string, qty: number): number {
    let have = this.qtyOf(item), total = 0;
    for (let i = 0; i < qty; i++) total += sellPrice(item, have++);
    return total;
  }

  /** The trader in this room and where she stands, or null (in the cave, or before colonists exist). */
  private trader(): Player | null {
    return this.room.colony.trader();
  }

  private near(p: Player): boolean {
    const t = this.trader();
    if (!t) { this.room.send(p.ws, { t: "msg", text: "There's nobody minding the stall right now." }); return false; }
    if (cheb(p.tx, p.tz, t.tx, t.tz) > SHOP_RANGE) {
      this.room.send(p.ws, { t: "msg", text: `You need to stand next to ${t.name}'s stall.` });
      this.close(p, "You walked away from the stall.");
      return false;
    }
    if (p.trade) { this.room.send(p.ws, { t: "msg", text: "Finish or decline your trade first." }); return false; }
    return true;
  }

  /** The window contents for one player: shelf with prices, their buyback list, credits, sell prices for their pack. */
  private state(p: Player) {
    const now = Date.now();
    p.buyback = (p.buyback ?? []).filter((b) => now - b.at < BUYBACK_MS);
    const sell: Record<string, number> = {};
    for (const s of p.inv) if (s && !(s.item in sell)) sell[s.item] = sellPrice(s.item, this.qtyOf(s.item));
    const t = this.trader();
    return {
      t: "shop", npc: t?.id ?? 0, name: t?.name ?? "Trader",
      stock: this.shelf.map((s) => ({ item: s.item, qty: s.qty, price: s.qty > 0 ? buyPrice(s.item, s.qty) : 0, sell: sellPrice(s.item, s.qty) })),
      buyback: p.buyback.map((b) => ({ item: b.item, qty: b.qty, price: b.price, expires_in: Math.ceil((BUYBACK_MS - (now - b.at)) / 1000) })),
      sell, credits: count(p.inv, "credits"),
    };
  }

  private refresh(p: Player) { this.room.send(p.ws, this.state(p)); }

  /** Everyone else with the window open sees the new shelf. */
  private refreshOthers(except: Player) {
    for (const o of this.open) if (o !== except) this.refresh(o);
  }

  isOpen(p: Player) { return this.open.has(p); }

  openFor(p: Player) {
    this.ensure();
    if (this.room.cave) return this.room.send(p.ws, { t: "msg", text: "There's no trader down here." });
    if (!this.near(p)) return;
    p.dialog = null;
    this.open.add(p);
    this.refresh(p);
  }

  close(p: Player, reason = "") {
    if (!this.open.delete(p)) return;
    this.room.send(p.ws, { t: "shop_end", reason });
  }

  buy(p: Player, msg: any) {
    this.ensure();
    if (!this.open.has(p) || !this.near(p)) return;
    const idx = msg.idx;
    const s = Number.isInteger(idx) && idx >= 0 && idx < this.shelf.length ? this.shelf[idx] : null;
    if (!s || s.qty <= 0) return this.room.send(p.ws, { t: "msg", text: "She's out of those." });
    const item = s.item;
    let qty = qtyArg(msg.qty, s.qty);
    if (!qty) return this.room.send(p.ws, { t: "err", text: "Bad amount." });
    const credits = count(p.inv, "credits");
    // as many as you can afford
    while (qty > 0 && this.costToBuy(item, qty) > credits) qty--;
    if (!qty) return this.room.send(p.ws, { t: "msg", text: `You can't afford that. (${buyPrice(item, s.qty)} credits each)` });
    const cost = this.costToBuy(item, qty);
    const inv = cloneInv(p.inv);
    takeFrom(inv, "credits", cost);
    if (!ITEMS[item].stack) qty = Math.min(qty, inv.filter((x) => x === null).length);
    if (!qty) return this.room.send(p.ws, { t: "msg", text: "You don't have enough inventory space." });
    const finalCost = this.costToBuy(item, qty);
    const inv2 = cloneInv(p.inv);
    takeFrom(inv2, "credits", finalCost);
    if (!addTo(inv2, item, qty)) return this.room.send(p.ws, { t: "msg", text: "You don't have enough inventory space." });
    p.inv.splice(0, INV_SLOTS, ...inv2);
    this.setQty(item, s.qty - qty);
    p.dirty = true;
    this.room.savePlayer(p);
    this.room.send(p.ws, { t: "inv", inv: p.inv });
    this.room.send(p.ws, { t: "msg", text: `You buy ${qty > 1 ? qty + " x " : ""}${ITEMS[item].name.toLowerCase()} for ${finalCost} credits.` });
    this.refresh(p);
    this.refreshOthers(p);
  }

  sell(p: Player, msg: any) {
    this.ensure();
    if (!this.open.has(p) || !this.near(p)) return;
    const i = msg.slot;
    const s = Number.isInteger(i) && i >= 0 && i < INV_SLOTS ? p.inv[i] : null;
    if (!s) return this.room.send(p.ws, { t: "err", text: "Nothing in that slot." });
    const item = s.item;
    if (UNSELLABLE.has(item) || !(item in VALUE)) {
      return this.room.send(p.ws, { t: "msg", text: item === "credits" ? "That's money." : `${this.trader()?.name ?? "The trader"} won't take that. It's bound to you.` });
    }
    const qty = Math.min(qtyArg(msg.qty, count(p.inv, item)), SHOP_MAX - this.qtyOf(item));
    if (qty <= 0) return this.room.send(p.ws, { t: "msg", text: "She can't take any more of those." });
    const pay = this.payForSale(item, qty);
    const inv = cloneInv(p.inv);
    takeFrom(inv, item, qty);
    if (pay > 0 && !addTo(inv, "credits", pay)) return this.room.send(p.ws, { t: "msg", text: "You don't have room for the credits." });
    p.inv.splice(0, INV_SLOTS, ...inv);
    this.setQty(item, this.qtyOf(item) + qty);
    p.buyback = [{ item, qty, price: pay, at: Date.now() }, ...(p.buyback ?? [])].slice(0, BUYBACK_SLOTS);
    p.dirty = true;
    this.room.savePlayer(p);
    this.room.send(p.ws, { t: "inv", inv: p.inv });
    this.room.send(p.ws, { t: "msg", text: `You sell ${qty > 1 ? qty + " x " : ""}${ITEMS[item].name.toLowerCase()} for ${pay} credits.` });
    this.refresh(p);
    this.refreshOthers(p);
  }

  /** Rebuy one of your own recent sales for exactly what you were paid. */
  buyback(p: Player, msg: any) {
    this.ensure();
    if (!this.open.has(p) || !this.near(p)) return;
    const now = Date.now();
    p.buyback = (p.buyback ?? []).filter((b) => now - b.at < BUYBACK_MS);
    const idx = msg.idx;
    const b = Number.isInteger(idx) && idx >= 0 && idx < p.buyback.length ? p.buyback[idx] : null;
    if (!b) return this.room.send(p.ws, { t: "err", text: "That isn't on your buyback list any more." });
    if (count(p.inv, "credits") < b.price) return this.room.send(p.ws, { t: "msg", text: `You need ${b.price} credits to buy that back.` });
    const inv = cloneInv(p.inv);
    takeFrom(inv, "credits", b.price);
    if (!addTo(inv, b.item, b.qty)) return this.room.send(p.ws, { t: "msg", text: "You don't have enough inventory space." });
    p.inv.splice(0, INV_SLOTS, ...inv);
    // it comes back off her shelf, so the same goods are never in two places
    this.setQty(b.item, Math.max(0, this.qtyOf(b.item) - b.qty));
    p.buyback.splice(idx, 1);
    p.dirty = true;
    this.room.savePlayer(p);
    this.room.send(p.ws, { t: "inv", inv: p.inv });
    this.room.send(p.ws, { t: "msg", text: `You buy back ${b.qty > 1 ? b.qty + " x " : ""}${ITEMS[b.item].name.toLowerCase()} for ${b.price} credits.` });
    this.refresh(p);
    this.refreshOthers(p);
  }

  /** "Value": what the trader would charge for one and pay for one, right now. */
  value(p: Player, msg: any) {
    this.ensure();
    const item = typeof msg.item === "string" ? msg.item : "";
    if (!ITEMS[item]) return this.room.send(p.ws, { t: "err", text: "Unknown item." });
    const q = this.qtyOf(item);
    const sold = this.shelf.some((s) => s.item === item);
    this.room.send(p.ws, { t: "value", item, buy: sold && q > 0 ? buyPrice(item, q) : null, sell: UNSELLABLE.has(item) || !(item in VALUE) ? null : sellPrice(item, q) });
  }

  /** Each tick: restock slowly, close windows of players who wandered off or started trading. */
  tick() {
    const r = this.room;
    for (const p of [...this.open]) {
      const t = this.trader();
      if (!r.players.has(p.ws)) this.open.delete(p);
      else if (p.trade) this.close(p, "You started a trade.");
      else if (!t || cheb(p.tx, p.tz, t.tx, t.tz) > SHOP_RANGE) this.close(p, "You walked away from the stall.");
    }
    if (!this.loaded || r.tickN % RESTOCK_TICKS !== 0) return;
    let changed = false;
    for (const s of this.shelf) {
      const n = SHOP_NORMAL[s.item] ?? 0;
      if (s.qty < n) { s.qty++; changed = true; } else if (s.qty > n) { s.qty--; changed = true; }
    }
    if (!changed) return;
    this.shelf = this.shelf.filter((x) => x.qty > 0 || SHOP_NORMAL[x.item] !== undefined);
    this.dirty = true;
    for (const p of this.open) this.refresh(p);
    if (r.tickN % (RESTOCK_TICKS * 20) === 0) this.persist(); // ~10 min while busy
  }

  onLeave(p: Player) {
    this.open.delete(p);
    p.buyback = []; // buyback is for this session only
  }
}
