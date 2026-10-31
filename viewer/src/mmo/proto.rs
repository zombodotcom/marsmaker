//! MarsScape wire protocol (JSON text frames over a WebSocket) and the game rules both the
//! offline simulation (sim.rs) and the server (server/) use: skills, the XP curve, nodes,
//! recipes, weapons, void creatures and the combat formulas. Unknown message types and fields are ignored.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const VER: u32 = 1;
pub const INV_SLOTS: usize = 28;
pub const TILE_M: f32 = 2.0;
pub const MAX_PATH: usize = 64;
pub const CHAT_MAX: usize = 80;

// ------------------------------------------------------------------------------------ messages

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum C2S {
    Hello { ver: u32, name: String, token: String, bounds: [i32; 4], spawn: [i32; 2] },
    Walk { path: Vec<[i32; 2]>, run: bool },
    /// gather, or use a locker / pit / ladder
    Act { node: u32 },
    Craft { recipe: String },
    Drop { slot: usize },
    Chat { msg: String },
    /// wield a weapon or wear armour (it goes to the item's gear slot)
    Equip { slot: usize },
    /// `gear`: "weapon" (default), "helmet", "suit", "shield", "cape"
    Unequip {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gear: Option<String>,
    },
    Attack { npc: u32 },
    Eat { slot: usize },
    Ping,
    BankDeposit { slot: usize, qty: Qty },
    BankWithdraw { idx: usize, qty: Qty },
    TradeRequest { id: u32 },
    TradeOffer { slot: usize, qty: Qty },
    TradeRemove { idx: usize, qty: Qty },
    TradeAccept,
    TradeDecline,
    /// a colonist: the server walks you over, then answers with `dialog`
    Talk { id: u32 },
    DialogChoose { option: String },
    /// rub an Insight Lamp on a skill
    Rub { slot: usize, skill: String },
}

/// An amount: a number, or "all".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qty {
    N(u32),
    All,
}

impl Serialize for Qty {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Qty::N(n) => s.serialize_u32(*n),
            Qty::All => s.serialize_str("all"),
        }
    }
}

