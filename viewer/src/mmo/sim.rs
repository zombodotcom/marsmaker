//! MarsScape offline world: the server's rules run in-process, so the mode is playable solo (and
//! on GitHub Pages) with no server. Speaks the same messages as the network (proto.rs): the
//! client sends `C2S` through `handle` and receives `S2C` from `update`, exactly as online.
//! Colonist bots live here too: a quartermaster with a starter gift, a guard who joins your fights
//! (say "help" or "follow me" for an escort, "stop" to release it), a medic, a miner and a surveyor.
//! They hand out the same quests as on the server. Also offline: the colony locker, armour, Insight
//! Lamps, the lava tube under the site (the same generator as the server, cave.rs) and Rift Surges.
//! Trading needs another player, so it's online only.

use super::cave;
use super::grid::WalkGrid;
use super::proto::*;
use bevy::math::IVec2;
use std::collections::{HashMap, VecDeque};

const ME: u32 = 1;

#[derive(Clone, Copy)]
enum Task {
    None,
    Gather(u32),
    Craft(&'static Recipe, u32),
    Attack(u32),
    /// a locker / pit / ladder, used when the walk ends next to it
    Use(u32),
    /// walk up to a colonist, then talk
    Talk(u32),
}

struct P {
    id: u32,
    name: String,
    tile: IVec2,
    path: VecDeque<IVec2>,
    run: bool,
    task: Task,
    timer: u32,
    skills: Skills,
    inv: Vec<Option<Slot>>,
    weapon: Option<String>,
    hp: i32,
    cd: u32,
    anim: u8,
    sent: Option<(IVec2, u8)>,
    bot: Option<Bot>,
    gear: Gear,
}

struct Bot {
    role: &'static str,
    next_chat: u64,
    idle: u32,
    /// guard: escorting the player until this tick
    follow_until: u64,
    /// medic: next heal allowed at this tick
    heal_at: u64,
}

struct N {
    id: u32,
    kind: &'static str,
    tile: IVec2,
    up: bool,
    respawn_at: u64,
}

struct M {
    id: u32,
    def: &'static CreatureDef,
    home: IVec2,
    tile: IVec2,
    hp: i32,
    target: Option<u32>,
    cd: u32,
    dead_until: Option<u64>,
    anim: u8,
    sent: Option<(IVec2, u8, i32)>,
    /// a Rift Surge creature: removed when it dies instead of respawning
    temp: bool,
}

/// The surface while you're down the lava tube.
struct Stash {
    players: Vec<P>,
    nodes: Vec<N>,
    npcs: Vec<M>,
    spawn: IVec2,
}

/// Rift Surge state (as the server: warning, three waves, the boss, loot).
#[derive(Default)]
struct Surge {
    /// 0 idle, 1 warning, 2 open, 3 boss
    phase: u8,
    next: u64,
    until: u64,
    wave: usize,
    spot: IVec2,
    boss: Option<u32>,
    /// damage you did to the boss
    dmg: i32,
    count: u32,
}

const SURGE_WARN: u64 = 200;
const SURGE_WAVE_GAP: u64 = 20;
const SURGE_BOSS: u64 = 600;
const SURGE_WAVES: [&[(&str, usize)]; 3] = [&[("skitter", 4)], &[("skitter", 2), ("stalker", 2)], &[("stalker", 2), ("golem", 1)]];

pub struct Sim {
    tick_ms: u32,
    n: u64,
    acc: f32,
    spawn: IVec2,
    players: Vec<P>,
    nodes: Vec<N>,
    npcs: Vec<M>,
    rng: u64,
    out: Vec<S2C>,
    joined: bool,
    /// quartermaster's greeting (tick) and one-time starter gift
    greet_at: Option<u64>,
    gift_given: bool,
    site: String,
    bank: Vec<Slot>,
    quests: HashMap<String, QuestSt>,
    /// open dialog: colonist id, offered quest, option ids
    dialog: Option<(u32, Option<&'static str>, Vec<String>)>,
    stash: Option<Stash>,
    /// down the lava tube: the surface is stashed
    in_cave: bool,
    /// died down there: come up at the landing site, not the pit
    up_at_spawn: bool,
    /// don't tick until the client has the welcome for a new room
    hold: bool,
    surge: Surge,
    next_temp: u32,
}

/// Colonists: name, role.
const COLONISTS: [(&str, &str); 5] = [("Quinn", "quartermaster"), ("Rix", "guard"), ("Doc Hollis", "medic"), ("Dusty Rae", "miner"), ("Valles Val", "surveyor")];

const LORE: [&str; 9] = [
    "The old Martians didn't build the seal to keep us out. They built it to keep something in.",
    "Every rift that opens, the stars inside look a little closer.",
    "Found the spiral glyph on an outcrop again. Same warning as the last one.",
    "Void crystals hum at night. Don't sleep next to your pack.",
    "They say the deepest rift is under Olympus Mons. Nobody's been back to check.",
    "The leviathans don't eat. They just watch.",
    "Before the rifts, the worst thing out here was the dust.",
    "These dunes are real, you know. Orbital survey, down to the metre.",
    "Mine by day, shoot by night. Colony life.",
];

fn role_line(role: &str, k: usize) -> &'static str {
    let lines: &[&str] = match role {
        "guard" => &["Rifts are quiet today. Too quiet.", "Need an escort? Say 'help' and I'll come running.", "Skitters first. Work up to the stalkers."],
        "medic" => &["Low on hitpoints? Come find me.", "Medkits: a water flask and a core sample, at the fabricator.", "Hitpoints come back slowly on their own. Medkits are faster."],
        "quartermaster" => &["Bricks, plates, blasters - the fabricator does it all if you bring the stuff.", "Two iron plates make a blaster. Plates come from hematite.", "Void crystals stack. Everything else wants its own slot."],
        "miner" => &["Ice opens up at Mining 15, hematite at 30. Keep swinging.", "A drill bit in your pack makes mining easier.", "Regolith's boring, but bricks don't make themselves."],
        _ => &["Outcrops give core samples. You'll want them for medkits and drill bits.", "The layers in these outcrops are older than any rift.", "Survey's quiet work. I like quiet."],
    };
    lines[k % lines.len()]
}

fn new_inv() -> Vec<Option<Slot>> {
    vec![None; INV_SLOTS]
}

impl Sim {
    pub fn new(grid: &mut WalkGrid, site_name: &str, spawn: IVec2) -> Self {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for b in site_name.bytes() {
            seed = (seed ^ b as u64).wrapping_mul(0x100_0000_01B3);
        }
        let mut s = Sim {
            tick_ms: 600,
            n: 0,
            acc: 0.0,
            spawn,
            players: Vec::new(),
            nodes: Vec::new(),
            npcs: Vec::new(),
            rng: seed | 1,
            out: Vec::new(),
            joined: false,
            greet_at: None,
            gift_given: false,
            site: site_name.to_string(),
            bank: Vec::new(),
            quests: HashMap::new(),
            dialog: None,
            stash: None,
            in_cave: false,
            up_at_spawn: false,
            hold: false,
            surge: Surge::default(),
            next_temp: 1000,
        };
        s.place_world(grid);
        s
    }

    fn rand(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    fn rand_i(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.rand() * (hi - lo + 1) as f64) as i32
    }

