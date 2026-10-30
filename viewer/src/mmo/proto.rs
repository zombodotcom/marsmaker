//! MarsScape wire protocol (JSON text frames over a WebSocket) and the game rules both the
//! offline simulation (sim.rs) and the server (server/) use: skills, the XP curve, nodes,
//! recipes, weapons, void creatures and the combat formulas. Unknown message types and fields are ignored.

use serde::{Deserialize, Serialize};

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
    Act { node: u32 },
    Craft { recipe: String },
    Drop { slot: usize },
    Chat { msg: String },
    Equip { slot: usize },
    Unequip,
    Attack { npc: u32 },
    Eat { slot: usize },
    Ping,
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
}

fn ten() -> i32 {
    10
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
];

pub fn node_def(kind: &str) -> Option<&'static NodeDef> {
    NODES.iter().find(|n| n.kind == kind)
}

pub fn node_name(kind: &str) -> &str {
    match kind {
        "fabricator" => "Fabricator",
        "rift" => "Void rift",
        k => node_def(k).map_or(k, |d| d.name),
    }
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
}

pub const RECIPES: &[Recipe] = &[
    Recipe { id: "brick", level: 1, xp: 12.0, inputs: &[("regolith", 2)], ticks: 3 },
    Recipe { id: "water", level: 10, xp: 22.0, inputs: &[("ice", 1)], ticks: 3 },
    Recipe { id: "medkit", level: 15, xp: 30.0, inputs: &[("water", 1), ("sample", 1)], ticks: 3 },
    Recipe { id: "iron_plate", level: 20, xp: 45.0, inputs: &[("hematite", 2)], ticks: 3 },
    Recipe { id: "pistol", level: 25, xp: 60.0, inputs: &[("iron_plate", 2)], ticks: 4 },
    Recipe { id: "drill_bit", level: 35, xp: 90.0, inputs: &[("sample", 1), ("iron_plate", 1)], ticks: 5 },
    Recipe { id: "rifle", level: 40, xp: 140.0, inputs: &[("iron_plate", 4), ("drill_bit", 1)], ticks: 5 },
    Recipe { id: "plasma_gun", level: 60, xp: 300.0, inputs: &[("iron_plate", 3), ("void_crystal", 5)], ticks: 6 },
];

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
        assert_eq!(serde_json::to_string(&C2S::Unequip).unwrap(), r#"{"t":"unequip"}"#);
        assert_eq!(serde_json::to_string(&C2S::Walk { path: vec![[1, 2]], run: true }).unwrap(), r#"{"t":"walk","path":[[1,2]],"run":true}"#);
        assert_eq!(Who::parse("n:12"), Some(Who::N(12)));
    }
}
