//! MarsScape offline world: the server's rules run in-process, so the mode is playable solo (and
//! on GitHub Pages) with no server. Speaks the same messages as the network (proto.rs): the
//! client sends `C2S` through `handle` and receives `S2C` from `update`, exactly as online.
//! Colonist bots live here too: a quartermaster with a starter gift, a guard who joins your fights
//! (say "help" or "follow me" for an escort, "stop" to release it), a medic, a miner and a surveyor.

use super::grid::WalkGrid;
use super::proto::*;
use bevy::math::IVec2;
use std::collections::VecDeque;

const ME: u32 = 1;

#[derive(Clone, Copy)]
enum Task {
    None,
    Gather(u32),
    Craft(&'static Recipe, u32),
    Attack(u32),
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
}

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
        let mut s = Sim { tick_ms: 600, n: 0, acc: 0.0, spawn, players: Vec::new(), nodes: Vec::new(), npcs: Vec::new(), rng: seed | 1, out: Vec::new(), joined: false, greet_at: None, gift_given: false };
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
                    self.npcs.push(M { id: mid, def, home, tile: home, hp: def.hp, target: None, cd: 0, dead_until: None, anim: anim::IDLE, sent: None });
                }
            }
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
                self.players[m].bot.as_mut().unwrap().heal_at = n + 12;
                self.players[m].anim = anim::CRAFT;
                let p = &mut self.players[me];
                p.hp = (p.hp + 10).min(max);
                let hp = p.hp;
                self.say_as(m, "Hold still... there. Patched you up.");
                self.out.push(S2C::Heal { src: Who::P(self.players[m].id).fmt(), dst: Who::P(ME).fmt(), amt: 10 });
                self.out.push(S2C::Hp { hp, max });
                self.out.push(S2C::Msg { text: "Doc Hollis heals you (+10 hitpoints).".into() });
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
                self.players.insert(0, P { id: ME, name: name.clone(), tile, path: VecDeque::new(), run: false, task: Task::None, timer: 0, skills: skills.clone(), inv: inv.clone(), weapon: None, hp, cd: 0, anim: anim::IDLE, sent: None, bot: None });
                let players = self
                    .players
                    .iter()
                    .map(|p| PlayerInfo { id: p.id, name: p.name.clone(), tx: p.tile.x, tz: p.tile.y, bot: p.bot.is_some(), role: p.bot.as_ref().map(|b| b.role.to_string()) })
                    .collect();
                let nodes = self.nodes.iter().map(|n| NodeInfo { id: n.id, kind: n.kind.into(), tx: n.tile.x, tz: n.tile.y, up: n.up }).collect();
                let npcs = self.npcs.iter().map(|m| NpcInfo { id: m.id, kind: m.def.kind.into(), tx: m.tile.x, tz: m.tile.y, hp: m.hp, max: m.def.hp }).collect();
                self.out.push(S2C::Welcome {
                    id: ME,
                    token: "offline".into(),
                    tick_ms: self.tick_ms,
                    you: You { name: Some(name.clone()), tx: tile.x, tz: tile.y, skills, inv, weapon: None, hp, max_hp: hp },
                    players,
                    nodes,
                    npcs,
                });
                self.out.push(S2C::Msg { text: format!("Welcome to MarsScape, {name}. You're offline: this world runs on your machine.") });
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
            C2S::Unequip => {
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
        }
    }

    fn act(&mut self, pid: u32, node: u32) {
        let Some(n) = self.nodes.iter().find(|n| n.id == node) else { return };
        let kind = n.kind;
        let Some(p) = self.players.iter_mut().find(|p| p.id == pid) else { return };
        match kind {
            "fabricator" => {}
            "rift" => self.msg(pid, "A tear into the void. Stars where the ground should be, and things crawling out."),
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
        m.dead_until = Some(self.n + NPC_RESPAWN_TICKS);
        let (id, def) = (m.id, m.def);
        self.out.push(S2C::Death { who: Who::N(id).fmt() });
        self.players[killer].task = Task::None;
        let pid = self.players[killer].id;
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
        for mi in 0..self.npcs.len() {
            if let Some(t) = self.npcs[mi].dead_until {
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
                let near = self
                    .players
                    .iter()
                    .filter(|p| p.skills.combat_level() < 2 * def.level && !busy.contains(&p.id))
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
                            let dmg = if self.rand() < hit_chance(creature_att_roll(def), def_roll(pdef)) { (self.rand() * (def.max_hit + 1) as f64) as i32 } else { 0 };
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
        if c != IVec2::ZERO && a + c != b && grid.walkable(a + c) {
            return a + c;
        }
    }
    a
}