impl<'de> Deserialize<'de> for Qty {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum R {
            N(u32),
            S(serde::de::IgnoredAny),
        }
        Ok(match R::deserialize(d)? {
            R::N(n) => Qty::N(n),
            R::S(_) => Qty::All,
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Slot {
    pub item: String,
    #[serde(default = "one")]
    pub qty: u32,
}

fn one() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Skills {
    #[serde(default)]
    pub mining: f64,
    #[serde(default)]
    pub survey: f64,
    #[serde(default)]
    pub fabrication: f64,
    #[serde(default)]
    pub ranged: f64,
    #[serde(default)]
    pub defence: f64,
    #[serde(default = "hp_start_xp")]
    pub hitpoints: f64,
}

fn hp_start_xp() -> f64 {
    HP_START_XP
}

impl Skills {
    pub fn fresh() -> Self {
        Skills { hitpoints: HP_START_XP, ..default() }
    }
    pub fn get(&self, s: Skill) -> f64 {
        match s {
            Skill::Mining => self.mining,
            Skill::Survey => self.survey,
            Skill::Fabrication => self.fabrication,
            Skill::Ranged => self.ranged,
            Skill::Defence => self.defence,
            Skill::Hitpoints => self.hitpoints,
        }
    }
    pub fn set(&mut self, s: Skill, xp: f64) {
        *match s {
            Skill::Mining => &mut self.mining,
            Skill::Survey => &mut self.survey,
            Skill::Fabrication => &mut self.fabrication,
            Skill::Ranged => &mut self.ranged,
            Skill::Defence => &mut self.defence,
            Skill::Hitpoints => &mut self.hitpoints,
        } = xp.min(MAX_XP);
    }
    pub fn level(&self, s: Skill) -> u32 {
        level_for_xp(self.get(s))
    }
    pub fn combat_level(&self) -> u32 {
        combat_level(self.level(Skill::Ranged), self.level(Skill::Defence), self.level(Skill::Hitpoints))
    }
}

fn default<T: Default>() -> T {
    T::default()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct You {
    /// the name the server settled on (cleaned / renamed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub tx: i32,
    pub tz: i32,
    #[serde(default = "Skills::fresh")]
    pub skills: Skills,
    #[serde(default)]
    pub inv: Vec<Option<Slot>>,
    #[serde(default)]
    pub weapon: Option<String>,
    #[serde(default = "ten")]
    pub hp: i32,
    #[serde(default = "ten")]
    pub max_hp: i32,
    #[serde(default)]
    pub gear: Gear,
    #[serde(default)]
    pub quests: HashMap<String, QuestSt>,
    #[serde(default)]
    pub quest_points: u32,
}

fn ten() -> i32 {
    10
}

/// Worn armour, by gear slot (item ids).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Gear {
    #[serde(default)]
    pub helmet: Option<String>,
    #[serde(default)]
    pub suit: Option<String>,
    #[serde(default)]
    pub shield: Option<String>,
    #[serde(default)]
    pub cape: Option<String>,
}

pub const GEAR_SLOTS: [&str; 4] = ["helmet", "suit", "shield", "cape"];

impl Gear {
    pub fn get(&self, slot: &str) -> Option<&String> {
        match slot {
            "helmet" => self.helmet.as_ref(),
            "suit" => self.suit.as_ref(),
            "shield" => self.shield.as_ref(),
            "cape" => self.cape.as_ref(),
            _ => None,
        }
    }
    pub fn slot_mut(&mut self, slot: &str) -> Option<&mut Option<String>> {
        match slot {
            "helmet" => Some(&mut self.helmet),
            "suit" => Some(&mut self.suit),
            "shield" => Some(&mut self.shield),
            "cape" => Some(&mut self.cape),
            _ => None,
        }
    }
    /// Total armour bonus (goes into the defence roll).
    pub fn bonus(&self) -> i32 {
        GEAR_SLOTS.iter().filter_map(|s| self.get(s)).filter_map(|i| armour(i)).map(|a| a.def).sum()
    }
}

/// One quest's progress: `s` "active" / "done", the current step and its progress.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QuestSt {
    pub s: String,
    #[serde(default)]
    pub step: u32,
    #[serde(default)]
    pub p: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CaveInfo {
    pub w: u32,
    pub h: u32,
    pub rle: String,
}

/// A Rift Surge update (`welcome.event` and the `event` message).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EventInfo {
    #[serde(default)]
    pub kind: String,
    pub phase: String,
    #[serde(default)]
    pub tx: i32,
    #[serde(default)]
    pub tz: i32,
    #[serde(default)]
    pub ends_in: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DialogOpt {
    pub id: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Accepted {
    #[serde(default)]
    pub me: bool,
    #[serde(default)]
    pub them: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlayerInfo {
    pub id: u32,
    pub name: String,
    pub tx: i32,
    pub tz: i32,
    /// a colonist bot, and its job: miner, surveyor, guard, medic, quartermaster
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NodeInfo {
    pub id: u32,
    pub kind: String,
    pub tx: i32,
    pub tz: i32,
    #[serde(default = "yes")]
    pub up: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NpcInfo {
    pub id: u32,
    pub kind: String,
    pub tx: i32,
    pub tz: i32,
    pub hp: i32,
    pub max: i32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dead: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum S2C {
    Welcome {
        id: u32,
        #[serde(default)]
        token: String,
        #[serde(default = "tick_ms")]
        tick_ms: u32,
        you: You,
        #[serde(default)]
        players: Vec<PlayerInfo>,
        #[serde(default)]
        nodes: Vec<NodeInfo>,
        #[serde(default)]
        npcs: Vec<NpcInfo>,
        /// "surface" or "cave"
        #[serde(default)]
        room: Option<String>,
        /// the Rift Surge going on right now, if any
        #[serde(default)]
        event: Option<EventInfo>,
        /// cave rooms: the wall map and the ladder tile
        #[serde(default)]
        cave: Option<CaveInfo>,
        #[serde(default)]
        exit: Option<[i32; 2]>,
        /// the server's tick count
        #[serde(default)]
        tick: u64,
    },
    /// p: [id, tx, tz, anim] players; m: [id, tx, tz, anim, hp] creatures
    Tick {
        n: u64,
        #[serde(default)]
        p: Vec<(u32, i32, i32, u8)>,
        #[serde(default)]
        m: Vec<(u32, i32, i32, u8, i32)>,
    },
    Join {
        id: u32,
        name: String,
        tx: i32,
        tz: i32,
        #[serde(default)]
        bot: bool,
        #[serde(default)]
        role: Option<String>,
    },
    Leave { id: u32 },
    Chat { id: u32, name: String, msg: String },
    Node { id: u32, up: bool },
    Xp { skill: String, xp: f64, gain: f64 },
    Level { skill: String, level: u32 },
    Inv { inv: Vec<Option<Slot>> },
    Equip { weapon: Option<String> },
    Hit { src: String, dst: String, dmg: i32 },
    /// a colonist medic healing someone
    Heal { src: String, dst: String, amt: i32 },
    Death { who: String },
    Hp { hp: i32, max: i32 },
    Msg { text: String },
    Err { text: String },
    /// weapon + armour; `bonus` = total armour bonus
    Gear {
        #[serde(default)]
        weapon: Option<String>,
        #[serde(default)]
        helmet: Option<String>,
        #[serde(default)]
        suit: Option<String>,
        #[serde(default)]
        shield: Option<String>,
        #[serde(default)]
        cape: Option<String>,
        #[serde(default)]
        bonus: i32,
    },
    /// reconnect to that room (the server closes this socket)
    Travel { room: String },
    Bank {
        items: Vec<Slot>,
        #[serde(default)]
        slots: u32,
    },
    TradeRequest { from: u32, name: String },
    Trade {
        stage: u8,
        with: u32,
        name: String,
        #[serde(default)]
        mine: Vec<Slot>,
        #[serde(default)]
        theirs: Vec<Slot>,
        #[serde(default)]
        accepted: Accepted,
    },
    TradeEnd {
        result: String,
        #[serde(default)]
        reason: String,
    },
    Dialog {
        npc: u32,
        name: String,
        #[serde(default)]
        role: Option<String>,
        text: String,
        #[serde(default)]
        quest: Option<String>,
        #[serde(default)]
        options: Vec<DialogOpt>,
    },
    DialogEnd,
    Quest {
        id: String,
        state: String,
        #[serde(default)]
        step: u32,
        #[serde(default)]
        progress: u32,
        #[serde(default)]
        goal: u32,
        #[serde(default)]
        qp: u32,
    },
    Event(EventInfo),
    NpcSpawn { npc: NpcInfo },
    NpcDespawn { id: u32 },
    #[serde(other)]
    Unknown,
}

fn tick_ms() -> u32 {
    600
}

pub mod anim {
    pub const IDLE: u8 = 0;
    pub const WALK: u8 = 1;
    pub const RUN: u8 = 2;
    pub const MINE: u8 = 3;
    pub const SURVEY: u8 = 4;
    pub const CRAFT: u8 = 5;
    pub const ATTACK: u8 = 6;
    pub const DEAD: u8 = 7;
}

/// "p:<id>" / "n:<id>" combat actor references.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Who {
    P(u32),
    N(u32),
}

impl Who {
    pub fn parse(s: &str) -> Option<Self> {
        let (k, id) = s.split_once(':')?;
        let id = id.parse().ok()?;
        match k {
            "p" => Some(Who::P(id)),
            "n" => Some(Who::N(id)),
            _ => None,
        }
    }
    pub fn fmt(self) -> String {
        match self {
            Who::P(i) => format!("p:{i}"),
            Who::N(i) => format!("n:{i}"),
        }
    }
}

// ------------------------------------------------------------------------------------ skills

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Skill {
    Mining,
    Survey,
    Fabrication,
    Ranged,
    Defence,
    Hitpoints,
}

pub const SKILLS: [Skill; 6] = [Skill::Mining, Skill::Survey, Skill::Fabrication, Skill::Ranged, Skill::Defence, Skill::Hitpoints];

impl Skill {
    pub fn key(self) -> &'static str {
        match self {
            Skill::Mining => "mining",
            Skill::Survey => "survey",
            Skill::Fabrication => "fabrication",
            Skill::Ranged => "ranged",
            Skill::Defence => "defence",
            Skill::Hitpoints => "hitpoints",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Skill::Mining => "Mining",
            Skill::Survey => "Survey",
            Skill::Fabrication => "Fabrication",
            Skill::Ranged => "Ranged",
            Skill::Defence => "Defence",
            Skill::Hitpoints => "Hitpoints",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        SKILLS.iter().copied().find(|k| k.key() == s)
    }
}

pub const MAX_LEVEL: u32 = 99;
pub const MAX_XP: f64 = 200_000_000.0;
/// Hitpoints starts at level 10
pub const HP_START_XP: f64 = 1154.0;

/// XP needed for `level` (the classic old-school curve: 83 xp for level 2, 13,034,431 for 99).
pub fn xp_for_level(level: u32) -> f64 {
    let mut pts = 0.0f64;
    for l in 1..level.min(MAX_LEVEL) {
        pts += (l as f64 + 300.0 * 2f64.powf(l as f64 / 7.0)).floor();
    }
    (pts / 4.0).floor()
}

pub fn level_for_xp(xp: f64) -> u32 {
    let mut lvl = 1;
    while lvl < MAX_LEVEL && xp >= xp_for_level(lvl + 1) {
        lvl += 1;
    }
    lvl
}

/// Old-school style combat level over ranged, defence and hitpoints (no prayer, no melee).
pub fn combat_level(ranged: u32, defence: u32, hitpoints: u32) -> u32 {
    let base = 0.25 * (defence + hitpoints) as f64;
    let rng = 0.325 * (ranged as f64 * 1.5).floor();
    (base + rng).floor() as u32
}

// ------------------------------------------------------------------------------------ items

pub fn item_name(id: &str) -> &str {
    match id {
        "regolith" => "Regolith",
        "ice" => "Water ice",
        "hematite" => "Hematite",
        "sample" => "Core sample",
        "brick" => "Regolith brick",
        "water" => "Water flask",
        "iron_plate" => "Iron plate",
        "drill_bit" => "Drill bit",
        "pistol" => "Blaster",
        "rifle" => "Pulse Rifle",
        "plasma_gun" => "Plasma Cannon",
        "medkit" => "Medkit",
        "chitin" => "Xeno Chitin",
        "void_crystal" => "Void Crystal",
        "ancient_relic" => "Ancient Relic",
        "xp_lamp" => "Insight Lamp",
        "chitin_helmet" => "Chitin Helmet",
        "chitin_suit" => "Chitin Suit",
        "iron_helmet" => "Iron Helmet",
        "iron_suit" => "Iron Suit",
        "void_shield" => "Void Shield",
        "relic_helmet" => "Relic Helm",
        "relic_suit" => "Relic Armour",
        "rift_blade" => "Rift Blade",
        "void_cape" => "Void Cape",
        other => other,
    }
}

pub fn item_examine(id: &str) -> &str {
    match id {
        "regolith" => "Loose Martian soil. Packs well.",
        "ice" => "Dirty water ice from just under the dust.",
        "hematite" => "Iron oxide. The reason the planet is red.",
        "sample" => "A rock core, labelled and sealed.",
        "brick" => "Sintered regolith. Habitat material.",
        "water" => "Melted, filtered, drinkable. Mostly.",
        "iron_plate" => "Smelted iron, rolled flat.",
        "drill_bit" => "A hardened core drill bit.",
        "pistol" => "A compact energy blaster. Standard colony issue.",
        "rifle" => "Long barrel, longer reach, pulses in threes.",
        "plasma_gun" => "Runs hot on void crystals.",
        "medkit" => "Heals 20 hitpoints.",
        "chitin" => "Shell plate from something that came through a rift.",
        "void_crystal" => "Cold to the touch. You can see stars inside it.",
        "ancient_relic" => "Carved by hands that weren't human. The glyphs match the ones on the rifts.",
        "xp_lamp" => "Rub it on a skill. Somebody else's memories are in there.",
        "chitin_helmet" => "Void chitin laced into a helmet. Armour +6 (Defence 1).",
        "chitin_suit" => "It smells faintly of the void. Armour +12 (Defence 5).",
        "iron_helmet" => "Hammered iron. Armour +14 (Defence 20).",
        "iron_suit" => "Riveted iron plates. Armour +28 (Defence 25).",
        "void_shield" => "The air around it shimmers. Armour +25 (Defence 40).",
        "relic_helmet" => "A helm the old Martians would recognise. Armour +24 (Defence 60).",
        "relic_suit" => "The glyphs glow when rifts are near. Armour +48 (Defence 60).",
        "rift_blade" => "A sliver of the void with an edge. Reach 2 (Ranged 50).",
        "void_cape" => "Proof you stood your ground in a Rift Surge.",
        _ => "Nothing interesting happens.",
    }
}

/// UI swatch colour (sRGB 0..1)
pub fn item_color(id: &str) -> [f32; 3] {
    match id {
        "regolith" => [0.62, 0.36, 0.22],
        "ice" => [0.78, 0.90, 0.98],
        "hematite" => [0.42, 0.20, 0.20],
        "sample" => [0.70, 0.62, 0.50],
        "brick" => [0.72, 0.42, 0.28],
        "water" => [0.35, 0.60, 0.95],
        "iron_plate" => [0.62, 0.64, 0.68],
        "drill_bit" => [0.85, 0.75, 0.35],
        "pistol" => [0.35, 0.55, 0.75],
        "rifle" => [0.45, 0.45, 0.60],
        "plasma_gun" => [0.30, 0.90, 0.80],
        "medkit" => [0.92, 0.92, 0.92],
        "chitin" => [0.50, 0.70, 0.62],
        "void_crystal" => [0.62, 0.35, 1.0],
        "ancient_relic" => [0.85, 0.70, 0.35],
        "xp_lamp" => [1.0, 0.85, 0.30],
        "chitin_helmet" | "chitin_suit" => [0.45, 0.62, 0.52],
        "iron_helmet" | "iron_suit" => [0.58, 0.60, 0.66],
        "void_shield" => [0.35, 0.80, 1.0],
        "relic_helmet" | "relic_suit" => [0.90, 0.68, 0.25],
        "rift_blade" => [0.75, 0.40, 1.0],
        "void_cape" => [0.45, 0.20, 0.75],
        _ => [0.5, 0.5, 0.5],
    }
}

// ------------------------------------------------------------------------------------ nodes

pub struct NodeDef {
    pub kind: &'static str,
    pub name: &'static str,
    pub verb: &'static str,
    pub skill: Skill,
    pub level: u32,
    pub item: &'static str,
    pub xp: f64,
    /// chance a successful gather depletes the node
    pub deplete: f64,
    pub respawn_ticks: u32,
    pub examine: &'static str,
}

pub const NODES: &[NodeDef] = &[
    NodeDef { kind: "regolith", name: "Regolith mound", verb: "Mine", skill: Skill::Mining, level: 1, item: "regolith", xp: 18.0, deplete: 0.15, respawn_ticks: 8, examine: "Fine dust and grit, piled by the wind." },
    NodeDef { kind: "ice", name: "Ice deposit", verb: "Mine", skill: Skill::Mining, level: 15, item: "ice", xp: 35.0, deplete: 0.25, respawn_ticks: 20, examine: "Buried water ice, exposed by a small impact." },
    NodeDef { kind: "hematite", name: "Hematite vein", verb: "Mine", skill: Skill::Mining, level: 30, item: "hematite", xp: 65.0, deplete: 0.35, respawn_ticks: 45, examine: "Grey-red iron oxide spherules in the rock." },
    NodeDef { kind: "outcrop", name: "Layered outcrop", verb: "Survey", skill: Skill::Survey, level: 1, item: "sample", xp: 25.0, deplete: 0.5, respawn_ticks: 25, examine: "Sedimentary layers. Billions of years in one cliff." },
    // lava-tube cave only
    NodeDef { kind: "void_vein", name: "Void vein", verb: "Mine", skill: Skill::Mining, level: 45, item: "void_crystal", xp: 90.0, deplete: 0.25, respawn_ticks: 60, examine: "Crystals growing out of the basalt, glowing from the inside." },
    NodeDef { kind: "fossil", name: "Fossil bed", verb: "Survey", skill: Skill::Survey, level: 40, item: "ancient_relic", xp: 80.0, deplete: 0.34, respawn_ticks: 80, examine: "Carved stone under the dust. Somebody lived down here once." },
];

pub fn node_def(kind: &str) -> Option<&'static NodeDef> {
    NODES.iter().find(|n| n.kind == kind)
}

pub fn node_name(kind: &str) -> &str {
    match kind {
        "fabricator" => "Fabricator",
        "rift" => "Void rift",
        "locker" => "Colony locker",
        "pit" => "Lava-tube pit",
        "ladder" => "Ladder",
        k => node_def(k).map_or(k, |d| d.name),
    }
}

/// Lockers, pits and ladders: walked up to and used (`act`), like gathering.
pub fn usable_node(kind: &str) -> bool {
    matches!(kind, "locker" | "pit" | "ladder")
}

/// Gathering attempts happen every this many ticks.
pub const GATHER_TICKS: u32 = 4;

/// Chance one attempt succeeds at `level` for a node needing `req` (a drill bit helps mining).
pub fn gather_chance(level: u32, req: u32, drill: bool) -> f64 {
    (0.3 + 0.015 * (level as f64 - req as f64) + if drill { 0.15 } else { 0.0 }).clamp(0.05, 0.95)
}

// ------------------------------------------------------------------------------------ recipes

pub struct Recipe {
    pub id: &'static str,
    pub level: u32,
    pub xp: f64,
    pub inputs: &'static [(&'static str, u32)],
    pub ticks: u32,
    /// a quest that must be done before you know how to make it
    pub quest: Option<&'static str>,
}

pub const RECIPES: &[Recipe] = &[
    Recipe { id: "brick", level: 1, xp: 12.0, inputs: &[("regolith", 2)], ticks: 3, quest: None },
    Recipe { id: "water", level: 10, xp: 22.0, inputs: &[("ice", 1)], ticks: 3, quest: None },
    Recipe { id: "medkit", level: 15, xp: 30.0, inputs: &[("water", 1), ("sample", 1)], ticks: 3, quest: None },
    Recipe { id: "iron_plate", level: 20, xp: 45.0, inputs: &[("hematite", 2)], ticks: 3, quest: None },
    Recipe { id: "pistol", level: 25, xp: 60.0, inputs: &[("iron_plate", 2)], ticks: 4, quest: None },
    Recipe { id: "drill_bit", level: 35, xp: 90.0, inputs: &[("sample", 1), ("iron_plate", 1)], ticks: 5, quest: None },
    Recipe { id: "rifle", level: 40, xp: 140.0, inputs: &[("iron_plate", 4), ("drill_bit", 1)], ticks: 5, quest: None },
    Recipe { id: "plasma_gun", level: 60, xp: 300.0, inputs: &[("iron_plate", 3), ("void_crystal", 5)], ticks: 6, quest: None },
    // armour
    Recipe { id: "chitin_helmet", level: 10, xp: 30.0, inputs: &[("chitin", 3)], ticks: 4, quest: None },
    Recipe { id: "chitin_suit", level: 15, xp: 45.0, inputs: &[("chitin", 5)], ticks: 4, quest: None },
    Recipe { id: "iron_helmet", level: 30, xp: 70.0, inputs: &[("iron_plate", 2), ("chitin", 1)], ticks: 4, quest: None },
    Recipe { id: "iron_suit", level: 35, xp: 110.0, inputs: &[("iron_plate", 4), ("chitin", 2)], ticks: 5, quest: None },
    Recipe { id: "void_shield", level: 50, xp: 180.0, inputs: &[("iron_plate", 2), ("void_crystal", 6)], ticks: 5, quest: None },
    Recipe { id: "relic_helmet", level: 70, xp: 260.0, inputs: &[("ancient_relic", 1), ("iron_plate", 2), ("void_crystal", 4)], ticks: 6, quest: Some("echoes") },
    Recipe { id: "relic_suit", level: 70, xp: 400.0, inputs: &[("ancient_relic", 2), ("iron_plate", 3), ("void_crystal", 6)], ticks: 6, quest: Some("echoes") },
];

// ------------------------------------------------------------------------------------ armour

pub struct ArmourDef {
    pub item: &'static str,
    pub slot: &'static str,
    /// defence level to wear it
    pub level: u32,
    /// armour bonus (added to the defence roll)
    pub def: i32,
}

pub const ARMOUR: &[ArmourDef] = &[
    ArmourDef { item: "chitin_helmet", slot: "helmet", level: 1, def: 6 },
    ArmourDef { item: "chitin_suit", slot: "suit", level: 5, def: 12 },
    ArmourDef { item: "iron_helmet", slot: "helmet", level: 20, def: 14 },
    ArmourDef { item: "iron_suit", slot: "suit", level: 25, def: 28 },
    ArmourDef { item: "void_shield", slot: "shield", level: 40, def: 25 },
    ArmourDef { item: "relic_helmet", slot: "helmet", level: 60, def: 24 },
    ArmourDef { item: "relic_suit", slot: "suit", level: 60, def: 48 },
    ArmourDef { item: "void_cape", slot: "cape", level: 1, def: 2 },
];

pub fn armour(item: &str) -> Option<&'static ArmourDef> {
    ARMOUR.iter().find(|a| a.item == item)
}

/// A player's defence roll against a creature: (defence + 9) * (armour bonus + 64).
pub fn player_def_roll(def_lvl: u32, bonus: i32) -> f64 {
    (def_lvl as f64 + 9.0) * (bonus as f64 + 64.0)
}

/// XP from rubbing an Insight Lamp on a skill at `level`.
pub fn lamp_xp(level: u32) -> f64 {
    100.0 + 25.0 * level as f64
}

// ------------------------------------------------------------------------------------ quests

pub struct QuestStep {
    /// kill (creature kind), gather (item), craft (recipe), deliver (item, taken at turn-in), enter (cave)
    pub kind: &'static str,
    pub target: &'static str,
    pub n: u32,
    pub text: &'static str,
}

pub struct QuestDef {
    pub id: &'static str,
    pub name: &'static str,
    /// colonist role that hands it out (the quartermaster when that role isn't around)
    pub giver: &'static str,
    pub qp: u32,
    pub req: &'static [(Skill, u32)],
    pub steps: &'static [QuestStep],
    pub offer: &'static str,
    pub thanks: &'static str,
    pub items: &'static [(&'static str, u32)],
    pub lamps: u32,
    pub reward: &'static str,
}

/// Colonist quests (the same list the server hands out).
pub const QUESTS: &[QuestDef] = &[
    QuestDef { id: "skitter_cull", name: "Pest Control", giver: "guard", qp: 1, req: &[], steps: &[QuestStep { kind: "kill", target: "skitter", n: 5, text: "Kill 5 Void Skitters" }], offer: "Skitters keep crawling out of the near rifts. Thin them out: five should do it.", thanks: "Nice shooting. The perimeter's quieter already.", items: &[("medkit", 1)], lamps: 1, reward: "1 Insight Lamp, 1 medkit" },
    QuestDef { id: "brick_drive", name: "Building Blocks", giver: "quartermaster", qp: 1, req: &[], steps: &[QuestStep { kind: "craft", target: "brick", n: 5, text: "Make 5 regolith bricks at the fabricator" }], offer: "The habitat needs bricks. Sinter five at the fabricator and you've earned your keep.", thanks: "Good, solid bricks. The colony grows.", items: &[("medkit", 1)], lamps: 1, reward: "1 Insight Lamp, 1 medkit" },
    QuestDef { id: "ice_run", name: "Thirsty Colony", giver: "miner", qp: 1, req: &[(Skill::Mining, 15)], steps: &[QuestStep { kind: "gather", target: "ice", n: 5, text: "Mine 5 chunks of buried ice" }], offer: "Water's running low. Dig up five chunks of ice for the tanks?", thanks: "That'll keep the taps running. Take this drill bit, it's spare.", items: &[("drill_bit", 1)], lamps: 1, reward: "1 Insight Lamp, 1 drill bit" },
    QuestDef { id: "core_samples", name: "Rock Solid", giver: "surveyor", qp: 1, req: &[], steps: &[QuestStep { kind: "deliver", target: "sample", n: 3, text: "Bring 3 core samples" }], offer: "I need core samples from the pale outcrops. Three of them. Something in the layers is glowing.", thanks: "These layers predate the rifts. Mostly. Thanks!", items: &[("medkit", 1)], lamps: 1, reward: "1 Insight Lamp, 1 medkit" },
    QuestDef { id: "field_hospital", name: "Field Hospital", giver: "medic", qp: 1, req: &[(Skill::Fabrication, 10)], steps: &[QuestStep { kind: "deliver", target: "water", n: 2, text: "Bring 2 water flasks" }], offer: "My med bay is dry. Two flasks of water, please: melt some ice at the fabricator.", thanks: "Perfect. Here, a couple of medkits for your trouble.", items: &[("medkit", 2)], lamps: 1, reward: "1 Insight Lamp, 2 medkits" },
    QuestDef { id: "dust_to_dust", name: "Dust to Dust", giver: "guard", qp: 2, req: &[(Skill::Ranged, 15)], steps: &[QuestStep { kind: "kill", target: "stalker", n: 3, text: "Kill 3 Dust Stalkers" }], offer: "Dust Stalkers are circling the colony. Put three of them down.", thanks: "That's three fewer pairs of eyes in the dust. Here, chitin armour from the last batch.", items: &[("chitin_suit", 1)], lamps: 2, reward: "2 Insight Lamps, a chitin suit" },
    QuestDef { id: "heavy_metal", name: "Heavy Metal", giver: "guard", qp: 2, req: &[(Skill::Ranged, 35)], steps: &[QuestStep { kind: "kill", target: "golem", n: 1, text: "Destroy a Rift Golem" }], offer: "A Rift Golem is pulling itself together out past the far rifts. Break it before it finishes.", thanks: "You actually did it. Take the crystals: it won't be needing them.", items: &[("void_crystal", 5)], lamps: 2, reward: "2 Insight Lamps, 5 void crystals" },
    QuestDef {
        id: "echoes",
        name: "Echoes Below",
        giver: "quartermaster",
        qp: 3,
        req: &[(Skill::Survey, 40)],
        steps: &[
            QuestStep { kind: "enter", target: "cave", n: 1, text: "Climb down the pit into the lava tube" },
            QuestStep { kind: "gather", target: "ancient_relic", n: 1, text: "Survey a fossil bed in the tube for an ancient relic" },
            QuestStep { kind: "deliver", target: "ancient_relic", n: 1, text: "Bring the ancient relic to the quartermaster" },
        ],
        offer: "The old Martians went underground, they say. There's a pit near the landing site. Go down and bring me proof.",
        thanks: "A real relic... the glyphs match the ones on the rifts. They didn't seal the void, they were hiding from it. Take these plans: relic armour.",
        items: &[],
        lamps: 3,
        reward: "3 Insight Lamps, relic armour plans",
    },
];

pub fn quest_def(id: &str) -> Option<&'static QuestDef> {
    QUESTS.iter().find(|q| q.id == id)
}