    fn place_world(&mut self, grid: &mut WalkGrid) {
        // the fabricator (node 0) a few tiles from spawn, as on the server
        let fab = grid.nearest_walkable(self.spawn + IVec2::new(3, 3), 4).unwrap_or(self.spawn + IVec2::new(3, 3));
        let mut taken: Vec<IVec2> = vec![self.spawn, fab];
        // the colony locker two tiles from the fabricator
        let locker = grid.nearest_walkable(fab + IVec2::new(-2, 0), 3).filter(|t| *t != self.spawn && *t != fab);
        taken.extend(locker);
        let mut place = |s: &mut Sim, grid: &mut WalkGrid, rmin: i32, rmax: i32| -> Option<IVec2> {
            for _ in 0..200 {
                let a = s.rand() * std::f64::consts::TAU;
                let r = s.rand_i(rmin, rmax) as f64;
                let t = s.spawn + IVec2::new((a.cos() * r).round() as i32, (a.sin() * r).round() as i32);
                if grid.ground_ok(t) && taken.iter().all(|o| chebyshev((o.x, o.y), (t.x, t.y)) > 2) {
                    taken.push(t);
                    return Some(t);
                }
            }
            None
        };
        let mut nodes: Vec<(&'static str, IVec2)> = Vec::new();
        nodes.push(("fabricator", fab));
        let layout: [(&'static str, usize, i32, i32); 4] = [("regolith", 10, 4, 22), ("outcrop", 6, 6, 26), ("ice", 8, 14, 40), ("hematite", 6, 26, 56)];
        for (kind, k, a, b) in layout {
            for _ in 0..k {
                if let Some(t) = place(self, grid, a, b) {
                    nodes.push((kind, t));
                }
            }
        }
        // rifts: two of skitters 25-40 tiles out, stalkers further, golems (and the leviathan) furthest
        let rifts: [(&'static [(&'static str, usize)], i32, i32); 4] = [(&[("skitter", 4)], 25, 40), (&[("skitter", 4)], 25, 40), (&[("stalker", 3)], 44, 54), (&[("golem", 2), ("leviathan", 1)], 56, 62)];
        let mut mid = 0;
        for (who, a, b) in rifts {
            let Some(t) = place(self, grid, a, b) else { continue };
            nodes.push(("rift", t));
            for (kind, per) in who {
                let def = creature_def(kind).unwrap();
                for j in 0..*per {
                    mid += 1;
                    let home = grid.nearest_walkable(t + IVec2::new(j as i32 - 1, 1), 3).unwrap_or(t);
                    self.npcs.push(M { id: mid, def, home, tile: home, hp: def.hp, target: None, cd: 0, dead_until: None, anim: anim::IDLE, sent: None, temp: false });
                }
            }
        }
        // the pit down into the lava tube, further out
        if let Some(t) = locker {
            nodes.push(("locker", t));
        }
        if let Some(t) = place(self, grid, 10, 30) {
            nodes.push(("pit", t));
        }
        for (i, (kind, t)) in nodes.into_iter().enumerate() {
            if kind != "rift" {
                grid.blocked.insert(t);
            }
            self.nodes.push(N { id: i as u32, kind, tile: t, up: true, respawn_at: 0 });
        }
    }

    fn spawn_bots(&mut self, grid: &mut WalkGrid) {
        for (i, (name, role)) in COLONISTS.iter().enumerate() {
            let off = IVec2::new(self.rand_i(-5, 5), self.rand_i(-5, 5));
            let tile = grid.nearest_walkable(self.spawn + off, 6).unwrap_or(self.spawn);
            let mut skills = Skills::fresh();
            skills.mining = xp_for_level(if *role == "miner" { 45 } else { self.rand_i(5, 25) as u32 });
            skills.survey = xp_for_level(if *role == "surveyor" { 40 } else { self.rand_i(1, 20) as u32 });
            let fighter = *role == "guard";
            skills.ranged = xp_for_level(if fighter { 55 } else { self.rand_i(10, 30) as u32 });
            skills.defence = xp_for_level(if fighter { 50 } else { self.rand_i(10, 30) as u32 });
            skills.hitpoints = xp_for_level(if fighter { 55 } else { self.rand_i(15, 30) as u32 });
            let hp = skills.level(Skill::Hitpoints) as i32;
            let next_chat = 40 + self.rand_i(0, 80) as u64 + i as u64 * 25;
            self.players.push(P {
                id: 100 + i as u32,
                name: name.to_string(),
                tile,
                path: VecDeque::new(),
                run: false,
                task: Task::None,
                timer: 0,
                skills,
                inv: new_inv(),
                weapon: Some(if fighter { "rifle" } else { "pistol" }.into()),
                hp,
                cd: 0,
                anim: anim::IDLE,
                sent: None,
                bot: Some(Bot { role, next_chat, idle: 0, follow_until: 0, heal_at: 0 }),
                gear: Gear::default(),
            });
        }
    }

    fn bot_index(&self, role: &str) -> Vec<usize> {
        (0..self.players.len()).filter(|&i| self.players[i].bot.as_ref().is_some_and(|b| b.role == role)).collect()
    }

    fn me_index(&self) -> Option<usize> {
        self.players.iter().position(|p| p.id == ME)
    }

    fn say_as(&mut self, pi: usize, msg: impl Into<String>) {
        let p = &self.players[pi];
        self.out.push(S2C::Chat { id: p.id, name: p.name.clone(), msg: msg.into() });
    }

    /// Colonist reactions to the player's chat: "help" / "follow me" calls the nearest guard as an
    /// escort for 2 minutes, "stop" releases it.
    fn colonist_chat(&mut self, msg: &str) {
        let m = msg.to_lowercase();
        let Some(me) = self.me_index() else { return };
        let at = self.players[me].tile;
        let guards = self.bot_index("guard");
        if m.contains("help") || m.contains("follow me") {
            let Some(&g) = guards.iter().min_by_key(|&&g| chebyshev((self.players[g].tile.x, self.players[g].tile.y), (at.x, at.y))) else { return };
            self.players[g].bot.as_mut().unwrap().follow_until = self.n + 200;
            self.players[g].run = true;
            let name = self.players[me].name.clone();
            self.say_as(g, format!("On my way, {name}. I've got your back for the next two minutes."));
        } else if m.split_whitespace().any(|w| w == "stop") {
            for g in guards {
                let b = self.players[g].bot.as_mut().unwrap();
                if b.follow_until > self.n {
                    b.follow_until = 0;
                    self.players[g].task = Task::None;
                    self.players[g].path.clear();
                    self.say_as(g, "Standing down. Holler if you need me.");
                }
            }
        }
    }

    /// Greeting, starter gift, medic heals: things the colonists do for the local player.
    fn colonists_help(&mut self) {
        let Some(me) = self.me_index() else { return };
        let n = self.n;
        let at = self.players[me].tile;
        let near = |p: &P, r: i32| chebyshev((p.tile.x, p.tile.y), (at.x, at.y)) <= r;
        if let Some(&q) = self.bot_index("quartermaster").first() {
            if self.greet_at.is_some_and(|t| n >= t) {
                self.greet_at = None;
                let name = self.players[me].name.clone();
                self.say_as(q, format!("Welcome to the colony, {name}! Tip: left-click a regolith mound to mine it, then turn two regolith into a brick at the fabricator."));
            }
            if !self.gift_given && near(&self.players[q], 6) && n > 6 {
                self.gift_given = true;
                let mut got = 0;
                for item in ["brick", "brick", "brick", "medkit"] {
                    got += self.give(me, item) as u32;
                }
                if got > 0 {
                    self.say_as(q, "Here - three bricks and a medkit to get you started. Don't waste the medkit.");
                    let inv = self.players[me].inv.clone();
                    self.out.push(S2C::Inv { inv });
                    self.out.push(S2C::Msg { text: "Quinn the quartermaster hands you 3 regolith bricks and a medkit.".into() });
                }
            }
        }
        let max = self.players[me].skills.level(Skill::Hitpoints) as i32;
        if self.players[me].hp * 2 < max {
            for m in self.bot_index("medic") {
                let b = self.players[m].bot.as_ref().unwrap();
                if n < b.heal_at || !near(&self.players[m], 2) {
                    continue;
                }
                self.players[m].bot.as_mut().unwrap().heal_at = n + 8;
                self.players[m].anim = anim::CRAFT;
                let p = &mut self.players[me];
                p.hp = (p.hp + 5).min(max);
                let hp = p.hp;
                self.say_as(m, "Hold still... there. Patched you up.");
                self.out.push(S2C::Heal { src: Who::P(self.players[m].id).fmt(), dst: Who::P(ME).fmt(), amt: 5 });
                self.out.push(S2C::Hp { hp, max });
                self.out.push(S2C::Msg { text: "Doc Hollis heals you (+5 hitpoints).".into() });
            }
        }
    }

    fn me(&mut self) -> Option<&mut P> {
        self.players.iter_mut().find(|p| p.id == ME)
    }

    fn msg(&mut self, pid: u32, text: impl Into<String>) {
        if pid == ME {
            self.out.push(S2C::Msg { text: text.into() });
        }
    }

    /// A message from the local client.
    pub fn handle(&mut self, m: C2S, grid: &mut WalkGrid) {
        match m {
            C2S::Hello { name, .. } => {
                if !self.joined {
                    self.joined = true;
                    self.spawn_bots(grid);
                    self.greet_at = Some(self.n + 4);
                }
                self.players.retain(|p| p.id != ME);
                let tile = grid.nearest_walkable(self.spawn, 8).unwrap_or(self.spawn);
                let skills = Skills::fresh();
                let hp = skills.level(Skill::Hitpoints) as i32;
                let mut inv = new_inv();
                inv[0] = Some(Slot { item: "pistol".into(), qty: 1 });
                self.players.insert(0, P { id: ME, name: name.clone(), tile, path: VecDeque::new(), run: false, task: Task::None, timer: 0, skills, inv, weapon: None, hp, cd: 0, anim: anim::IDLE, sent: None, bot: None, gear: Gear::default() });
                let w = self.welcome();
                self.out.push(w);
                self.out.push(S2C::Msg { text: format!("Welcome to MarsScape, {name}. You're offline: this world runs on your machine.") });
                if self.surge.next == 0 {
                    // the first Rift Surge comes sooner offline
                    self.surge.next = self.n + 1200 + self.rand_i(0, 600) as u64;
                }
            }
            C2S::Walk { path, run } => {
                let Some(p) = self.me() else { return };
                p.path = path.into_iter().map(|[x, z]| IVec2::new(x, z)).take(MAX_PATH).collect();
                p.run = run;
                p.task = Task::None;
                p.timer = 0;
            }
            C2S::Act { node } => self.act(ME, node),
            C2S::Craft { recipe: r } => {
                let Some(r) = recipe(&r) else { return };
                self.start_craft(ME, r);
            }
            C2S::Drop { slot } => {
                let Some(p) = self.me() else { return };
                if slot < INV_SLOTS && p.inv[slot].take().is_some() {
                    let inv = p.inv.clone();
                    self.out.push(S2C::Inv { inv });
                }
            }
            C2S::Chat { msg } => {
                let msg: String = msg.chars().filter(|c| !c.is_control()).take(CHAT_MAX).collect();
                if msg.trim().is_empty() {
                    return;
                }
                let Some(p) = self.me() else { return };
                let name = p.name.clone();
                self.out.push(S2C::Chat { id: ME, name, msg: msg.clone() });
                self.colonist_chat(&msg);
            }
            C2S::Equip { slot } => self.equip(ME, slot),
            C2S::Unequip { gear: Some(g) } if g != "weapon" => self.take_off(&g),
            C2S::Unequip { .. } => {
                let Some(p) = self.me() else { return };
                let Some(w) = p.weapon.clone() else { return };
                let Some(free) = p.inv.iter().position(|s| s.is_none()) else {
                    self.msg(ME, "You don't have enough inventory space.");
                    return;
                };
                p.inv[free] = Some(Slot { item: w, qty: 1 });
                p.weapon = None;
                let inv = p.inv.clone();
                self.out.push(S2C::Equip { weapon: None });
                self.out.push(S2C::Inv { inv });
                self.send_gear();
            }
            C2S::Attack { npc } => {
                if self.npcs.iter().any(|m| m.id == npc && m.dead_until.is_none()) {
                    if let Some(p) = self.me() {
                        p.task = Task::Attack(npc);
                    }
                }
            }
            C2S::Eat { slot } => {
                let Some(p) = self.me() else { return };
                if p.inv.get(slot).and_then(|s| s.as_ref()).is_some_and(|s| s.item == "medkit") {
                    p.inv[slot] = None;
                    let max = p.skills.level(Skill::Hitpoints) as i32;
                    p.hp = (p.hp + MEDKIT_HEAL).min(max);
                    p.cd += MEDKIT_DELAY;
                    let (hp, inv) = (p.hp, p.inv.clone());
                    self.out.push(S2C::Hp { hp, max });
                    self.out.push(S2C::Inv { inv });
                    self.msg(ME, "You patch yourself up with the medkit.");
                }
            }
            C2S::Ping => {}
            C2S::BankDeposit { slot, qty } => self.bank_deposit(slot, qty),
            C2S::BankWithdraw { idx, qty } => self.bank_withdraw(idx, qty),
            C2S::TradeRequest { .. } | C2S::TradeOffer { .. } | C2S::TradeRemove { .. } | C2S::TradeAccept | C2S::TradeDecline => {
                self.msg(ME, "Trading needs another player: it works on the online server.");
            }
            C2S::Talk { id } => {
                if self.players.iter().any(|p| p.id == id && p.bot.is_some()) {
                    if let Some(p) = self.me() {
                        p.task = Task::Talk(id);
                        p.path.clear();
                    }
                    self.dialog = None;
                }
            }
            C2S::DialogChoose { option } => self.choose(&option),
            C2S::Rub { slot, skill } => self.rub(slot, &skill),
        }
    }

    fn act(&mut self, pid: u32, node: u32) {
        let Some(n) = self.nodes.iter().find(|n| n.id == node) else { return };
        let kind = n.kind;
        let Some(p) = self.players.iter_mut().find(|p| p.id == pid) else { return };
        match kind {
            "fabricator" => {}
            "rift" => self.msg(pid, "A tear into the void. Stars where the ground should be, and things crawling out."),
            k if usable_node(k) => p.task = Task::Use(node),
            _ => {
                p.task = Task::Gather(node);
                p.timer = 0;
            }
        }
    }

    fn start_craft(&mut self, pid: u32, r: &'static Recipe) {
        let fabs: Vec<IVec2> = self.nodes.iter().filter(|n| n.kind == "fabricator").map(|n| n.tile).collect();
        let Some(p) = self.players.iter_mut().find(|p| p.id == pid) else { return };
        if !fabs.iter().any(|f| chebyshev((f.x, f.y), (p.tile.x, p.tile.y)) <= FAB_RANGE) {
            self.msg(pid, "You need to be next to a fabricator.");
            return;
        }
        if r.quest.is_some_and(|q| self.quests.get(q).is_none_or(|s| s.s != "done")) {
            self.msg(pid, "You don't know how to make that yet. Maybe a colonist can teach you.");
            return;
        }
        let Some(p) = self.players.iter_mut().find(|p| p.id == pid) else { return };
        if p.skills.level(Skill::Fabrication) < r.level {
            self.msg(pid, format!("You need a Fabrication level of {} to make that.", r.level));
            return;
        }
        if !r.inputs.iter().all(|(it, q)| count(&p.inv, it) >= *q) {
            self.msg(pid, "You don't have the materials for that.");
            return;
        }
        p.path.clear();
        p.task = Task::Craft(r, r.ticks);
    }

    fn equip(&mut self, pid: u32, slot: usize) {
        let Some(p) = self.players.iter_mut().find(|p| p.id == pid) else { return };
        let Some(item) = p.inv.get(slot).and_then(|s| s.as_ref()).map(|s| s.item.clone()) else { return };
        if let Some(a) = armour(&item) {
            if p.skills.level(Skill::Defence) < a.level {
                self.msg(pid, format!("You need a Defence level of {} to wear that.", a.level));
                return;
            }
            let old = p.gear.slot_mut(a.slot).and_then(|g| g.replace(item.clone()));
            p.inv[slot] = old.map(|o| Slot { item: o, qty: 1 });
            if pid == ME {
                let inv = p.inv.clone();
                self.out.push(S2C::Inv { inv });
                self.send_gear();
            }
            return;
        }
        let w = weapon(Some(&item));
        if w.item.is_empty() {
            self.msg(pid, "You can't equip that.");
            return;
        }
        if p.skills.level(Skill::Ranged) < w.level {
            self.msg(pid, format!("You need a Ranged level of {} to use that.", w.level));
            return;
        }
        p.inv[slot] = p.weapon.take().map(|o| Slot { item: o, qty: 1 });
        p.weapon = Some(item.clone());
        if pid == ME {
            let inv = p.inv.clone();
            self.out.push(S2C::Equip { weapon: Some(item) });
            self.out.push(S2C::Inv { inv });
            self.send_gear();
        }
    }

    /// Test hook (`--mmo-script give:ITEM`): put an item in the local player's inventory.
    pub fn cheat_give(&mut self, item: &str) -> Vec<S2C> {
        if let Some(pi) = self.players.iter().position(|p| p.id == ME) {
            self.give(pi, item);
            let inv = self.players[pi].inv.clone();
            return vec![S2C::Inv { inv }];
        }
        Vec::new()
    }

    /// Test hook (`--mmo-script xp:SKILL:XP`): set a skill's XP.
    pub fn cheat_xp(&mut self, skill: &str, xp: f64) -> Vec<S2C> {
        let (Some(pi), Some(s)) = (self.players.iter().position(|p| p.id == ME), Skill::parse(skill)) else { return Vec::new() };
        let p = &mut self.players[pi];
        p.skills.set(s, xp);
        let mut v = vec![S2C::Xp { skill: s.key().into(), xp, gain: 0.0 }];
        if s == Skill::Hitpoints {
            let max = p.skills.level(s) as i32;
            p.hp = max;
            v.push(S2C::Hp { hp: max, max });
        }
        v
    }

    /// Advance the world by `dt` seconds; returns everything the local client should receive.
    pub fn update(&mut self, dt: f32, grid: &mut WalkGrid) -> Vec<S2C> {
        if std::mem::take(&mut self.hold) {
            return std::mem::take(&mut self.out);
        }
        if self.joined {
            self.acc += dt;
            let tick = self.tick_ms as f32 / 1000.0;
            let mut k = 0;
            while self.acc >= tick && k < 5 {
                self.acc -= tick;
                self.tick(grid);
                k += 1;
            }
            if k == 5 {
                self.acc = 0.0;
            }
        }
        std::mem::take(&mut self.out)
    }

    fn give(&mut self, pi: usize, item: &str) -> bool {
        let p = &mut self.players[pi];
        if stacks(item) {
            if let Some(s) = p.inv.iter_mut().flatten().find(|s| s.item == item) {
                s.qty += 1;
                return true;
            }
        }
        match p.inv.iter().position(|s| s.is_none()) {
            Some(i) => {
                p.inv[i] = Some(Slot { item: item.into(), qty: 1 });
                true
            }
            None => false,
        }
    }

    fn add_xp(&mut self, pi: usize, skill: Skill, gain: f64) {
        let p = &mut self.players[pi];
        let before = p.skills.level(skill);
        let xp = (p.skills.get(skill) + gain).min(MAX_XP);
        p.skills.set(skill, xp);
        let after = p.skills.level(skill);
        if p.id == ME {
            self.out.push(S2C::Xp { skill: skill.key().into(), xp, gain });
            if after > before {
                self.out.push(S2C::Msg { text: format!("Your {} level is now {after}!", skill.key()) });
                self.out.push(S2C::Level { skill: skill.key().into(), level: after });
                if skill == Skill::Hitpoints {
                    let (hp, max) = (self.players[pi].hp, after as i32);
                    self.out.push(S2C::Hp { hp, max });
                }
            }
        }
    }

    fn tick(&mut self, grid: &mut WalkGrid) {
        self.n += 1;
        let n = self.n;
        for i in 0..self.nodes.len() {
            if !self.nodes[i].up && n >= self.nodes[i].respawn_at {
                self.nodes[i].up = true;
                self.out.push(S2C::Node { id: self.nodes[i].id, up: true });
            }
        }
        for pi in 0..self.players.len() {
            if self.players[pi].bot.is_some() {
                self.bot_brain(pi, grid);
            }
            self.player_tick(pi, grid);
        }
        self.colonists_help();
        self.npc_tick(grid);
        if !self.in_cave {
            self.surge_tick(grid);
        }
        if n % HP_REGEN_TICKS == 0 {
            for p in self.players.iter_mut() {
                let max = p.skills.level(Skill::Hitpoints) as i32;
                if p.hp < max {
                    p.hp += 1;
                    if p.id == ME {
                        self.out.push(S2C::Hp { hp: p.hp, max });
                    }
                }
            }
        }
        let full = n % 10 == 0;
        let mut pv = Vec::new();
        for p in self.players.iter_mut() {
            let now = (p.tile, p.anim);
            if full || p.sent != Some(now) {
                p.sent = Some(now);
                pv.push((p.id, p.tile.x, p.tile.y, p.anim));
            }
        }
        let mut mv = Vec::new();
        for m in self.npcs.iter_mut() {
            let now = (m.tile, m.anim, m.hp);
            if full || m.sent != Some(now) {
                m.sent = Some(now);
                mv.push((m.id, m.tile.x, m.tile.y, m.anim, m.hp));
            }
        }
        self.out.push(S2C::Tick { n, p: pv, m: mv });
    }

    fn player_tick(&mut self, pi: usize, grid: &mut WalkGrid) {
        // attacking: move into range, then fire
        if let Task::Attack(mid) = self.players[pi].task {
            let Some(mi) = self.npcs.iter().position(|m| m.id == mid && m.dead_until.is_none()) else {
                self.players[pi].task = Task::None;
                return self.player_tick(pi, grid);
            };
            let w = weapon(self.players[pi].weapon.as_deref());
            let mt = self.npcs[mi].tile;
            let p = &mut self.players[pi];
            let d = chebyshev((p.tile.x, p.tile.y), (mt.x, mt.y));
            if d > w.range {
                if p.path.is_empty() {
                    let range = w.range;
                    p.path = grid.find_path(p.tile, mt, |t| chebyshev((t.x, t.y), (mt.x, mt.y)) <= range).into_iter().collect();
                    if p.path.is_empty() {
                        p.task = Task::None;
                        self.msg(self.players[pi].id, "You can't reach that.");
                        return;
                    }
                }
            } else {
                p.path.clear();
            }
        }
        // movement
        let p = &mut self.players[pi];
        let steps = if p.run { 2 } else { 1 };
        let mut moved = 0;
        for _ in 0..steps {
            let Some(next) = p.path.front().copied() else { break };
            let d = next - p.tile;
            if d == IVec2::ZERO {
                p.path.pop_front();
                continue;
            }
            if d.x.abs() > 1 || d.y.abs() > 1 || !grid.walkable(next) {
                p.path.clear();
                break;
            }
            p.tile = next;
            p.path.pop_front();
            moved += 1;
        }
        p.anim = match moved {
            0 => anim::IDLE,
            1 => anim::WALK,
            _ => anim::RUN,
        };
        if p.cd > 0 {
            p.cd -= 1;
        }
        let pid = p.id;
        match p.task {
            Task::None => {}
            Task::Use(nid) => {
                if !p.path.is_empty() {
                    return;
                }
                p.task = Task::None;
                let Some(n) = self.nodes.iter().find(|n| n.id == nid) else { return };
                let (nt, kind) = (n.tile, n.kind);
                let p = &self.players[pi];
                if chebyshev((p.tile.x, p.tile.y), (nt.x, nt.y)) > 1 {
                    self.msg(pid, "You need to stand next to that.");
                    return;
                }
                if pid != ME {
                    return;
                }
                match kind {
                    "locker" => {
                        let items = self.bank.clone();
                        self.out.push(S2C::Bank { items, slots: 100 });
                    }
                    "pit" => {
                        let room = format!("{}~cave", self.site);
                        self.out.push(S2C::Travel { room });
                    }
                    "ladder" => {
                        let room = self.site.clone();
                        self.out.push(S2C::Travel { room });
                    }
                    _ => {}
                }
            }
            Task::Talk(cid) => {
                let Some(ci) = self.players.iter().position(|q| q.id == cid) else {
                    self.players[pi].task = Task::None;
                    return;
                };
                let (ct, mt) = (self.players[ci].tile, self.players[pi].tile);
                if chebyshev((ct.x, ct.y), (mt.x, mt.y)) > 2 {
                    if self.players[pi].path.is_empty() {
                        let path = grid.find_path(mt, ct, |t| chebyshev((t.x, t.y), (ct.x, ct.y)) <= 2);
                        if path.is_empty() {
                            self.players[pi].task = Task::None;
                            return;
                        }
                        self.players[pi].path = path.into_iter().collect();
                    }
                    return;
                }
                self.players[pi].task = Task::None;
                self.players[pi].path.clear();
                self.talk(ci);
            }
            Task::Gather(nid) => {
                if !p.path.is_empty() {
                    return;
                }
                let Some(ni) = self.nodes.iter().position(|n| n.id == nid) else { return };
                let (nt, up, kind) = (self.nodes[ni].tile, self.nodes[ni].up, self.nodes[ni].kind);
                let Some(def) = node_def(kind) else { return };
                let p = &mut self.players[pi];
                if chebyshev((p.tile.x, p.tile.y), (nt.x, nt.y)) != 1 {
                    p.task = Task::None;
                    self.msg(pid, "You can't reach that.");
                    return;
                }
                if !up {
                    p.task = Task::None;
                    self.msg(pid, format!("There's nothing left to {} here.", def.verb.to_lowercase()));
                    return;
                }
                if p.skills.level(def.skill) < def.level {
                    p.task = Task::None;
                    self.msg(pid, format!("You need a {} level of {} to {} this.", def.skill.label(), def.level, def.verb.to_lowercase()));
                    return;
                }
                if p.inv.iter().all(|s| s.is_some()) {
                    p.task = Task::None;
                    self.msg(pid, "Your inventory is too full to hold any more.");
                    return;
                }
                p.anim = if def.skill == Skill::Survey { anim::SURVEY } else { anim::MINE };
                p.timer += 1;
                if p.timer % GATHER_TICKS != 0 {
                    return;
                }
                let lvl = p.skills.level(def.skill);
                let drill = def.skill == Skill::Mining && count(&p.inv, "drill_bit") > 0;
                if self.rand() < gather_chance(lvl, def.level, drill) {
                    self.give(pi, def.item);
                    if pid == ME {
                        let inv = self.players[pi].inv.clone();
                        self.out.push(S2C::Inv { inv });
                        self.msg(pid, format!("You get some {}.", item_name(def.item).to_lowercase()));
                        self.quest_event("gather", def.item);
                    }
                    self.add_xp(pi, def.skill, def.xp);
                    if self.rand() < def.deplete {
                        self.nodes[ni].up = false;
                        self.nodes[ni].respawn_at = self.n + def.respawn_ticks as u64;
                        self.out.push(S2C::Node { id: nid, up: false });
                        self.players[pi].task = Task::None;
                        self.players[pi].anim = anim::IDLE;
                    }
                }
            }
            Task::Craft(r, left) => {
                p.anim = anim::CRAFT;
                if left > 1 {
                    p.task = Task::Craft(r, left - 1);
                    return;
                }
                if !r.inputs.iter().all(|(it, q)| count(&p.inv, it) >= *q) {
                    p.task = Task::None;
                    return;
                }
                for (it, q) in r.inputs {
                    let mut left = *q;
                    for s in p.inv.iter_mut() {
                        if left == 0 {
                            break;
                        }
                        if let Some(x) = s.as_mut().filter(|x| x.item == *it) {
                            let take = x.qty.min(left);
                            x.qty -= take;
                            left -= take;
                            if x.qty == 0 {
                                *s = None;
                            }
                        }
                    }
                }
                self.give(pi, r.id);
                if pid == ME {
                    let inv = self.players[pi].inv.clone();
                    self.out.push(S2C::Inv { inv });
                    self.msg(pid, format!("You make: {}.", item_name(r.id)));
                    self.quest_event("craft", r.id);
                }
                self.add_xp(pi, Skill::Fabrication, r.xp);
                let p = &mut self.players[pi];
                // make-all: keep going while the materials last
                p.task = if r.inputs.iter().all(|(it, q)| count(&p.inv, it) >= *q) { Task::Craft(r, r.ticks) } else { Task::None };
            }
            Task::Attack(mid) => {
                if !p.path.is_empty() {
                    return;
                }
                p.anim = anim::ATTACK;
                if p.cd > 0 {
                    return;
                }
                let w = weapon(p.weapon.as_deref());
                p.cd = w.speed;
                let lvl = p.skills.level(Skill::Ranged);
                let Some(mi) = self.npcs.iter().position(|m| m.id == mid) else { return };
                let def = self.npcs[mi].def;
                let chance = hit_chance(player_att_roll(lvl, w.acc), def_roll(def.defence));
                let dmg = if self.rand() < chance { (self.rand() * (max_hit(lvl, w.str) + 1) as f64) as i32 } else { 0 };
                let dmg = dmg.min(self.npcs[mi].hp);
                self.out.push(S2C::Hit { src: Who::P(pid).fmt(), dst: Who::N(mid).fmt(), dmg });
                self.npcs[mi].hp -= dmg;
                if pid == ME && self.surge.boss == Some(mid) {
                    self.surge.dmg += dmg;
                }
                if self.npcs[mi].target.is_none() {
                    self.npcs[mi].target = Some(pid);
                }
                if dmg > 0 {
                    self.add_xp(pi, Skill::Ranged, RANGED_XP_PER_DMG * dmg as f64);
                    self.add_xp(pi, Skill::Hitpoints, HP_XP_PER_DMG * dmg as f64);
                    self.add_xp(pi, Skill::Defence, DEF_XP_PER_DMG * dmg as f64);
                }
                if self.npcs[mi].hp <= 0 {
                    self.npc_dies(mi, pi);
                }
            }
        }
    }

    fn npc_dies(&mut self, mi: usize, killer: usize) {
        let m = &mut self.npcs[mi];
        m.hp = 0;
        m.anim = anim::DEAD;
        m.target = None;
        // surge creatures are removed a few ticks after they fall
        m.dead_until = Some(self.n + if m.temp { 4 } else { NPC_RESPAWN_TICKS });
        let (id, def) = (m.id, m.def);
        self.out.push(S2C::Death { who: Who::N(id).fmt() });
        self.players[killer].task = Task::None;
        let pid = self.players[killer].id;
        if pid == ME {
            self.quest_event("kill", def.kind);
        }
        if self.surge.boss == Some(id) {
            self.surge_finish(true);
            return;
        }
        let mut got = Vec::new();
        for (item, chance) in creature_drops(def.kind) {
            if self.rand() < *chance {
                if self.give(killer, item) {
                    got.push(item_name(item));
                } else {
                    self.msg(pid, format!("Your inventory is full: the {} crumbles to dust.", item_name(item).to_lowercase()));
                }
            }
        }
        if pid == ME {
            let inv = self.players[killer].inv.clone();
            self.out.push(S2C::Inv { inv });
            if !got.is_empty() {
                self.msg(pid, format!("You defeat the {}. Loot: {}.", def.name, got.join(", ")));
            }
        }
    }

    fn npc_tick(&mut self, grid: &mut WalkGrid) {
        let mut gone = Vec::new();
        for mi in 0..self.npcs.len() {
            if let Some(t) = self.npcs[mi].dead_until {
                if self.n >= t && self.npcs[mi].temp {
                    gone.push(self.npcs[mi].id);
                    continue;
                }
                if self.n >= t {
                    let m = &mut self.npcs[mi];
                    m.dead_until = None;
                    m.hp = m.def.hp;
                    m.tile = m.home;
                    m.anim = anim::IDLE;
                    m.cd = 0;
                }
                continue;
            }
            let (tile, home, def) = (self.npcs[mi].tile, self.npcs[mi].home, self.npcs[mi].def);
            if self.npcs[mi].cd > 0 {
                self.npcs[mi].cd -= 1;
            }
            // aggression: nearest player within range of the creature
            // aggression: nearest player within 5 tiles whose combat level is under twice ours,
            // and nobody else is already fighting (single-way combat)
            if self.npcs[mi].target.is_none() && chebyshev((tile.x, tile.y), (home.x, home.y)) <= LEASH_TILES / 2 {
                let busy: Vec<u32> = self.npcs.iter().filter(|m| m.dead_until.is_none()).filter_map(|m| m.target).collect();
                // surge creatures go for anyone nearby
                let temp = self.npcs[mi].temp;
                let near = self
                    .players
                    .iter()
                    .filter(|p| temp || (p.skills.combat_level() < 2 * def.level && !busy.contains(&p.id)))
                    .map(|p| (chebyshev((p.tile.x, p.tile.y), (tile.x, tile.y)), p.id))
                    .filter(|(d, _)| *d <= AGGRO_TILES)
                    .min();
                self.npcs[mi].target = near.map(|x| x.1);
            }
            let target = self.npcs[mi].target.and_then(|id| self.players.iter().position(|p| p.id == id));
            let leashed = chebyshev((tile.x, tile.y), (home.x, home.y)) > LEASH_TILES;
            match target {
                Some(pi) if !leashed => {
                    let pt = self.players[pi].tile;
                    let d = chebyshev((tile.x, tile.y), (pt.x, pt.y));
                    if d > def.range || d == 0 {
                        let next = step_toward(grid, tile, pt);
                        self.npcs[mi].tile = next;
                        self.npcs[mi].anim = if next != tile { anim::WALK } else { anim::IDLE };
                    } else {
                        self.npcs[mi].anim = anim::ATTACK;
                        if self.npcs[mi].cd == 0 {
                            self.npcs[mi].cd = def.speed;
                            let pdef = self.players[pi].skills.level(Skill::Defence);
                            let bonus = self.players[pi].gear.bonus();
                            let dmg = if self.rand() < hit_chance(creature_att_roll(def), player_def_roll(pdef, bonus)) { (self.rand() * (def.max_hit + 1) as f64) as i32 } else { 0 };
                            let p = &mut self.players[pi];
                            let dmg = dmg.min(p.hp);
                            p.hp -= dmg;
                            let (pid, hp, max) = (p.id, p.hp, p.skills.level(Skill::Hitpoints) as i32);
                            self.out.push(S2C::Hit { src: Who::N(self.npcs[mi].id).fmt(), dst: Who::P(pid).fmt(), dmg });
                            if pid == ME {
                                self.out.push(S2C::Hp { hp, max });
                            }
                            // bots (and idle players, like auto-retaliate) shoot back
                            if matches!(self.players[pi].task, Task::None) && self.players[pi].path.is_empty() {
                                self.players[pi].task = Task::Attack(self.npcs[mi].id);
                            }
                            if hp <= 0 {
                                self.player_dies(pi, grid);
                                self.npcs[mi].target = None;
                            }
                        }
                    }
                }
                _ => {
                    self.npcs[mi].target = None;
                    if chebyshev((tile.x, tile.y), (home.x, home.y)) > 4 {
                        // walk back home and heal
                        let next = step_toward(grid, tile, home);
                        self.npcs[mi].tile = if next == tile { home } else { next };
                        self.npcs[mi].anim = anim::WALK;
                        self.npcs[mi].hp = def.hp;
                    } else if self.rand() < 0.08 {
                        let w = home + IVec2::new(self.rand_i(-4, 4), self.rand_i(-4, 4));
                        if grid.walkable(w) {
                            self.npcs[mi].tile = w;
                            self.npcs[mi].anim = anim::WALK;
                        }
                    } else {
                        self.npcs[mi].anim = anim::IDLE;
                    }
                }
            }
        }
        for id in gone {
            self.npcs.retain(|m| m.id != id);
            self.out.push(S2C::NpcDespawn { id });
        }
    }

    fn player_dies(&mut self, pi: usize, grid: &mut WalkGrid) {
        let spawn = grid.nearest_walkable(self.spawn, 8).unwrap_or(self.spawn);
        let p = &mut self.players[pi];
        let pid = p.id;
        let lost = p.weapon.take();
        p.tile = spawn;
        p.path.clear();
        p.task = Task::None;
        p.hp = p.skills.level(Skill::Hitpoints) as i32;
        p.anim = anim::IDLE;
        let (hp, max) = (p.hp, p.hp);
        if p.bot.is_some() {
            p.weapon = lost.clone(); // bots keep their kit; they're just scenery
        }
        self.out.push(S2C::Death { who: Who::P(pid).fmt() });
        if pid == ME {
            self.out.push(S2C::Hp { hp, max });
            self.out.push(S2C::Equip { weapon: None });
            let text = match lost {
                Some(w) => format!("Oh dear, you are dead! Your {} is lost in the dust.", item_name(&w).to_lowercase()),
                None => "Oh dear, you are dead!".into(),
            };
            self.msg(pid, text);
            self.send_gear();
            if self.in_cave {
                // carried back up to the landing site
                self.up_at_spawn = true;
                let room = self.site.clone();
                self.out.push(S2C::Travel { room });
            }
        }
    }

    fn bot_brain(&mut self, pi: usize, grid: &mut WalkGrid) {
        let n = self.n;
        let role = self.players[pi].bot.as_ref().map_or("miner", |b| b.role);
        let chat_due = self.players[pi].bot.as_ref().is_some_and(|b| n >= b.next_chat);
        if chat_due {
            let k = self.rand_i(0, 99) as usize;
            let line = if k % 2 == 0 { LORE[k / 2 % LORE.len()] } else { role_line(role, k / 2) };
            let next = n + 120 + self.rand_i(0, 200) as u64;
            self.players[pi].bot.as_mut().unwrap().next_chat = next;
            self.say_as(pi, line);
        }
        let me = self.me_index();
        let my = me.map(|m| (self.players[m].tile, self.players[m].hp, self.players[m].skills.level(Skill::Hitpoints) as i32));
        let at = self.players[pi].tile;
        let dist_me = my.map_or(i32::MAX, |(t, _, _)| chebyshev((t.x, t.y), (at.x, at.y)));
        match role {
            "guard" => {
                let follow = self.players[pi].bot.as_ref().is_some_and(|b| b.follow_until > n);
                // a creature on the player nearby (or anywhere, while escorting): shoot it
                let threat = self.npcs.iter().filter(|m| m.dead_until.is_none() && m.target == Some(ME)).map(|m| m.id).next();
                if let Some(t) = threat.filter(|_| follow || dist_me <= 12) {
                    if !matches!(self.players[pi].task, Task::Attack(x) if x == t) {
                        self.players[pi].task = Task::Attack(t);
                        self.players[pi].path.clear();
                        self.players[pi].run = true;
                        if n % 3 == 0 {
                            self.say_as(pi, "Contact! I've got it!");
                        }
                    }
                    return;
                }
                if follow {
                    if matches!(self.players[pi].task, Task::None) && dist_me > 2 {
                        if let Some((t, _, _)) = my {
                            let path = grid.find_path(at, t, |x| chebyshev((x.x, x.y), (t.x, t.y)) <= 2);
                            self.players[pi].path = path.into_iter().collect();
                        }
                    }
                    return;
                }
                if self.players[pi].bot.as_ref().is_some_and(|b| b.follow_until != 0) {
                    self.players[pi].bot.as_mut().unwrap().follow_until = 0;
                    self.say_as(pi, "Escort's over. Back to my post.");
                }
            }
            "medic" => {
                if let Some((t, hp, max)) = my {
                    if hp * 2 < max && dist_me <= 15 && dist_me > 1 {
                        if self.players[pi].path.is_empty() || n % 4 == 0 {
                            let path = grid.find_path(at, t, |x| chebyshev((x.x, x.y), (t.x, t.y)) == 1);
                            self.players[pi].path = path.into_iter().collect();
                            self.players[pi].run = true;
                            self.players[pi].task = Task::None;
                        }
                        return;
                    }
                }
            }
            _ => {}
        }
        let p = &self.players[pi];
        if !matches!(p.task, Task::None) || !p.path.is_empty() {
            return;
        }
        if matches!(role, "guard" | "medic" | "quartermaster") {
            // stay on post near the landing site / fabricator
            let idle = self.rand_i(3, 10) as u32;
            let b = self.players[pi].bot.as_mut().unwrap();
            if b.idle > 0 {
                b.idle -= 1;
                return;
            }
            b.idle = idle;
            let home = if role == "quartermaster" { self.nodes.first().map_or(self.spawn, |f| f.tile) } else { self.spawn };
            let r = if role == "guard" { 8 } else { 4 };
            let to = home + IVec2::new(self.rand_i(-r, r), self.rand_i(-r, r));
            if my.is_some_and(|(t, _, _)| chebyshev((to.x, to.y), (t.x, t.y)) <= 1) {
                return; // don't stand on the player
            }
            let path = grid.find_path(at, to, |t| t == to);
            self.players[pi].path = path.into_iter().collect();
            self.players[pi].run = false;
            return;
        }
        let idle = self.rand_i(2, 8) as u32;
        let b = self.players[pi].bot.as_mut().unwrap();
        if b.idle > 0 {
            b.idle -= 1;
            return;
        }
        b.idle = idle;
        let p = &mut self.players[pi];
        if p.inv.iter().filter(|s| s.is_some()).count() > 20 {
            p.inv = new_inv(); // banked, in spirit
        }
        let at = p.tile;
        if self.rand() < 0.75 {
            let lvl = |s: &Skills, d: &NodeDef| s.level(d.skill) >= d.level;
            let skills = self.players[pi].skills.clone();
            let best = self
                .nodes
                .iter()
                .filter(|n| n.up && node_def(n.kind).is_some_and(|d| lvl(&skills, d) && (d.skill == Skill::Survey) == (role == "surveyor")))
                // leave the rocks next to the player for the player
                .filter(|n| my.is_none_or(|(t, _, _)| chebyshev((n.tile.x, n.tile.y), (t.x, t.y)) > 5))
                .map(|n| (chebyshev((n.tile.x, n.tile.y), (at.x, at.y)), n.id, n.tile))
                .filter(|(d, _, _)| *d <= 30)
                .min_by_key(|(d, id, _)| d * 4 + (*id as i32 * 7 + n as i32) % 13);
            if let Some((_, nid, nt)) = best {
                let path = grid.find_path(at, nt, |t| chebyshev((t.x, t.y), (nt.x, nt.y)) == 1);
                let p = &mut self.players[pi];
                p.path = path.into_iter().collect();
                p.run = false;
                p.task = Task::Gather(nid);
                p.timer = 0;
                return;
            }
        }
        let to = self.spawn + IVec2::new(self.rand_i(-12, 12), self.rand_i(-12, 12));
        if my.is_some_and(|(t, _, _)| chebyshev((to.x, to.y), (t.x, t.y)) <= 1) {
            return;
        }
        let path = grid.find_path(at, to, |t| t == to);
        self.players[pi].path = path.into_iter().collect();
    }
}

/// One greedy 8-neighbour step from `a` toward `b` (creatures chasing).
fn step_toward(grid: &mut WalkGrid, a: IVec2, b: IVec2) -> IVec2 {
    let d = (b - a).signum();
    for c in [d, IVec2::new(d.x, 0), IVec2::new(0, d.y)] {
        let corner_ok = c.x == 0 || c.y == 0 || (grid.walkable(a + IVec2::new(c.x, 0)) && grid.walkable(a + IVec2::new(0, c.y)));
        if c != IVec2::ZERO && a + c != b && grid.walkable(a + c) && corner_ok {
            return a + c;
        }
    }
    a
}

// ------------------------------------------------------------------------------------ new features

impl Sim {
    /// Everything the client gets queued since the last update (travel applies it at once).
    pub fn take_out(&mut self) -> Vec<S2C> {
        std::mem::take(&mut self.out)
    }

