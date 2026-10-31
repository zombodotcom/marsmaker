//! The trader's shop for the offline world: the same rules as the server's shop.ts (prices from
//! proto.rs, a shelf that restocks toward normal, buyback at the price you were paid). Every
//! transaction is tried on a copy of the inventory first, so a refusal changes nothing.

use super::proto::*;

/// Sold items can be bought back for 30 minutes (3000 ticks).
const BUYBACK_TICKS: u64 = 3000;
const BUYBACK_SLOTS: usize = 10;

struct Sold {
    item: String,
    qty: u32,
    price: u32,
    at: u64,
}

pub struct Shelf {
    stock: Vec<(String, u32)>,
    sold: Vec<Sold>,
}

/// Add `qty` of `item` to an inventory; false (and nothing changed) if it won't fit.
pub fn add_to(inv: &mut [Option<Slot>], item: &str, qty: u32) -> bool {
    if qty == 0 {
        return true;
    }
    if stackable(item) {
        if let Some(s) = inv.iter_mut().flatten().find(|s| s.item == item) {
            s.qty += qty;
            return true;
        }
        return match inv.iter().position(|s| s.is_none()) {
            Some(i) => {
                inv[i] = Some(Slot { item: item.into(), qty });
                true
            }
            None => false,
        };
    }
    let free: Vec<usize> = inv.iter().enumerate().filter(|(_, s)| s.is_none()).map(|(i, _)| i).collect();
    if free.len() < qty as usize {
        return false;
    }
    for &i in free.iter().take(qty as usize) {
        inv[i] = Some(Slot { item: item.into(), qty: 1 });
    }
    true
}

pub fn take_from(inv: &mut [Option<Slot>], item: &str, mut qty: u32) {
    for s in inv.iter_mut() {
        if qty == 0 {
            break;
        }
        if let Some(x) = s.as_mut().filter(|x| x.item == item) {
            let t = x.qty.min(qty);
            x.qty -= t;
            qty -= t;
            if x.qty == 0 {
                *s = None;
            }
        }
    }
}

fn amount(q: Qty, max: u32) -> u32 {
    match q {
        Qty::All => max,
        Qty::N(n) => n.min(max).min(1000),
    }
}

impl Default for Shelf {
    fn default() -> Self {
        Shelf { stock: SHOP_STOCK.iter().map(|(i, n)| (i.to_string(), *n)).collect(), sold: Vec::new() }
    }
}

impl Shelf {
    fn qty_of(&self, item: &str) -> u32 {
        self.stock.iter().find(|(i, _)| i == item).map_or(0, |(_, q)| *q)
    }

    fn set_qty(&mut self, item: &str, q: u32) {
        match self.stock.iter_mut().find(|(i, _)| i == item) {
            Some(e) => e.1 = q,
            None if q > 0 => self.stock.push((item.to_string(), q)),
            None => {}
        }
        // things she doesn't normally stock leave the shelf when they run out
        self.stock.retain(|(i, q)| *q > 0 || shop_normal(i).is_some());
    }

    fn cost_to_buy(&self, item: &str, qty: u32) -> u32 {
        let mut have = self.qty_of(item);
        let mut total = 0;
        for _ in 0..qty {
            total += buy_price(item, have);
            have = have.saturating_sub(1);
        }
        total
    }

    fn pay_for_sale(&self, item: &str, qty: u32) -> u32 {
        let have = self.qty_of(item);
        (0..qty).map(|k| sell_price(item, have + k)).sum()
    }

    /// The window: shelf with prices, buyback, her offer for each item in the pack, credits.
    pub fn view(&mut self, inv: &[Option<Slot>], npc: u32, name: &str, now: u64) -> S2C {
        self.sold.retain(|s| now < s.at + BUYBACK_TICKS);
        let mut sell = std::collections::HashMap::new();
        for s in inv.iter().flatten() {
            sell.entry(s.item.clone()).or_insert_with(|| sell_price(&s.item, self.qty_of(&s.item)));
        }
        S2C::Shop {
            npc,
            name: name.to_string(),
            stock: self.stock.iter().map(|(i, q)| ShopItem { item: i.clone(), qty: *q, price: if *q > 0 { buy_price(i, *q) } else { 0 }, sell: sell_price(i, *q) }).collect(),
            buyback: self.sold.iter().map(|s| BuybackItem { item: s.item.clone(), qty: s.qty, price: s.price, expires_in: ((s.at + BUYBACK_TICKS).saturating_sub(now) as f32 * 0.6) as u32 }).collect(),
            sell,
            credits: count(inv, "credits"),
        }
    }