pub fn quest_points(qs: &HashMap<String, QuestSt>) -> u32 {
    QUESTS.iter().filter(|q| qs.get(q.id).is_some_and(|s| s.s == "done")).map(|q| q.qp).sum()
}

// (each recipe's level is at least the levels of the recipes making its inputs)
pub fn recipe(id: &str) -> Option<&'static Recipe> {
    RECIPES.iter().find(|r| r.id == id)
}

/// Crafting needs a fabricator within this many tiles (Chebyshev).
pub const FAB_RANGE: i32 = 1;

/// Items that stack in one slot.
pub fn stacks(item: &str) -> bool {
    item == "void_crystal"
}

pub fn count(inv: &[Option<Slot>], item: &str) -> u32 {
    inv.iter().flatten().filter(|s| s.item == item).map(|s| s.qty).sum()
}

// ------------------------------------------------------------------------------------ combat

pub struct Weapon {
    pub item: &'static str,
    pub level: u32,
    /// accuracy and strength bonuses
    pub acc: i32,
    pub str: i32,
    pub range: i32,
    pub speed: u32,
}

/// Bare hands: a 1-tile punch.
pub const FISTS: Weapon = Weapon { item: "", level: 1, acc: 10, str: 50, range: 1, speed: 4 };

pub const WEAPONS: &[Weapon] = &[
    Weapon { item: "pistol", level: 1, acc: 30, str: 60, range: 7, speed: 4 },
    Weapon { item: "rifle", level: 20, acc: 50, str: 100, range: 7, speed: 5 },
    Weapon { item: "plasma_gun", level: 40, acc: 80, str: 150, range: 7, speed: 6 },
    // Rift Surge unique
    Weapon { item: "rift_blade", level: 50, acc: 95, str: 170, range: 2, speed: 4 },
];