    fn welcome(&self) -> S2C {
        let me = self.players.iter().find(|p| p.id == ME);
        let players = self
            .players
            .iter()
            .filter(|p| p.id != ME)
            .map(|p| PlayerInfo { id: p.id, name: p.name.clone(), tx: p.tile.x, tz: p.tile.y, bot: p.bot.is_some(), role: p.bot.as_ref().map(|b| b.role.to_string()) })
            .collect();
        let nodes = self.nodes.iter().map(|n| NodeInfo { id: n.id, kind: n.kind.into(), tx: n.tile.x, tz: n.tile.y, up: n.up }).collect();
        let npcs = self.npcs.iter().map(|m| NpcInfo { id: m.id, kind: m.def.kind.into(), tx: m.tile.x, tz: m.tile.y, hp: m.hp, max: m.def.hp, dead: m.dead_until.is_some() }).collect();
        let you = match me {
            Some(p) => You {
                name: Some(p.name.clone()),
                tx: p.tile.x,
                tz: p.tile.y,
                skills: p.skills.clone(),
                inv: p.inv.clone(),
                weapon: p.weapon.clone(),
                hp: p.hp,
                max_hp: p.skills.level(Skill::Hitpoints) as i32,
                gear: p.gear.clone(),
                quests: self.quests.clone(),
                quest_points: quest_points(&self.quests),
            },
            None => You { name: None, tx: self.spawn.x, tz: self.spawn.y, skills: Skills::fresh(), inv: new_inv(), weapon: None, hp: 10, max_hp: 10, gear: Gear::default(), quests: HashMap::new(), quest_points: 0 },
        };
        let cave = if self.in_cave {
            let g = cave::generate(&self.site);
            Some(CaveInfo { w: g.w as u32, h: g.h as u32, rle: g.rle() })
        } else {
            None
        };
        S2C::Welcome {
            id: ME,
            token: "offline".into(),
            tick_ms: self.tick_ms,
            you,
            players,
            nodes,
            npcs,
            room: Some(if self.in_cave { "cave" } else { "surface" }.into()),
            event: self.surge_info(),
            cave,
            exit: None,
            tick: self.n,
        }
    }