    /// Buy as many of shelf `idx` as you asked for, can afford and can carry. Ok = the chat line.
    pub fn buy(&mut self, inv: &mut Vec<Option<Slot>>, idx: usize, q: Qty) -> Result<String, String> {
        let Some((item, have)) = self.stock.get(idx).cloned() else { return Err("She doesn't sell that.".into()) };
        if have == 0 {
            return Err("She's out of those.".into());
        }
        let credits = count(inv, "credits");
        let mut qty = amount(q, have);
        while qty > 0 && self.cost_to_buy(&item, qty) > credits {
            qty -= 1;
        }
        if qty == 0 {
            return Err(format!("You can't afford that. ({} credits each)", buy_price(&item, have)));
        }
        if !stackable(&item) {
            let mut after = inv.clone();
            take_from(&mut after, "credits", self.cost_to_buy(&item, qty));
            qty = qty.min(after.iter().filter(|s| s.is_none()).count() as u32);
        }
        let cost = self.cost_to_buy(&item, qty);
        let mut next = inv.clone();
        take_from(&mut next, "credits", cost);
        if qty == 0 || !add_to(&mut next, &item, qty) {
            return Err("You don't have enough inventory space.".into());
        }
        *inv = next;
        self.set_qty(&item, have - qty);
        let n = if qty > 1 { format!("{qty} x ") } else { String::new() };
        Ok(format!("You buy {n}{} for {cost} credits.", item_name(&item).to_lowercase()))
    }

    pub fn sell(&mut self, inv: &mut Vec<Option<Slot>>, slot: usize, q: Qty, now: u64) -> Result<String, String> {
        let Some(item) = inv.get(slot).cloned().flatten().map(|s| s.item) else { return Err("Nothing in that slot.".into()) };
        if unsellable(&item) {
            return Err(if item == "credits" { "That's money.".into() } else { "She won't take that. It's bound to you.".into() });
        }
        let qty = amount(q, count(inv, &item)).min(SHOP_MAX.saturating_sub(self.qty_of(&item)));
        if qty == 0 {
            return Err("She can't take any more of those.".into());
        }
        let pay = self.pay_for_sale(&item, qty);
        let mut next = inv.clone();
        take_from(&mut next, &item, qty);
        if !add_to(&mut next, "credits", pay) {
            return Err("You don't have room for the credits.".into());
        }
        *inv = next;
        let have = self.qty_of(&item);
        self.set_qty(&item, have + qty);
        self.sold.insert(0, Sold { item: item.clone(), qty, price: pay, at: now });
        self.sold.truncate(BUYBACK_SLOTS);
        let n = if qty > 1 { format!("{qty} x ") } else { String::new() };
        Ok(format!("You sell {n}{} for {pay} credits.", item_name(&item).to_lowercase()))
    }

    pub fn buyback(&mut self, inv: &mut Vec<Option<Slot>>, idx: usize, now: u64) -> Result<String, String> {
        self.sold.retain(|s| now < s.at + BUYBACK_TICKS);
        let Some(s) = self.sold.get(idx) else { return Err("That isn't on your buyback list any more.".into()) };
        if count(inv, "credits") < s.price {
            return Err(format!("You need {} credits to buy that back.", s.price));
        }
        let mut next = inv.clone();
        take_from(&mut next, "credits", s.price);
        if !add_to(&mut next, &s.item, s.qty) {
            return Err("You don't have enough inventory space.".into());
        }
        *inv = next;
        let s = self.sold.remove(idx);
        let have = self.qty_of(&s.item);
        self.set_qty(&s.item, have.saturating_sub(s.qty));
        let n = if s.qty > 1 { format!("{} x ", s.qty) } else { String::new() };
        Ok(format!("You buy back {n}{} for {} credits.", item_name(&s.item).to_lowercase(), s.price))
    }