pub fn weapon(item: Option<&str>) -> &'static Weapon {
    item.and_then(|i| WEAPONS.iter().find(|w| w.item == i)).unwrap_or(&FISTS)
}

pub const MEDKIT_HEAL: i32 = 20;
/// ticks a medkit delays your next shot
pub const MEDKIT_DELAY: u32 = 2;
pub const HP_REGEN_TICKS: u64 = 10;

pub struct CreatureDef {
    pub kind: &'static str,
    pub name: &'static str,
    pub level: u32,
    pub hp: i32,
    pub attack: u32,
    pub defence: u32,
    pub max_hit: i32,
    pub speed: u32,
    pub range: i32,
    pub examine: &'static str,
}

/// Void creatures. Lore: ancient Martians sealed something beneath the planet; the rifts are
/// tearing open into the void and these pour out. Looks: look.rs `ENEMY_LOOKS`.
pub const CREATURES: &[CreatureDef] = &[
    CreatureDef { kind: "skitter", name: "Void Skitter", level: 5, hp: 5, attack: 1, defence: 1, max_hit: 1, speed: 4, range: 1, examine: "Six legs, no eyes you can find, and it glows." },
    CreatureDef { kind: "stalker", name: "Dust Stalker", level: 20, hp: 30, attack: 18, defence: 15, max_hit: 5, speed: 4, range: 1, examine: "Its hide matches the sand. Its eyes don't." },
    CreatureDef { kind: "golem", name: "Rift Golem", level: 45, hp: 70, attack: 40, defence: 38, max_hit: 10, speed: 6, range: 1, examine: "Rock that walks, held together by something from the void." },
    CreatureDef { kind: "leviathan", name: "Void Leviathan", level: 90, hp: 250, attack: 85, defence: 80, max_hit: 20, speed: 5, range: 4, examine: "It shouldn't be able to float here. It does anyway." },
    // lava-tube cave
    CreatureDef { kind: "glowworm", name: "Glowworm", level: 30, hp: 40, attack: 28, defence: 25, max_hit: 6, speed: 4, range: 1, examine: "A lot of worm, lit up like a runway." },
    CreatureDef { kind: "crystal_horror", name: "Crystal Horror", level: 60, hp: 90, attack: 58, defence: 55, max_hit: 13, speed: 5, range: 2, examine: "The void grew crystals in the dark, and the crystals grew legs." },
];