    /// Down the pit / up the ladder (the client calls this on our `travel`).
    pub fn travel(&mut self, room: &str) {
        let down = room.ends_with("~cave");
        let Some(mi) = self.players.iter().position(|p| p.id == ME) else { return };
        if down && !self.in_cave {
            let mut me = self.players.remove(mi);
            let g = cave::generate(&self.site);
            self.stash = Some(Stash { players: std::mem::take(&mut self.players), nodes: std::mem::take(&mut self.nodes), npcs: std::mem::take(&mut self.npcs), spawn: self.spawn });
            self.nodes = g.nodes.iter().enumerate().map(|(i, (k, t))| N { id: i as u32, kind: k, tile: *t, up: true, respawn_at: 0 }).collect();
            self.npcs = g
                .npcs
                .iter()
                .enumerate()
                .filter_map(|(i, (k, t))| creature_def(k).map(|def| M { id: i as u32 + 1, def, home: *t, tile: *t, hp: def.hp, target: None, cd: 0, dead_until: None, anim: anim::IDLE, sent: None, temp: false }))
                .collect();
            me.tile = g.spawn;
            me.path.clear();
            me.task = Task::None;
            me.sent = None;
            self.spawn = g.spawn;
            self.players = vec![me];
            self.in_cave = true;
            self.dialog = None;
            let w = self.welcome();
            self.out.push(w);
            self.out.push(S2C::Msg { text: "You climb down into the lava tube. The walls glow faintly, and something skitters in the dark.".into() });
            self.quest_event("enter", "cave");
            self.hold = true;
        } else if !down && self.in_cave {
            let mut me = self.players.remove(mi);
            let Some(st) = self.stash.take() else { return };
            self.players = st.players;
            self.nodes = st.nodes;
            self.npcs = st.npcs;
            self.spawn = st.spawn;
            let pit = self.nodes.iter().find(|n| n.kind == "pit").map(|n| n.tile);
            me.tile = match (std::mem::take(&mut self.up_at_spawn), pit) {
                (false, Some(t)) => t + IVec2::new(1, 0),
                _ => self.spawn,
            };
            me.path.clear();
            me.task = Task::None;
            me.sent = None;
            self.players.insert(0, me);
            self.in_cave = false;
            let w = self.welcome();
            self.out.push(w);
            self.hold = true;
        }
    }