    /// "Value": her price for one (None = not on the shelf) and her offer for one (None = won't buy).
    pub fn value(&self, item: &str) -> (Option<u32>, Option<u32>) {
        let q = self.qty_of(item);
        let buy = (q > 0).then(|| buy_price(item, q));
        let sell = (!unsellable(item)).then(|| sell_price(item, q));
        (buy, sell)
    }

    /// One step back toward normal stock on every shelf. True if anything changed.
    pub fn restock(&mut self) -> bool {
        let mut changed = false;
        for (i, q) in self.stock.iter_mut() {
            let n = shop_normal(i).unwrap_or(0);
            if *q < n {
                *q += 1;
                changed = true;
            } else if *q > n {
                *q -= 1;
                changed = true;
            }
        }
        self.stock.retain(|(i, q)| *q > 0 || shop_normal(i).is_some());
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv_with(credits: u32) -> Vec<Option<Slot>> {
        let mut inv = vec![None; INV_SLOTS];
        inv[0] = Some(Slot { item: "credits".into(), qty: credits });
        inv
    }

    #[test]
    fn full_pack_takes_more_of_a_stack_only() {
        let mut inv: Vec<Option<Slot>> = (0..INV_SLOTS).map(|_| Some(Slot { item: "pistol".into(), qty: 1 })).collect();
        inv[0] = Some(Slot { item: "chitin".into(), qty: 10 });
        assert!(add_to(&mut inv, "chitin", 5));
        assert_eq!(count(&inv, "chitin"), 15);
        let before = inv.clone();
        assert!(!add_to(&mut inv, "void_crystal", 1), "a new stack needs a free slot");
        assert!(!add_to(&mut inv, "rifle", 1), "gear needs a free slot");
        assert_eq!(inv, before, "a refusal changes nothing");
        take_from(&mut inv, "chitin", 15);
        assert!(inv[0].is_none(), "an emptied stack frees its slot");
        assert!(add_to(&mut inv, "void_crystal", 40) && count(&inv, "void_crystal") == 40);
        assert_eq!(inv.iter().filter(|s| s.as_ref().is_some_and(|s| s.item == "void_crystal")).count(), 1);
    }

    #[test]
    fn no_profit_loop() {
        // buying then selling back, or selling then buying, never makes credits, at any stock level
        for &(item, _) in SHOP_STOCK {
            for q in 1..60 {
                assert!(sell_price(item, q) < buy_price(item, q.max(1)), "{item} at {q}");
                assert!(sell_price(item, q.saturating_sub(1)) < buy_price(item, q), "{item} at {q}");
            }
        }
    }

    #[test]
    fn buy_sell_buyback() {
        let mut s = Shelf::default();
        let mut inv = inv_with(1000);
        let idx = s.stock.iter().position(|(i, _)| i == "medkit").unwrap();
        let price = buy_price("medkit", 10);
        s.buy(&mut inv, idx, Qty::N(1)).unwrap();
        assert_eq!(count(&inv, "credits"), 1000 - price);
        assert_eq!(count(&inv, "medkit"), 1);
        let slot = inv.iter().position(|x| x.as_ref().is_some_and(|x| x.item == "medkit")).unwrap();
        s.sell(&mut inv, slot, Qty::N(1), 0).unwrap();
        let paid = sell_price("medkit", 9);
        assert_eq!(count(&inv, "credits"), 1000 - price + paid);
        s.buyback(&mut inv, 0, 1).unwrap();
        assert_eq!(count(&inv, "credits"), 1000 - price);
        assert_eq!(count(&inv, "medkit"), 1);
        assert_eq!(s.qty_of("medkit"), 9);
    }

    #[test]
    fn refusals_change_nothing() {
        let mut s = Shelf::default();
        let mut inv = inv_with(5);
        let lamp = s.stock.iter().position(|(i, _)| i == "xp_lamp").unwrap();
        assert!(s.buy(&mut inv, lamp, Qty::N(1)).is_err());
        assert_eq!(count(&inv, "credits"), 5);
        inv[1] = Some(Slot { item: "rift_blade".into(), qty: 1 });
        assert!(s.sell(&mut inv, 1, Qty::N(1), 0).is_err());
        assert_eq!(count(&inv, "rift_blade"), 1);
    }
}