pub fn creature_def(kind: &str) -> Option<&'static CreatureDef> {
    CREATURES.iter().find(|d| d.kind == kind)
}

pub const AGGRO_TILES: i32 = 5;
pub const LEASH_TILES: i32 = 15;
pub const NPC_RESPAWN_TICKS: u64 = 50;

/// Max hit from the attacker's ranged level and weapon strength.
pub fn max_hit(ranged_lvl: u32, str_bonus: i32) -> i32 {
    (0.5 + (ranged_lvl as f64 + 8.0) * (str_bonus as f64 + 64.0) / 640.0).floor() as i32
}

/// Old-school accuracy from attack and defence rolls.
pub fn hit_chance(att_roll: f64, def_roll: f64) -> f64 {
    if att_roll > def_roll { 1.0 - (def_roll + 2.0) / (2.0 * (att_roll + 1.0)) } else { att_roll / (2.0 * (def_roll + 1.0)) }
}

pub fn player_att_roll(ranged_lvl: u32, acc: i32) -> f64 {
    (ranged_lvl as f64 + 8.0) * (acc as f64 + 64.0)
}

pub fn def_roll(def_lvl: u32) -> f64 {
    (def_lvl as f64 + 9.0) * 64.0
}

pub fn creature_att_roll(d: &CreatureDef) -> f64 {
    (d.attack as f64 + 8.0) * 64.0
}