    fn send_gear(&mut self) {
        let Some(p) = self.players.iter().find(|p| p.id == ME) else { return };
        let g = p.gear.clone();
        let bonus = g.bonus();
        self.out.push(S2C::Gear { weapon: p.weapon.clone(), helmet: g.helmet, suit: g.suit, shield: g.shield, cape: g.cape, bonus });
    }

    fn take_off(&mut self, slot: &str) {
        let Some(pi) = self.me_index() else { return };
        let p = &mut self.players[pi];
        let Some(free) = p.inv.iter().position(|s| s.is_none()) else {
            self.msg(ME, "You don't have enough inventory space.");
            return;
        };
        let Some(item) = p.gear.slot_mut(slot).and_then(|g| g.take()) else { return };
        p.inv[free] = Some(Slot { item, qty: 1 });
        let inv = p.inv.clone();
        self.out.push(S2C::Inv { inv });
        self.send_gear();
    }

    fn near_locker(&self) -> bool {
        let Some(me) = self.players.iter().find(|p| p.id == ME) else { return false };
        self.nodes.iter().any(|n| n.kind == "locker" && chebyshev((n.tile.x, n.tile.y), (me.tile.x, me.tile.y)) <= 1)
    }

    fn bank_deposit(&mut self, slot: usize, qty: Qty) {
        if !self.near_locker() {
            return self.msg(ME, "You need to stand next to the colony locker.");
        }
        let Some(pi) = self.me_index() else { return };
        let Some(item) = self.players[pi].inv.get(slot).cloned().flatten().map(|s| s.item) else { return };
        let have = count(&self.players[pi].inv, &item);
        let n = match qty {
            Qty::All => have,
            Qty::N(k) => k.min(have),
        };
        if n == 0 {
            return;
        }
        if !self.bank.iter().any(|s| s.item == item) && self.bank.len() >= 100 {
            return self.msg(ME, "Your locker is full.");
        }
        let mut left = n;
        for s in self.players[pi].inv.iter_mut() {
            if left == 0 {
                break;
            }
            if let Some(x) = s.as_mut().filter(|x| x.item == item) {
                let take = x.qty.min(left);
                x.qty -= take;
                left -= take;
                if x.qty == 0 {
                    *s = None;
                }
            }
        }
        match self.bank.iter_mut().find(|s| s.item == item) {
            Some(b) => b.qty += n,
            None => self.bank.push(Slot { item, qty: n }),
        }
        let (inv, items) = (self.players[pi].inv.clone(), self.bank.clone());
        self.out.push(S2C::Inv { inv });
        self.out.push(S2C::Bank { items, slots: 100 });
    }