pub const RANGED_XP_PER_DMG: f64 = 4.0;
pub const HP_XP_PER_DMG: f64 = 1.33;
pub const DEF_XP_PER_DMG: f64 = 1.0;

/// Creature drops: (item, chance) rolled independently; chitin always.
pub fn creature_drops(kind: &str) -> &'static [(&'static str, f64)] {
    match kind {
        "skitter" => &[("chitin", 1.0), ("void_crystal", 0.2), ("pistol", 1.0 / 25.0)],
        "stalker" => &[("chitin", 1.0), ("void_crystal", 0.5), ("pistol", 1.0 / 15.0), ("rifle", 1.0 / 80.0)],
        "golem" => &[("chitin", 1.0), ("void_crystal", 1.0), ("rifle", 1.0 / 30.0)],
        "leviathan" => &[("chitin", 1.0), ("void_crystal", 1.0), ("void_crystal", 1.0), ("void_crystal", 1.0), ("void_crystal", 1.0), ("void_crystal", 1.0), ("plasma_gun", 0.25)],
        "glowworm" => &[("chitin", 1.0), ("void_crystal", 0.4), ("rifle", 1.0 / 40.0)],
        "crystal_horror" => &[("chitin", 1.0), ("void_crystal", 1.0), ("void_crystal", 1.0), ("ancient_relic", 1.0 / 6.0), ("plasma_gun", 1.0 / 40.0)],
        _ => &[("chitin", 1.0)],
    }
}

pub fn chebyshev(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs())
}

pub fn sanitize_site(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_curve() {
        assert_eq!(xp_for_level(1), 0.0);
        assert_eq!(xp_for_level(2), 83.0);
        assert_eq!(xp_for_level(10), 1154.0);
        assert_eq!(xp_for_level(99), 13_034_431.0);
        assert_eq!(level_for_xp(HP_START_XP), 10);
        assert_eq!(level_for_xp(82.0), 1);
        assert_eq!(level_for_xp(MAX_XP), 99);
        assert_eq!(combat_level(1, 1, 10), 3);
        assert_eq!(combat_level(99, 99, 99), 97);
    }

    #[test]
    fn json() {
        let m: S2C = serde_json::from_str(r#"{"t":"tick","n":4,"p":[[1,2,3,1]],"extra":5}"#).unwrap();
        assert_eq!(m, S2C::Tick { n: 4, p: vec![(1, 2, 3, 1)], m: vec![] });
        let u: S2C = serde_json::from_str(r#"{"t":"pong","x":1}"#).unwrap();
        assert_eq!(u, S2C::Unknown);
        let w: S2C = serde_json::from_str(r#"{"t":"welcome","id":3,"token":"x","tick_ms":600,"you":{"tx":1,"tz":2,"skills":{"mining":10},"inv":[null,{"item":"ice","qty":1}]},"players":[],"nodes":[{"id":1,"kind":"ice","tx":0,"tz":0,"up":true}]}"#).unwrap();
        let S2C::Welcome { you, .. } = w else { panic!() };
        assert_eq!(you.skills.hitpoints, HP_START_XP);
        assert_eq!(you.hp, 10);
        assert_eq!(serde_json::to_string(&C2S::Ping).unwrap(), r#"{"t":"ping"}"#);
        assert_eq!(serde_json::to_string(&C2S::Unequip { gear: None }).unwrap(), r#"{"t":"unequip"}"#);
        assert_eq!(serde_json::to_string(&C2S::BankDeposit { slot: 2, qty: Qty::All }).unwrap(), r#"{"t":"bank_deposit","slot":2,"qty":"all"}"#);
        assert_eq!(serde_json::to_string(&C2S::TradeAccept).unwrap(), r#"{"t":"trade_accept"}"#);
        let e: S2C = serde_json::from_str(r#"{"t":"event","kind":"surge","phase":"warning","tx":3,"tz":4,"ends_in":200}"#).unwrap();
        assert!(matches!(e, S2C::Event(EventInfo { ref phase, tx: 3, .. }) if phase == "warning"));
        let d: S2C = serde_json::from_str(r#"{"t":"dialog_end"}"#).unwrap();
        assert_eq!(d, S2C::DialogEnd);
        let t: S2C = serde_json::from_str(r#"{"t":"trade","stage":1,"with":4,"name":"Bea","mine":[{"item":"brick","qty":2}],"theirs":[],"accepted":{"me":true,"them":false}}"#).unwrap();
        assert!(matches!(t, S2C::Trade { stage: 1, .. }));
        assert_eq!(serde_json::to_string(&C2S::Walk { path: vec![[1, 2]], run: true }).unwrap(), r#"{"t":"walk","path":[[1,2]],"run":true}"#);
        assert_eq!(Who::parse("n:12"), Some(Who::N(12)));
    }
}