    fn bank_withdraw(&mut self, idx: usize, qty: Qty) {
        if !self.near_locker() {
            return self.msg(ME, "You need to stand next to the colony locker.");
        }
        let Some(pi) = self.me_index() else { return };
        let Some(e) = self.bank.get(idx).cloned() else { return };
        let want = match qty {
            Qty::All => e.qty,
            Qty::N(k) => k.min(e.qty),
        };
        let mut got = 0;
        for _ in 0..want {
            if !self.give(pi, &e.item) {
                break;
            }
            got += 1;
        }
        if got == 0 {
            return self.msg(ME, "You don't have enough inventory space.");
        }
        self.bank[idx].qty -= got;
        if self.bank[idx].qty == 0 {
            self.bank.remove(idx);
        }
        let (inv, items) = (self.players[pi].inv.clone(), self.bank.clone());
        self.out.push(S2C::Inv { inv });
        self.out.push(S2C::Bank { items, slots: 100 });
    }

    fn rub(&mut self, slot: usize, skill: &str) {
        let Some(pi) = self.me_index() else { return };
        if self.players[pi].inv.get(slot).cloned().flatten().is_none_or(|s| s.item != "xp_lamp") {
            return self.out.push(S2C::Err { text: "That isn't a lamp.".into() });
        }
        let Some(k) = Skill::parse(skill) else { return };
        let xp = lamp_xp(self.players[pi].skills.level(k));
        self.players[pi].inv[slot] = None;
        let inv = self.players[pi].inv.clone();
        self.out.push(S2C::Inv { inv });
        self.msg(ME, format!("The lamp flares and memories that aren't yours flood in. (+{xp} {skill} xp)"));
        self.add_xp(pi, k, xp);
    }

    // ---- quests

    fn send_quest(&mut self, q: &'static QuestDef) {
        let Some(st) = self.quests.get(q.id).cloned() else { return };
        let step = &q.steps[(st.step as usize).min(q.steps.len() - 1)];
        let qp = quest_points(&self.quests);
        self.out.push(S2C::Quest { id: q.id.into(), state: st.s, step: st.step, progress: st.p.min(step.n), goal: step.n, qp });
    }

    /// Something happened that quests may count (kill / gather / craft / enter).
    fn quest_event(&mut self, kind: &str, target: &str) {
        for q in QUESTS {
            let Some(st) = self.quests.get_mut(q.id) else { continue };
            if st.s != "active" {
                continue;
            }
            let step = &q.steps[st.step as usize];
            if step.kind != kind || step.target != target || st.p >= step.n {
                continue;
            }
            st.p += 1;
            if st.p >= step.n && (st.step as usize) < q.steps.len() - 1 {
                st.step += 1;
                st.p = 0;
                let next = q.steps[st.step as usize].text;
                self.out.push(S2C::Msg { text: format!("Quest updated: {}. {next}.", q.name) });
            } else if st.p >= step.n {
                self.out.push(S2C::Msg { text: format!("{}: done! Go back and tell them.", q.name) });
            }
            self.send_quest(q);
        }
    }

    fn quest_ready(&self, q: &QuestDef) -> bool {
        let Some(st) = self.quests.get(q.id) else { return false };
        if st.s != "active" || st.step as usize != q.steps.len() - 1 {
            return false;
        }
        let step = &q.steps[st.step as usize];
        let inv = self.players.iter().find(|p| p.id == ME).map(|p| p.inv.as_slice()).unwrap_or(&[]);
        if step.kind == "deliver" { count(inv, step.target) >= step.n } else { st.p >= step.n }
    }

    fn quest_lock(&self, q: &QuestDef) -> Option<String> {
        let me = self.players.iter().find(|p| p.id == ME)?;
        q.req.iter().find(|(k, l)| me.skills.level(*k) < *l).map(|(k, l)| format!("{} {l}", k.key()))
    }

    fn dialog(&mut self, ci: usize, text: String, quest: Option<&'static str>, options: Vec<(String, String)>) {
        let c = &self.players[ci];
        let role = c.bot.as_ref().map(|b| b.role.to_string());
        self.dialog = Some((c.id, quest, options.iter().map(|o| o.0.clone()).collect()));
        let options = options.into_iter().map(|(id, text)| DialogOpt { id, text }).collect();
        self.out.push(S2C::Dialog { npc: c.id, name: c.name.clone(), role, text, quest: quest.map(Into::into), options });
    }

    fn talk(&mut self, ci: usize) {
        let Some(role) = self.players[ci].bot.as_ref().map(|b| b.role) else { return };
        let roles: Vec<&str> = self.players.iter().filter_map(|p| p.bot.as_ref().map(|b| b.role)).collect();
        let mine: Vec<&'static QuestDef> = QUESTS.iter().filter(|q| (if roles.contains(&q.giver) { q.giver } else { "quartermaster" }) == role).collect();
        if let Some(q) = mine.iter().find(|q| self.quest_ready(q)) {
            return self.dialog(ci, format!("Is that it? {}, all done?", q.name), Some(q.id), vec![("turn_in".into(), "Here you go.".into()), ("bye".into(), "Not yet.".into())]);
        }
        if let Some(q) = mine.iter().find(|q| self.quests.get(q.id).is_some_and(|s| s.s == "active")) {
            let st = &self.quests[q.id];
            let step = &q.steps[st.step as usize];
            let inv = self.players.iter().find(|p| p.id == ME).map(|p| p.inv.clone()).unwrap_or_default();
            let prog = if step.kind == "deliver" { count(&inv, step.target) } else { st.p };
            return self.dialog(ci, format!("{}: {} ({}/{}). I'm counting on you.", q.name, step.text, prog.min(step.n), step.n), Some(q.id), vec![("bye".into(), "On it.".into())]);
        }
        let open: Vec<&'static QuestDef> = mine.iter().copied().filter(|q| !self.quests.contains_key(q.id) && self.quest_lock(q).is_none()).collect();
        if open.len() == 1 {
            return self.offer(ci, open[0]);
        }
        if open.len() > 1 {
            let mut opts: Vec<(String, String)> = open.iter().map(|q| (format!("offer:{}", q.id), q.name.to_string())).collect();
            opts.push(("bye".into(), "Not now.".into()));
            return self.dialog(ci, "I've got work, if you want it.".into(), None, opts);
        }
        if let Some(q) = mine.iter().find(|q| !self.quests.contains_key(q.id)) {
            let lock = self.quest_lock(q).unwrap_or_default();
            return self.dialog(ci, format!("I might have work for you once you've got {lock}."), Some(q.id), vec![("bye".into(), "Okay.".into())]);
        }
        let name = self.players.iter().find(|p| p.id == ME).map_or(String::new(), |p| p.name.clone());
        self.dialog(ci, format!("Nothing for you right now, {name}. Stay clear of the rifts."), None, vec![("bye".into(), "Bye.".into())]);
    }

    fn offer(&mut self, ci: usize, q: &'static QuestDef) {
        self.dialog(ci, format!("{} (Reward: {}.)", q.offer, q.reward), Some(q.id), vec![("accept".into(), "I'll do it.".into()), ("decline".into(), "Not now.".into())]);
    }

    fn choose(&mut self, option: &str) {
        let Some((npc, quest, opts)) = self.dialog.take() else {
            return self.out.push(S2C::Err { text: "That option isn't open.".into() });
        };
        if !opts.iter().any(|o| o == option) {
            return self.out.push(S2C::Err { text: "That option isn't open.".into() });
        }
        if let Some(id) = option.strip_prefix("offer:") {
            if let (Some(ci), Some(q)) = (self.players.iter().position(|p| p.id == npc), quest_def(id)) {
                return self.offer(ci, q);
            }
            return self.out.push(S2C::DialogEnd);
        }
        self.out.push(S2C::DialogEnd);
        let Some(q) = quest.and_then(quest_def) else { return };
        match option {
            "accept" => {
                if self.quests.contains_key(q.id) || self.quest_lock(q).is_some() {
                    return;
                }
                self.quests.insert(q.id.into(), QuestSt { s: "active".into(), step: 0, p: 0 });
                self.out.push(S2C::Msg { text: format!("Quest started: {}. {}.", q.name, q.steps[0].text) });
                self.send_quest(q);
                // already down the tube / already holding the thing
                if q.steps[0].kind == "enter" && self.in_cave {
                    self.quest_event("enter", "cave");
                }
            }
            "turn_in" => {
                if !self.quest_ready(q) {
                    return self.out.push(S2C::Msg { text: "You don't have everything yet.".into() });
                }
                let Some(pi) = self.me_index() else { return };
                let mut items: Vec<&str> = Vec::new();
                for (it, n) in q.items {
                    for _ in 0..*n {
                        items.push(it);
                    }
                }
                for _ in 0..q.lamps {
                    items.push("xp_lamp");
                }
                let step = &q.steps[q.steps.len() - 1];
                let freed = if step.kind == "deliver" && !stacks(step.target) { step.n as usize } else { 0 };
                let need = items.iter().filter(|i| !stacks(i)).count() + usize::from(items.iter().any(|i| stacks(i)));
                if self.players[pi].inv.iter().filter(|s| s.is_none()).count() + freed < need {
                    return self.out.push(S2C::Msg { text: "Make some room in your pack first.".into() });
                }
                if step.kind == "deliver" {
                    let mut left = step.n;
                    for s in self.players[pi].inv.iter_mut() {
                        if left == 0 {
                            break;
                        }
                        if let Some(x) = s.as_mut().filter(|x| x.item == step.target) {
                            let take = x.qty.min(left);
                            x.qty -= take;
                            left -= take;
                            if x.qty == 0 {
                                *s = None;
                            }
                        }
                    }
                }
                for it in items {
                    self.give(pi, it);
                }
                self.quests.insert(q.id.into(), QuestSt { s: "done".into(), step: q.steps.len() as u32 - 1, p: step.n });
                let inv = self.players[pi].inv.clone();
                self.out.push(S2C::Inv { inv });
                self.say_as_id(npc, q.thanks);
                self.out.push(S2C::Msg { text: format!("Quest complete: {}! You get {}. (+{} quest point{})", q.name, q.reward, q.qp, if q.qp > 1 { "s" } else { "" }) });
                if q.id == "echoes" {
                    self.out.push(S2C::Msg { text: "New fabricator recipes unlocked.".into() });
                }
                self.send_quest(q);
            }
            _ => {}
        }
    }

    fn say_as_id(&mut self, id: u32, msg: &str) {
        if let Some(i) = self.players.iter().position(|p| p.id == id) {
            self.say_as(i, msg);
        }
    }

    // ---- Rift Surge

    fn surge_info(&self) -> Option<EventInfo> {
        let phase = match self.surge.phase {
            1 => "warning",
            2 => "open",
            3 => "boss",
            _ => return None,
        };
        Some(EventInfo { kind: "surge".into(), phase: phase.into(), tx: self.surge.spot.x, tz: self.surge.spot.y, ends_in: self.surge.until.saturating_sub(self.n) })
    }

    fn surge_announce(&mut self, text: &str) {
        let e = self.surge_info().unwrap_or(EventInfo { kind: "surge".into(), phase: "ended".into(), tx: self.surge.spot.x, tz: self.surge.spot.y, ends_in: 0 });
        self.out.push(S2C::Event(e));
        self.out.push(S2C::Msg { text: text.into() });
    }

    /// Test hook (`--mmo-script surge:now` / `surge:boss`).
    pub fn cheat_surge(&mut self, how: &str) {
        if self.in_cave {
            return;
        }
        self.surge_clear();
        if how == "boss" {
            let me = self.players.iter().find(|p| p.id == ME).map_or(self.spawn, |p| p.tile);
            self.surge.spot = me + IVec2::new(4, 0);
            self.surge.count += 1;
            self.surge.phase = 2;
            self.surge.until = self.n; // straight to the boss on the next tick
        } else {
            self.surge.next = self.n + 1;
        }
    }

    fn surge_clear(&mut self) {
        let temp: Vec<u32> = self.npcs.iter().filter(|m| m.temp).map(|m| m.id).collect();
        for id in temp {
            self.npcs.retain(|m| m.id != id);
            self.out.push(S2C::NpcDespawn { id });
        }
        self.surge.phase = 0;
        self.surge.boss = None;
        self.surge.dmg = 0;
    }

    fn spawn_temp(&mut self, grid: &mut WalkGrid, kind: &str, at: IVec2) -> Option<u32> {
        let def = creature_def(kind)?;
        let home = grid.nearest_walkable(at, 6).unwrap_or(at);
        self.next_temp += 1;
        let id = self.next_temp;
        self.npcs.push(M { id, def, home, tile: home, hp: def.hp, target: None, cd: 0, dead_until: None, anim: anim::IDLE, sent: None, temp: true });
        self.out.push(S2C::NpcSpawn { npc: NpcInfo { id, kind: kind.into(), tx: home.x, tz: home.y, hp: def.hp, max: def.hp, dead: false } });
        Some(id)
    }

    fn surge_tick(&mut self, grid: &mut WalkGrid) {
        let n = self.n;
        match self.surge.phase {
            0 => {
                if self.surge.next != 0 && n >= self.surge.next {
                    let a = self.rand() * std::f64::consts::TAU;
                    let d = 15.0 + self.rand() * 20.0;
                    let t = self.spawn + IVec2::new((a.cos() * d).round() as i32, (a.sin() * d).round() as i32);
                    self.surge.spot = grid.nearest_walkable(t, 6).unwrap_or(t);
                    self.surge.count += 1;
                    self.surge.phase = 1;
                    self.surge.until = n + if self.surge.count == 1 && self.surge.next <= n + 1 { SURGE_WARN } else { SURGE_WARN };
                    self.surge_announce("The ground hums. A Rift Surge is building near the landing site!");
                }
            }
            1 => {
                if n >= self.surge.until {
                    self.surge.phase = 2;
                    self.surge.wave = 0;
                    self.surge.until = n + SURGE_WAVE_GAP * SURGE_WAVES.len() as u64;
                    self.surge_announce("A Rift Surge tears open! Creatures are pouring out.");
                    self.surge_wave(grid);
                }
            }
            2 => {
                if n >= self.surge.until {
                    self.surge.phase = 3;
                    self.surge.until = n + SURGE_BOSS;
                    self.surge.dmg = 0;
                    let spot = self.surge.spot;
                    self.surge.boss = self.spawn_temp(grid, "leviathan", spot);
                    self.surge_announce("The rift screams. A Void Leviathan forces its way through!");
                } else if (self.surge.until - n) % SURGE_WAVE_GAP == 0 {
                    self.surge_wave(grid);
                }
            }
            _ => {
                let alive = self.surge.boss.is_some_and(|b| self.npcs.iter().any(|m| m.id == b && m.dead_until.is_none()));
                if !alive || n >= self.surge.until {
                    self.surge_finish(false);
                }
            }
        }
    }

    fn surge_wave(&mut self, grid: &mut WalkGrid) {
        let Some(w) = SURGE_WAVES.get(self.surge.wave) else { return };
        self.surge.wave += 1;
        for (kind, k) in w.iter() {
            for _ in 0..*k {
                let at = self.surge.spot + IVec2::new(self.rand_i(-3, 3), self.rand_i(-3, 3));
                self.spawn_temp(grid, kind, at);
            }
        }
    }

    fn surge_finish(&mut self, killed: bool) {
        self.surge.phase = 0;
        let boss = self.surge.boss.take();
        if killed && self.surge.dmg * 20 >= 250 {
            let mut drops = Vec::new();
            for _ in 0..self.rand_i(3, 8) {
                drops.push("void_crystal");
            }
            if self.rand() < 1.0 / 8.0 {
                drops.push("rift_blade");
            }
            if self.rand() < 1.0 / 5.0 {
                drops.push("void_cape");
            }
            if let Some(pi) = self.me_index() {
                let got: Vec<&str> = drops.into_iter().filter(|it| self.give(pi, it)).collect();
                let inv = self.players[pi].inv.clone();
                self.out.push(S2C::Inv { inv });
                let share = (100 * self.surge.dmg / 250).min(100);
                let mut counts: Vec<(&str, u32)> = Vec::new();
                for it in got {
                    match counts.iter_mut().find(|c| c.0 == it) {
                        Some(c) => c.1 += 1,
                        None => counts.push((it, 1)),
                    }
                }
                let list: Vec<String> = counts.iter().map(|(i, k)| if *k > 1 { format!("{k} x {}", item_name(i)) } else { item_name(i).to_string() }).collect();
                self.out.push(S2C::Msg { text: format!("Surge loot ({share}% of the damage): {}.", if list.is_empty() { "nothing (inventory full)".into() } else { list.join(", ") }) });
            }
            self.surge_announce("The Void Leviathan falls! The rift seals.");
        } else if killed {
            self.surge_announce("The Void Leviathan falls! The rift seals. (You didn't do enough damage for a share of the loot.)");
        } else {
            self.surge_announce("The rift collapses, dragging the creatures back into the void.");
        }
        let temp: Vec<u32> = self.npcs.iter().filter(|m| m.temp && (Some(m.id) != boss || !killed)).map(|m| m.id).collect();
        for id in temp {
            self.npcs.retain(|m| m.id != id);
            self.out.push(S2C::NpcDespawn { id });
        }
        self.surge.dmg = 0;
        self.surge.next = self.n + 2000 + self.rand_i(0, 1000) as u64;
    }
}
