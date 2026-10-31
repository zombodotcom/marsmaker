//! MARSSCAPE: an old-school, tick-based online RPG mode on the real terrain. Walk the 2 m tile
//! grid by clicking, mine regolith / ice / hematite, survey outcrops, make things at fabricators,
//! fight the void creatures pouring out of the rifts with blasters, level six skills on the classic XP curve, chat.
//!
//! The world is authoritative on the server (server/, protocol in proto.rs). With no server
//! configured (`--mmo-server URL` / `--mmo URL` / browser `?mmo=wss://...`) or reachable, the same
//! rules run in-process (sim.rs) with a few bot astronauts, so the mode is always playable.
//! The client only predicts paths (A*, grid.rs) and interpolates between the 600 ms ticks.
//!
//! Also: the lava tube under each site (a pit on the surface, its own room underground, cave.rs),
//! the colony locker (bank), trading between players, colonist quests with Insight Lamps, armour,
//! and Rift Surges (a big rift opens, waves pour out, then a Void Leviathan).
//!
//! Started from the menu's MARSSCAPE card (or `--mode scape`); the buggy is not spawned.
//! Test hook: `--mmo-script "wait:2,walk:6,3,act:regolith@8,tab:skills,chat:hi,shot:a.png,quit"`
//! (see `script`), `--mmo-name NAME` skips the name prompt.

mod cave;
mod grid;
mod look;
mod net;
mod pick;
pub mod proto;
mod sim;
mod store;
mod ui;

use crate::menu::{Menu, SiteUnloaded};
use crate::terrain::Site;
use crate::{Args, LoadFailed, Loading};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::picking::hover::HoverMap;
use bevy::picking::pointer::PointerId;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::window::PrimaryWindow;
use cave::Gnd;
use grid::{WalkGrid, tile_of};
use look::{Ctx, Kit};
use net::{NetEvent, Ws};
use proto::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use ui::{CtxMenu, Opt, UiAct, UiMsg, UiState};

/// Online server used when none is given on the command line / URL (`--mmo-server offline` or
/// `?mmo=offline` forces the offline world). Unreachable = offline world.
pub const DEFAULT_SERVER: &str = "wss://marsscape.tommysciano.workers.dev";

/// Command-line / URL options for this mode.
#[derive(Clone, Default)]
pub struct MmoArgs {
    pub server: String,
    pub name: Option<String>,
    pub script: Option<String>,
}

/// Where the game's attention is (the terrain's full-resolution window follows it).
#[derive(Resource)]
pub struct Focus(pub Vec3);

pub struct MmoPlugin;

impl Plugin for MmoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Kit>()
            .init_resource::<UiState>()
            .init_resource::<pick::PickRects>()
            .init_resource::<pick::ClickTest>()
            .add_message::<UiMsg>()
            // runs in every mode: leaving MARSSCAPE for a driving mode must tear it down too
            .add_systems(Update, unload.before(start))
            // lights / sky / terrain for the lava tube, restored on the way out (any mode)
            .add_systems(Update, cave_env.after(unload))
            .add_systems(
                Update,
                (
                    start,
                    name_entry,
                    network,
                    script,
                    keys,
                    pick::rects,
                    pick::click_test,
                    world_input,
                    pick::hover_ring,
                    ui_actions,
                    move_actors,
                    dress_me,
                    animate,
                    orbit_camera,
                    effects,
                    ui::refresh,
                    ui::overlays,
                    ui::minimap,
                )
                    .chain()
                    .run_if(|a: Res<Args>| a.scape),
            );
    }
}

// ------------------------------------------------------------------------------------ state

enum Link {
    Offline(Box<sim::Sim>),
    Online { ws: Ws, open: bool, ever: bool, tries: u32, ping_at: f32, since: f32 },
    Retry { at: f32, tries: u32 },
}

pub struct Actor {
    pub name: String,
    /// colonist bot's job (name tag shows it)
    pub role: Option<String>,
    pub npc: bool,
    pub kind: String,
    pub level: u32,
    pub tile: IVec2,
    from: Vec3,
    to: Vec3,
    t: f32,
    pub pos: Vec3,
    pub anim: u8,
    pub e: Entity,
    yaw: f32,
    face: Option<Vec3>,
    pub hp: i32,
    pub max: i32,
    /// seconds since this actor was last in combat (health bar shows for a while)
    pub combat_t: f32,
    pub height: f32,
}

pub struct NodeS {
    pub kind: String,
    pub tile: IVec2,
    pub up: bool,
    pub e: Entity,
    pub pos: Vec3,
}

pub struct Splat {
    pub who: Who,
    pub dmg: i32,
    /// a heal (green) rather than damage
    pub heal: bool,
    pub t: f32,
    pub k: u32,
}

pub struct XpDrop {
    pub text: String,
    pub t: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Intent {
    None,
    Fab(u32),
    /// walking over to ask this player to trade
    Trade(u32),
}

/// The trade window as the server last described it.
pub struct TradeView {
    pub stage: u8,
    pub name: String,
    pub mine: Vec<Slot>,
    pub theirs: Vec<Slot>,
    pub me_ok: bool,
    pub them_ok: bool,
}

/// A colonist talking to you.
pub struct DialogView {
    pub name: String,
    pub role: Option<String>,
    pub text: String,
    pub options: Vec<DialogOpt>,
}

/// The Rift Surge going on: phase ("warning" / "open" / "boss"), where, when the phase ends.
pub struct SurgeView {
    pub phase: String,
    pub at: IVec2,
    pub ends_tick: u64,
}

#[derive(Resource)]
pub struct Scape {
    link: Link,
    url: String,
    room: String,
    /// remember the login token (off for `--mmo-name` test runs, so they never touch the profile)
    persist: bool,
    pub grid: WalkGrid,
    spawn: IVec2,
    pub me: Option<u32>,
    pub name: String,
    pub players: HashMap<u32, Actor>,
    pub npcs: HashMap<u32, Actor>,
    pub nodes: HashMap<u32, NodeS>,
    pub skills: Skills,
    pub inv: Vec<Option<Slot>>,
    pub weapon: Option<String>,
    pub hp: i32,
    pub max_hp: i32,
    pub run: bool,
    tick_s: f32,
    pub tick_n: u64,
    pub chat: VecDeque<(String, Color)>,
    pub splats: Vec<Splat>,
    pub drops: Vec<XpDrop>,
    pub status: String,
    intent: Intent,
    marker: Option<(Vec3, bool)>,
    pending_shots: Vec<(Vec3, Vec3, bool, bool)>,
    splat_k: u32,
    /// UI needs a rebuild (inventory / skills / gear changed)
    pub dirty: bool,
    pub chat_dirty: bool,
    /// the lava tube we're in (None = on the surface) and its geometry
    pub cave: Option<Arc<cave::CaveMap>>,
    cave_root: Option<Entity>,
    /// the surface site's room id (its cave is "<site_room>~cave")
    site_room: String,
    /// login token from the last welcome (kept in memory for travel, even when it isn't stored)
    token: String,
    pub gear: Gear,
    /// total armour bonus
    pub armour: i32,
    /// my astronaut needs dressing in `gear`
    gear_dirty: bool,
    pub quests: HashMap<String, QuestSt>,
    pub qp: u32,
    /// the colony locker is open: its contents
    pub bank: Option<Vec<Slot>>,
    pub trade: Option<TradeView>,
    /// someone asked to trade: (id, name)
    pub trade_req: Option<(u32, String)>,
    pub dialog: Option<DialogView>,
    pub surge: Option<SurgeView>,
    /// rubbing the Insight Lamp in this slot: pick a skill
    pub lamp: Option<usize>,
    /// frame the camera for a new place (after travel)
    cam_reset: bool,
    greeted: bool,
}

pub const GAME: Color = Color::srgb(1.0, 0.92, 0.55);
pub const SAY: Color = Color::srgb(0.65, 0.85, 1.0);
pub const WARN: Color = Color::srgb(1.0, 0.45, 0.35);
pub const TRADE: Color = Color::srgb(0.95, 0.55, 1.0);

impl Scape {
    pub fn say(&mut self, text: impl Into<String>, c: Color) {
        self.chat.push_back((text.into(), c));
        while self.chat.len() > 60 {
            self.chat.pop_front();
        }
        self.chat_dirty = true;
    }

    fn send(&mut self, m: C2S) {
        match &mut self.link {
            Link::Offline(sim) => sim.handle(m, &mut self.grid),
            Link::Online { ws, open: true, .. } => ws.send(&m),
            _ => {}
        }
    }

    pub fn online(&self) -> bool {
        matches!(self.link, Link::Online { open: true, .. })
    }

    pub fn my_tile(&self) -> Option<IVec2> {
        self.me.and_then(|m| self.players.get(&m)).map(|a| a.tile)
    }

    pub fn my_pos(&self) -> Option<Vec3> {
        self.me.and_then(|m| self.players.get(&m)).map(|a| a.pos)
    }

    /// The ground under us: the site's terrain, or the lava tube.
    pub fn gnd<'a>(&'a self, site: &'a Site) -> Gnd<'a> {
        match self.cave.as_deref() {
            Some(c) => Gnd::Cave(c),
            None => Gnd::Site(site),
        }
    }

    /// Tokens are per site; the site's cave uses the same one.
    fn token_key(&self) -> String {
        format!("{}/{}", self.url, self.site_room)
    }

    fn hello(&mut self) {
        let token = if !self.token.is_empty() {
            self.token.clone()
        } else if self.persist {
            store::token(&self.token_key())
        } else {
            String::new()
        };
        let m = C2S::Hello { ver: VER, name: self.name.clone(), token, bounds: [self.grid.min.x, self.grid.min.y, self.grid.max.x, self.grid.max.y], spawn: [self.spawn.x, self.spawn.y] };
        self.send(m);
    }

    /// Walk to `t` (or as close as the path finder gets). Returns the path's last tile.
    pub fn walk_to(&mut self, t: IVec2, run: bool) -> Option<IVec2> {
        let from = self.my_tile()?;
        let path = self.grid.find_path(from, t, |x| x == t);
        self.intent = Intent::None;
        self.dialog = None;
        let end = path.last().copied();
        self.send(C2S::Walk { path: path.iter().map(|p| [p.x, p.y]).collect(), run: run || self.run });
        if let Some(e) = end {
            if let Some(w) = self.grid.world(e) {
                self.marker = Some((w, false));
            }
        }
        end
    }

    /// Walk next to a node / creature, then send `then`.
    fn approach(&mut self, at: IVec2, reach: i32, then: Option<C2S>) {
        let Some(from) = self.my_tile() else { return };
        let path = self.grid.find_path(from, at, |x| {
            let d = chebyshev((x.x, x.y), (at.x, at.y));
            d >= 1.min(reach) && d <= reach
        });
        if !path.is_empty() {
            self.send(C2S::Walk { path: path.iter().map(|p| [p.x, p.y]).collect(), run: self.run });
        }
        if let Some(w) = self.grid.world(at) {
            self.marker = Some((w, true));
        }
        if let Some(m) = then {
            self.send(m);
        }
    }

    pub fn act_node(&mut self, id: u32) {
        let Some(n) = self.nodes.get(&id) else { return };
        let (kind, tile) = (n.kind.clone(), n.tile);
        match kind.as_str() {
            "fabricator" => {
                self.approach(tile, 1, None);
                self.intent = Intent::Fab(id);
            }
            "rift" => self.say("A tear into the void. Stars where the ground should be, and things crawling out.", GAME),
            _ => {
                self.approach(tile, 1, Some(C2S::Act { node: id }));
                self.intent = Intent::None;
            }
        }
    }

    pub fn attack(&mut self, id: u32) {
        let Some(n) = self.npcs.get(&id) else { return };
        if n.anim == anim::DEAD || n.hp <= 0 {
            return;
        }
        self.intent = Intent::None;
        // the server walks us into range; the cross marks the target
        if let Some(w) = self.grid.world(n.tile) {
            self.marker = Some((w, true));
        }
        self.send(C2S::Attack { npc: id });
    }

    pub fn craft(&mut self, id: &str) {
        self.send(C2S::Craft { recipe: id.into() });
    }

    pub fn item(&mut self, slot: usize, verb: &str) {
        let Some(item) = self.inv.get(slot).cloned().flatten() else { return };
        match verb {
            "drop" => self.send(C2S::Drop { slot }),
            "equip" | "wear" => self.send(C2S::Equip { slot }),
            "rub" => {
                self.lamp = Some(slot);
                self.dirty = true;
            }
            "eat" => self.send(C2S::Eat { slot }),
            _ => self.say(item_examine(&item.item).to_string(), GAME),
        }
    }

    pub fn chat_send(&mut self, msg: &str) {
        let msg: String = msg.chars().take(CHAT_MAX).collect();
        if !msg.trim().is_empty() {
            self.send(C2S::Chat { msg });
        }
    }

    pub fn unequip(&mut self) {
        self.send(C2S::Unequip { gear: None });
    }

    pub fn unequip_gear(&mut self, slot: &str) {
        self.send(C2S::Unequip { gear: Some(slot.to_string()) });
    }

    /// Talk to a colonist (the server walks you over).
    pub fn talk(&mut self, id: u32) {
        let Some(a) = self.players.get(&id) else { return };
        let t = a.tile;
        self.intent = Intent::None;
        self.dialog = None;
        if let Some(w) = self.grid.world(t) {
            self.marker = Some((w, true));
        }
        self.send(C2S::Talk { id });
    }

    /// Ask a player to trade (walking closer first if they're far).
    pub fn trade_with(&mut self, id: u32) {
        let (Some(a), Some(me)) = (self.players.get(&id), self.my_tile()) else { return };
        let t = a.tile;
        if chebyshev((t.x, t.y), (me.x, me.y)) <= 8 {
            self.send(C2S::TradeRequest { id });
            self.intent = Intent::None;
        } else {
            self.approach(t, 2, None);
            self.intent = Intent::Trade(id);
        }
    }

    pub fn choose(&mut self, option: &str) {
        self.dialog = None;
        self.dirty = true;
        self.send(C2S::DialogChoose { option: option.to_string() });
    }

    pub fn rub(&mut self, skill: Skill) {
        if let Some(slot) = self.lamp.take() {
            self.send(C2S::Rub { slot, skill: skill.key().into() });
        }
        self.dirty = true;
    }

    /// A node of this kind within reach (1 tile).
    pub fn near_kind(&self, kind: &str) -> Option<u32> {
        let me = self.my_tile()?;
        self.nodes.iter().find(|(_, n)| n.kind == kind && chebyshev((n.tile.x, n.tile.y), (me.x, me.y)) <= 1).map(|(i, _)| *i)
    }

    pub fn bank_deposit(&mut self, slot: usize, qty: Qty) {
        self.send(C2S::BankDeposit { slot, qty });
    }

    pub fn bank_withdraw(&mut self, idx: usize, qty: Qty) {
        self.send(C2S::BankWithdraw { idx, qty });
    }

    pub fn trade_offer(&mut self, slot: usize, qty: Qty) {
        self.send(C2S::TradeOffer { slot, qty });
    }

    pub fn trade_remove(&mut self, idx: usize) {
        self.send(C2S::TradeRemove { idx, qty: Qty::All });
    }

    pub fn trade_accept(&mut self) {
        self.send(C2S::TradeAccept);
    }

    pub fn trade_decline(&mut self) {
        self.send(C2S::TradeDecline);
        self.trade = None;
        self.trade_req = None;
        self.dirty = true;
    }

    pub fn fab_near(&self) -> Option<u32> {
        let me = self.my_tile()?;
        self.nodes.iter().find(|(_, n)| n.kind == "fabricator" && chebyshev((n.tile.x, n.tile.y), (me.x, me.y)) <= FAB_RANGE).map(|(i, _)| *i)
    }
}

// ------------------------------------------------------------------------------------ lifecycle

fn unload(
    mut commands: Commands,
    mut ev: MessageReader<SiteUnloaded>,
    args: Res<Args>,
    scape: Option<Res<Scape>>,
    ui_roots: Query<Entity, Or<(With<ui::ScapeUi>, With<look::HoverRing>)>>,
    mut st: ResMut<UiState>,
) {
    let unloaded = ev.read().count() > 0;
    let switched_off = !args.scape && (scape.is_some() || !ui_roots.is_empty());
    if !unloaded && !switched_off {
        return;
    }
    commands.remove_resource::<Scape>();
    commands.remove_resource::<Focus>();
    commands.remove_resource::<Orbit>();
    commands.remove_resource::<ScriptRun>();
    for e in &ui_roots {
        commands.entity(e).despawn();
    }
    *st = UiState::default();
}

#[derive(Resource)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    dist: f32,
    look: Option<Vec3>,
}

#[allow(clippy::too_many_arguments)]
fn start(
    mut commands: Commands,
    args: Res<Args>,
    site: Option<Res<Site>>,
    scape: Option<Res<Scape>>,
    loading: Option<Res<Loading>>,
    failed: Option<Res<LoadFailed>>,
    mut st: ResMut<UiState>,
    mut images: ResMut<Assets<Image>>,
) {
    if scape.is_some() || loading.is_some() || failed.is_some() || st.name_entry.is_some() {
        return;
    }
    let Some(site) = site else { return };
    let name = args.mmo.name.clone().or_else(store::name).filter(|n| valid_name(n));
    let Some(name) = name else {
        st.name_entry = Some(String::new());
        ui::spawn_name_entry(&mut commands);
        return;
    };
    let mut grid = WalkGrid::new(&site);
    let s = site.spawn;
    let spawn = grid.nearest_walkable(tile_of(s.x, s.z), 20).unwrap_or(tile_of(s.x, s.z));
    let room = match &args.live {
        Some(id) => id.clone(),
        None => site.name.clone(),
    };
    let url = match args.mmo.server.as_str() {
        "" => DEFAULT_SERVER.to_string(),
        "offline" | "none" => String::new(),
        u => u.to_string(),
    };
    let link = if url.is_empty() {
        Link::Offline(Box::new(sim::Sim::new(&mut grid, &room, spawn)))
    } else {
        Link::Online { ws: Ws::connect(&net::room_url(&url, &room)), open: false, ever: false, tries: 0, ping_at: 0.0, since: -1.0 }
    };
    let offline = matches!(link, Link::Offline(_));
    let room_id = room.clone();
    let mut sc = Scape {
        link,
        url: url.clone(),
        room,
        persist: args.mmo.name.is_none(),
        grid,
        spawn,
        me: None,
        name,
        players: HashMap::new(),
        npcs: HashMap::new(),
        nodes: HashMap::new(),
        skills: Skills::fresh(),
        inv: vec![None; INV_SLOTS],
        weapon: None,
        hp: 10,
        max_hp: 10,
        run: false,
        tick_s: 0.6,
        tick_n: 0,
        chat: VecDeque::new(),
        splats: Vec::new(),
        drops: Vec::new(),
        status: if offline { "offline world".into() } else { format!("connecting to {url} ...") },
        intent: Intent::None,
        marker: None,
        pending_shots: Vec::new(),
        splat_k: 0,
        dirty: true,
        chat_dirty: true,
        cave: None,
        cave_root: None,
        site_room: room_id,
        token: String::new(),
        gear: Gear::default(),
        armour: 0,
        gear_dirty: false,
        quests: HashMap::new(),
        qp: 0,
        bank: None,
        trade: None,
        trade_req: None,
        dialog: None,
        surge: None,
        lamp: None,
        cam_reset: false,
        greeted: false,
    };
    if offline {
        sc.hello();
    }
    info!("marsscape: {} as {} in room {} (spawn tile {:?})", if offline { "offline" } else { "online" }, sc.name, sc.room, spawn);
    commands.insert_resource(sc);
    commands.insert_resource(Orbit { yaw: 0.6, pitch: 0.62, dist: 15.0, look: None });
    commands.insert_resource(Focus(site.spawn));
    if let Some(s) = &args.mmo.script {
        commands.insert_resource(ScriptRun::parse(s));
    }
    ui::spawn(&mut commands, &mut images);
    *st = UiState::default();
}

fn valid_name(n: &str) -> bool {
    let n = n.trim();
    !n.is_empty() && n.len() <= 12 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-')
}

/// First-play name prompt: type, Enter.
fn name_entry(
    mut commands: Commands,
    mut st: ResMut<UiState>,
    mut kb: MessageReader<bevy::input::keyboard::KeyboardInput>,
    menu: Option<Res<Menu>>,
    mut text: Query<&mut Text, With<ui::NameText>>,
    roots: Query<Entity, With<ui::NamePrompt>>,
) {
    let Some(mut name) = st.name_entry.clone() else {
        kb.clear();
        return;
    };
    if menu.is_some_and(|m| m.open) {
        return;
    }
    let mut done = false;
    for ev in kb.read().filter(|e| e.state.is_pressed()) {
        use bevy::input::keyboard::Key;
        match &ev.logical_key {
            Key::Backspace => {
                name.pop();
            }
            Key::Enter => done = valid_name(&name),
            _ => {
                if let Some(t) = &ev.text {
                    for c in t.chars() {
                        if name.len() < 12 && (c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-') {
                            name.push(c);
                        }
                    }
                }
            }
        }
    }
    if let Ok(mut t) = text.single_mut() {
        let s = format!("{name}_");
        if t.0 != s {
            t.0 = s;
        }
    }
    if done {
        let name = name.trim().to_string();
        store::set_name(&name);
        info!("marsscape: name set to {name}");
        for e in &roots {
            commands.entity(e).despawn();
        }
        st.name_entry = None;
        // `start` picks the stored name up next frame
    } else {
        st.name_entry = Some(name);
    }
}

// ------------------------------------------------------------------------------------ network

#[allow(clippy::too_many_arguments)]
fn network(
    mut commands: Commands,
    scape: Option<ResMut<Scape>>,
    site: Option<Res<Site>>,
    time: Res<Time>,
    real: Res<Time<Real>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut kit: ResMut<Kit>,
    mut st: ResMut<UiState>,
) {
    let (Some(mut sc), Some(site)) = (scape, site) else { return };
    let now = real.elapsed_secs();
    let mut msgs = Vec::new();
    // a `travel` makes the server close this socket: that's not a lost connection
    let mut traveling = false;
    let mut go_offline: Option<String> = None;
    let sc_ref = &mut *sc;
    match &mut sc_ref.link {
        Link::Offline(sim) => msgs = sim.update(time.delta_secs(), &mut sc_ref.grid),
        Link::Online { ws, open, ever, tries, ping_at, since } => {
            let mut closed = None;
            if *since < 0.0 {
                *since = now;
            }
            if !*open && now - *since > 8.0 {
                closed = Some("no answer in 8 s".to_string());
            }
            for ev in ws.poll() {
                match ev {
                    NetEvent::Open => {
                        *open = true;
                        *ever = true;
                        *tries = 0;
                        *ping_at = now + 20.0;
                    }
                    NetEvent::Msg(m) => {
                        traveling |= matches!(m, S2C::Travel { .. });
                        msgs.push(m);
                    }
                    NetEvent::Closed(why) => closed = Some(why),
                }
            }
            if *open && now > *ping_at {
                *ping_at = now + 20.0;
                ws.send(&C2S::Ping);
            }
            if traveling {
                // handled by `apply` (reconnects to the new room)
            } else if let Some(why) = closed {
                if !*ever {
                    go_offline = Some(why);
                } else {
                    let tries = *tries + 1;
                    let wait = (1u32 << tries.min(5)).min(30) as f32;
                    warn!("marsscape: connection lost ({why}); retrying in {wait} s");
                    sc_ref.status = format!("connection lost - reconnecting in {wait:.0} s");
                    sc_ref.link = Link::Retry { at: now + wait, tries };
                    sc_ref.say(format!("Connection lost. Reconnecting in {wait:.0} s..."), WARN);
                }
            } else if matches!(sc_ref.link, Link::Online { open: true, .. }) && sc_ref.me.is_none() && !st.hello_sent {
                st.hello_sent = true;
                sc_ref.hello();
            }
        }
        Link::Retry { at, tries } => {
            if now >= *at {
                let tries = *tries;
                sc_ref.link = Link::Online { ws: Ws::connect(&net::room_url(&sc_ref.url, &sc_ref.room)), open: false, ever: true, tries, ping_at: 0.0, since: now };
                st.hello_sent = false;
                sc_ref.me = None;
            }
        }
    }
    if let Some(why) = go_offline {
        warn!("marsscape: server {} unreachable ({why}); playing offline", sc.url);
        let (room, spawn) = (sc.room.clone(), sc.spawn);
        let sim = sim::Sim::new(&mut sc.grid, &room, spawn);
        sc.link = Link::Offline(Box::new(sim));
        sc.status = "offline world (server unreachable)".into();
        sc.say("The server can't be reached, so you're in an offline world.", WARN);
        sc.hello();
    }
    if msgs.is_empty() {
        return;
    }
    let mut cx = Ctx { commands: &mut commands, meshes: &mut meshes, mats: &mut mats, kit: &mut kit };
    for m in msgs {
        apply(&mut sc, &mut cx, m, &mut st, &site);
    }
}

/// `travel`: the server moved us to another room (down the pit / up the ladder). Online, connect
/// there with the same token; offline, the world switches over and sends a new welcome.
fn travel(sc: &mut Scape, cx: &mut Ctx, st: &mut UiState, site: &Site, room: String) {
    let down = room.ends_with("~cave");
    sc.say(if down { "You climb down into the lava tube..." } else { "You climb back up towards the daylight..." }, GAME);
    clear_world(sc, cx);
    sc.me = None;
    sc.room = room.clone();
    sc.bank = None;
    sc.trade = None;
    sc.trade_req = None;
    sc.dialog = None;
    sc.lamp = None;
    sc.intent = Intent::None;
    st.fab_open = false;
    st.ctx = None;
    sc.dirty = true;
    let mut out = Vec::new();
    match &mut sc.link {
        Link::Offline(sim) => {
            sim.travel(&room);
            out = sim.take_out();
        }
        _ => {
            sc.link = Link::Online { ws: Ws::connect(&net::room_url(&sc.url, &room)), open: false, ever: true, tries: 0, ping_at: 0.0, since: -1.0 };
            st.hello_sent = false;
        }
    }
    for m in out {
        apply(sc, cx, m, st, site);
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_actor(sc: &mut Scape, cx: &mut Ctx, id: u32, name: String, role: Option<String>, npc: bool, kind: &str, tile: IVec2, hp: i32, max: i32) {
    let pos = sc.grid.world(tile).unwrap_or(Vec3::ZERO);
    let (e, level, height) = if npc {
        let d = creature_def(kind);
        (look::creature(cx, kind, pos), d.map_or(1, |d| d.level), look::enemy_look(kind).height)
    } else {
        (look::astronaut(cx, &name, pos), 0, 2.0)
    };
    let a = Actor { name, role, npc, kind: kind.into(), level, tile, from: pos, to: pos, t: 1.0, pos, anim: anim::IDLE, e, yaw: 0.0, face: None, hp, max, combat_t: 99.0, height };
    let old = if npc { sc.npcs.insert(id, a) } else { sc.players.insert(id, a) };
    if let Some(o) = old {
        cx.commands.entity(o.e).despawn();
    }
}

fn clear_world(sc: &mut Scape, cx: &mut Ctx) {
    for (_, a) in sc.players.drain().chain(sc.npcs.drain()) {
        if let Ok(mut e) = cx.commands.get_entity(a.e) {
            e.despawn();
        }
    }
    for (_, n) in sc.nodes.drain() {
        if let Ok(mut e) = cx.commands.get_entity(n.e) {
            e.despawn();
        }
    }
    sc.grid.blocked.clear();
}

/// Switch the ground between the surface and the lava tube for a welcome.
fn set_place(sc: &mut Scape, cx: &mut Ctx, site: &Site, cave_info: Option<CaveInfo>) {
    match cave_info {
        Some(ci) => {
            // far below the terrain (which is hidden while we're down there)
            let y0 = site.spawn.y - 3000.0;
            let Some(map) = cave::CaveMap::from_rle(ci.w as usize, ci.h as usize, &ci.rle, y0) else {
                warn!("marsscape: bad cave map ({}x{})", ci.w, ci.h);
                return;
            };
            let map = Arc::new(map);
            if let Some(r) = sc.cave_root.take() {
                cx.commands.entity(r).despawn();
            }
            sc.cave_root = Some(look::cave(cx, &map));
            sc.grid = WalkGrid::new_cave(map.clone());
            sc.cave = Some(map);
            sc.cam_reset = true;
            info!("marsscape: in the lava tube ({}x{} tiles, {} floor)", ci.w, ci.h, sc.cave.as_ref().map_or(0, |c| c.grid.iter().filter(|&&v| v == 1).count()));
        }
        None => {
            if sc.cave.take().is_some() {
                if let Some(r) = sc.cave_root.take() {
                    cx.commands.entity(r).despawn();
                }
                sc.grid = WalkGrid::new(site);
                sc.cam_reset = true;
            }
        }
    }
}

fn actor_mut<'a>(sc: &'a mut Scape, w: Who) -> Option<&'a mut Actor> {
    match w {
        Who::P(i) => sc.players.get_mut(&i),
        Who::N(i) => sc.npcs.get_mut(&i),
    }
}

fn apply(sc: &mut Scape, cx: &mut Ctx, m: S2C, st: &mut UiState, site: &Site) {
    match m {
        S2C::Welcome { id, token, tick_ms, you, players, nodes, npcs, room, event, cave, exit: _, tick } => {
            clear_world(sc, cx);
            sc.tick_n = tick;
            let in_cave = room.as_deref() == Some("cave") || cave.is_some();
            set_place(sc, cx, site, if in_cave { cave } else { None });
            sc.me = Some(id);
            if let Some(n) = you.name.clone().filter(|n| !n.is_empty()) {
                sc.name = n;
            }
            if !token.is_empty() && !sc.url.is_empty() && sc.persist {
                store::set_token(&sc.token_key(), &token);
            }
            if !token.is_empty() {
                sc.token = token;
            }
            sc.gear = you.gear.clone();
            sc.armour = you.gear.bonus();
            sc.gear_dirty = true;
            sc.quests = you.quests.clone();
            sc.qp = you.quest_points.max(quest_points(&sc.quests));
            sc.bank = None;
            sc.trade = None;
            sc.trade_req = None;
            sc.dialog = None;
            sc.lamp = None;
            sc.surge = event.filter(|e| e.phase != "ended").map(|e| SurgeView { phase: e.phase, at: IVec2::new(e.tx, e.tz), ends_tick: tick + e.ends_in });
            sc.tick_s = tick_ms.max(100) as f32 / 1000.0;
            sc.skills = you.skills;
            sc.inv = you.inv;
            sc.inv.resize(INV_SLOTS, None);
            sc.weapon = you.weapon;
            sc.hp = you.hp;
            sc.max_hp = you.max_hp;
            for n in nodes {
                let t = IVec2::new(n.tx, n.tz);
                let pos = sc.grid.world(t).unwrap_or(Vec3::ZERO);
                if n.kind != "rift" {
                    sc.grid.blocked.insert(t);
                }
                let e = look::node(cx, &n.kind, pos, n.id);
                sc.nodes.insert(n.id, NodeS { kind: n.kind, tile: t, up: n.up, e, pos });
            }
            let name = sc.name.clone();
            spawn_actor(sc, cx, id, name, None, false, "", IVec2::new(you.tx, you.tz), you.hp, you.max_hp);
            for p in players.into_iter().filter(|p| p.id != id) {
                let role = if p.bot { Some(p.role.unwrap_or_else(|| "colonist".into())) } else { None };
                spawn_actor(sc, cx, p.id, p.name, role, false, "", IVec2::new(p.tx, p.tz), 10, 10);
            }
            for n in npcs {
                spawn_actor(sc, cx, n.id, String::new(), None, true, &n.kind, IVec2::new(n.tx, n.tz), n.hp, n.max);
                if n.hp <= 0 || n.dead {
                    if let Some(a) = sc.npcs.get_mut(&n.id) {
                        a.anim = anim::DEAD;
                    }
                }
            }
            if sc.online() {
                sc.status = format!("online - {}", sc.url);
                if !sc.greeted {
                    sc.say(format!("Welcome to MarsScape, {}.", sc.name), GAME);
                }
            }
            if sc.cave.is_some() {
                sc.status = format!("{} - lava tube", if sc.online() { "online" } else { "offline" });
            }
            sc.dirty = true;
            if !sc.greeted {
                sc.say("Left-click the ground to walk, a rock to mine it. Right-click anything for more options.", ui::TIP);
                sc.say("Talk to colonists for work. The pit near the landing site leads down into a lava tube.", ui::TIP);
            }
            sc.greeted = true;
            info!("marsscape: welcome: id {id}, {} players, {} nodes, {} creatures", sc.players.len(), sc.nodes.len(), sc.npcs.len());
        }
        S2C::Tick { n, p, m } => {
            sc.tick_n = n;
            for (id, tx, tz, an) in p {
                set_tile(sc, Who::P(id), IVec2::new(tx, tz), an, None);
            }
            for (id, tx, tz, an, hp) in m {
                set_tile(sc, Who::N(id), IVec2::new(tx, tz), an, Some(hp));
            }
            // close enough to ask for that trade
            if let Intent::Trade(id) = sc.intent {
                let near = sc.players.get(&id).zip(sc.my_tile()).is_some_and(|(a, me)| chebyshev((a.tile.x, a.tile.y), (me.x, me.y)) <= 8);
                if near {
                    sc.intent = Intent::None;
                    sc.send(C2S::TradeRequest { id });
                }
            }
            // arrived next to the fabricator we walked to
            if let Intent::Fab(f) = sc.intent {
                if sc.fab_near() == Some(f) {
                    st.fab_open = true;
                    sc.intent = Intent::None;
                    sc.dirty = true;
                }
            }
        }
        S2C::Join { id, name, tx, tz, bot, role } => {
            if Some(id) != sc.me {
                let role = if bot { Some(role.unwrap_or_else(|| "colonist".into())) } else { None };
                if role.is_none() {
                    sc.say(format!("{name} has arrived."), GAME);
                }
                spawn_actor(sc, cx, id, name, role, false, "", IVec2::new(tx, tz), 10, 10);
            }
        }
        S2C::Leave { id } => {
            if let Some(a) = sc.players.remove(&id) {
                cx.commands.entity(a.e).despawn();
                sc.say(format!("{} has left.", a.name), GAME);
            }
        }
        S2C::Chat { name, msg, .. } => sc.say(format!("{name}: {msg}"), SAY),
        S2C::Node { id, up } => {
            if let Some(n) = sc.nodes.get_mut(&id) {
                n.up = up;
            }
        }
        S2C::Xp { skill, xp, gain } => {
            if let Some(s) = Skill::parse(&skill) {
                sc.skills.set(s, xp);
                if gain > 0.0 {
                    sc.drops.push(XpDrop { text: format!("+{} {}", fmt_xp(gain), s.label()), t: 0.0 });
                }
                sc.dirty = true;
            }
        }
        S2C::Level { skill, level } => {
            // the server's own message says it in chat; this is the flourish by the minimap
            if let Some(s) = Skill::parse(&skill) {
                sc.drops.push(XpDrop { text: format!("{} level {level}!", s.label()), t: -0.6 });
            }
        }
        S2C::Inv { mut inv } => {
            inv.resize(INV_SLOTS, None);
            sc.inv = inv;
            sc.dirty = true;
        }
        S2C::Equip { weapon } => {
            sc.weapon = weapon;
            sc.dirty = true;
        }
        S2C::Hit { src, dst, dmg } => {
            let (Some(s), Some(d)) = (Who::parse(&src), Who::parse(&dst)) else { return };
            let dpos = actor_mut(sc, d).map(|a| a.pos + Vec3::Y * a.height * 0.55);
            if let Some(a) = actor_mut(sc, d) {
                a.combat_t = 0.0;
                if a.npc {
                    a.hp = (a.hp - dmg).max(0);
                }
            }
            let mine = s == Who::P(sc.me.unwrap_or(u32::MAX));
            let plasma = mine && sc.weapon.as_deref() == Some("plasma_gun");
            let mut shot = None;
            if let (Some(a), Some(dp)) = (actor_mut(sc, s), dpos) {
                a.combat_t = 0.0;
                a.face = Some(dp);
                let ranged = !a.npc || a.kind == "leviathan";
                if ranged {
                    let fwd = (dp - a.pos).with_y(0.0).normalize_or_zero();
                    let from = a.pos + Vec3::Y * if a.npc { a.height * 0.6 } else { 1.35 } + fwd * 0.7;
                    let lord = a.npc;
                    shot = Some((from, dp, plasma || lord, lord));
                }
                if let Ok(mut ec) = cx.commands.get_entity(a.e) {
                    ec.insert(Recoil);
                }
            }
            sc.pending_shots.extend(shot);
            sc.splat_k += 1;
            let k = sc.splat_k;
            sc.splats.push(Splat { who: d, dmg, heal: false, t: 0.0, k });
        }
        S2C::Heal { src, dst, amt } => {
            let (Some(s), Some(d)) = (Who::parse(&src), Who::parse(&dst)) else { return };
            let dpos = actor_mut(sc, d).map(|a| a.pos);
            if let (Some(a), Some(dp)) = (actor_mut(sc, s), dpos) {
                a.face = Some(dp);
            }
            sc.splat_k += 1;
            let k = sc.splat_k;
            sc.splats.push(Splat { who: d, dmg: amt, heal: true, t: 0.0, k });
        }
        S2C::Death { who } => {
            let Some(w) = Who::parse(&who) else { return };
            if let Some(a) = actor_mut(sc, w) {
                if a.npc {
                    a.anim = anim::DEAD;
                    a.hp = 0;
                }
                a.combat_t = 99.0;
            }
        }
        S2C::Hp { hp, max } => {
            sc.hp = hp;
            sc.max_hp = max;
            if let Some(me) = sc.me {
                if let Some(a) = sc.players.get_mut(&me) {
                    a.hp = hp;
                    a.max = max;
                }
            }
        }
        S2C::Msg { text } => sc.say(text, GAME),
        S2C::Err { text } => sc.say(text, WARN),
        S2C::Gear { weapon, helmet, suit, shield, cape, bonus } => {
            sc.weapon = weapon;
            sc.gear = Gear { helmet, suit, shield, cape };
            sc.armour = bonus;
            sc.gear_dirty = true;
            sc.dirty = true;
        }
        S2C::Travel { room } => travel(sc, cx, st, site, room),
        S2C::Bank { items, .. } => {
            sc.bank = Some(items);
            st.fab_open = false;
            sc.dirty = true;
        }
        S2C::TradeRequest { from, name } => {
            sc.say(format!("{name} wishes to trade with you."), TRADE);
            sc.trade_req = Some((from, name));
            sc.dirty = true;
        }
        S2C::Trade { stage, name, mine, theirs, accepted, .. } => {
            sc.trade = Some(TradeView { stage, name, mine, theirs, me_ok: accepted.me, them_ok: accepted.them });
            sc.trade_req = None;
            sc.bank = None;
            st.fab_open = false;
            sc.dirty = true;
        }
        S2C::TradeEnd { result, reason } => {
            sc.trade = None;
            if result == "done" {
                sc.say("Trade accepted.", TRADE);
            } else if reason.is_empty() {
                sc.say("Trade cancelled.", TRADE);
            } else {
                sc.say(format!("Trade cancelled: {reason}"), TRADE);
            }
            sc.dirty = true;
        }
        S2C::Dialog { name, role, text, options, .. } => {
            sc.dialog = Some(DialogView { name, role, text, options });
            sc.dirty = true;
        }
        S2C::DialogEnd => {
            sc.dialog = None;
            sc.dirty = true;
        }
        S2C::Quest { id, state, step, progress, qp, .. } => {
            sc.quests.insert(id, QuestSt { s: state, step, p: progress });
            sc.qp = qp.max(quest_points(&sc.quests));
            sc.dirty = true;
        }
        S2C::Event(e) => {
            if e.phase == "ended" || e.phase == "idle" {
                sc.surge = None;
            } else {
                let at = IVec2::new(e.tx, e.tz);
                if sc.surge.is_none() {
                    info!("marsscape: rift surge {} at {at:?}", e.phase);
                }
                sc.surge = Some(SurgeView { phase: e.phase, at, ends_tick: sc.tick_n + e.ends_in });
            }
            sc.dirty = true;
        }
        S2C::NpcSpawn { npc } => {
            spawn_actor(sc, cx, npc.id, String::new(), None, true, &npc.kind, IVec2::new(npc.tx, npc.tz), npc.hp, npc.max);
            if let Some(a) = sc.npcs.get_mut(&npc.id) {
                a.combat_t = 99.0;
                if npc.dead || npc.hp <= 0 {
                    a.anim = anim::DEAD;
                }
            }
        }
        S2C::NpcDespawn { id } => {
            if let Some(a) = sc.npcs.remove(&id) {
                if let Ok(mut e) = cx.commands.get_entity(a.e) {
                    e.despawn();
                }
            }
        }
        S2C::Unknown => {}
    }
}

/// Marker component: the rig just fired / struck (animate kicks the recoil).
#[derive(Component)]
struct Recoil;

fn set_tile(sc: &mut Scape, w: Who, t: IVec2, an: u8, hp: Option<i32>) {
    let world = sc.grid.world(t);
    let Some(a) = actor_mut(sc, w) else { return };
    if let Some(hp) = hp {
        if hp > a.hp && a.anim == anim::DEAD {
            a.from = world.unwrap_or(a.pos); // respawned at home
            a.pos = a.from;
            a.t = 1.0;
        }
        a.hp = hp;
    }
    if t != a.tile {
        let jump = chebyshev((t.x, t.y), (a.tile.x, a.tile.y)) > 3;
        a.tile = t;
        if let Some(wp) = world {
            a.from = if jump { wp } else { a.pos };
            a.to = wp;
            a.t = if jump { 1.0 } else { 0.0 };
            if jump {
                a.pos = wp;
            }
        }
        a.face = None;
    }
    if !(a.npc && a.anim == anim::DEAD && hp.is_some_and(|h| h <= 0)) {
        a.anim = an;
    }
    if !matches!(an, anim::ATTACK | anim::MINE | anim::SURVEY | anim::CRAFT) {
        a.face = None;
    }
}

pub fn fmt_xp(x: f64) -> String {
    if (x - x.round()).abs() < 0.05 { format!("{}", x.round() as i64) } else { format!("{x:.1}") }
}

// ------------------------------------------------------------------------------------ input

fn keys(keys: Res<ButtonInput<KeyCode>>, mut kb: MessageReader<bevy::input::keyboard::KeyboardInput>, mut st: ResMut<UiState>, scape: Option<ResMut<Scape>>, menu: Option<Res<Menu>>) {
    let typed: Vec<_> = kb.read().filter(|e| e.state.is_pressed()).cloned().collect();
    let Some(mut sc) = scape else { return };
    if menu.is_some_and(|m| m.open) || st.name_entry.is_some() {
        return;
    }
    use bevy::input::keyboard::Key;
    if let Some(mut line) = st.typing.clone() {
        let mut send = false;
        for ev in &typed {
            match &ev.logical_key {
                Key::Enter => send = true,
                Key::Backspace => {
                    line.pop();
                }
                Key::Escape => {
                    st.typing = None;
                    return;
                }
                _ => {
                    if let Some(t) = &ev.text {
                        for c in t.chars().filter(|c| !c.is_control()) {
                            if line.chars().count() < CHAT_MAX {
                                line.push(c);
                            }
                        }
                    }
                }
            }
        }
        if send {
            sc.chat_send(&line);
            st.typing = None;
        } else {
            st.typing = Some(line);
        }
        sc.chat_dirty = true;
        return;
    }
    if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]) {
        st.typing = Some(String::new());
        sc.chat_dirty = true;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        sc.run = !sc.run;
        sc.dirty = true;
    }
    for (k, tab) in [(KeyCode::F1, 0u8), (KeyCode::F2, 1), (KeyCode::F3, 2), (KeyCode::F4, 3)] {
        if keys.just_pressed(k) {
            st.tab = tab;
            sc.dirty = true;
        }
    }
}

/// The right-click options for one thing in the world (first = its left-click action).
fn target_options(sc: &Scape, t: pick::Target) -> Vec<Opt> {
    use pick::Target;
    match t {
        Target::Npc(id) => {
            let Some(a) = sc.npcs.get(&id) else { return Vec::new() };
            let def = creature_def(&a.kind);
            let name = def.map_or("Creature", |d| d.name);
            let ex = def.map_or("", |d| d.examine);
            vec![Opt::new(format!("Attack {name} (level {})", a.level), UiAct::Attack(id)).on(t), Opt::new(format!("Examine {name}"), UiAct::Say(ex.into())).on(t)]
        }
        Target::Player(id) => {
            let Some(a) = sc.players.get(&id) else { return Vec::new() };
            let mut o = Vec::new();
            match &a.role {
                Some(r) => o.push(Opt::new(format!("Talk-to {} ({r})", a.name), UiAct::Talk(id)).on(t)),
                None => o.push(Opt::new(format!("Trade with {}", a.name), UiAct::TradeWith(id)).on(t)),
            }
            let ex = match a.role.as_deref() {
                Some("guard") => format!("{}, colony guard. Say \"help\" and they'll watch your back.", a.name),
                Some("medic") => format!("{}, the colony medic. Patches up anyone who's hurt.", a.name),
                Some("quartermaster") => format!("{}, the quartermaster. Hands newcomers a starter kit.", a.name),
                Some(r) => format!("{}, a colonist ({r}).", a.name),
                None => format!("{}, a fellow astronaut.", a.name),
            };
            o.push(Opt::new(format!("Examine {}", a.name), UiAct::Say(ex)).on(t));
            o
        }
        Target::Node(id) => {
            let Some(n) = sc.nodes.get(&id) else { return Vec::new() };
            let name = node_name(&n.kind).to_string();
            let mut o = Vec::new();
            match n.kind.as_str() {
                "fabricator" => o.push(Opt::new("Use Fabricator".into(), UiAct::Node(id)).on(t)),
                "rift" => {}
                "locker" => o.push(Opt::new("Open Colony locker".into(), UiAct::Node(id)).on(t)),
                "pit" => o.push(Opt::new("Climb-down Lava-tube pit".into(), UiAct::Node(id)).on(t)),
                "ladder" => o.push(Opt::new("Climb-up Ladder".into(), UiAct::Node(id)).on(t)),
                k => {
                    if let Some(d) = node_def(k) {
                        o.push(Opt::new(format!("{} {name}", d.verb), UiAct::Node(id)).on(t));
                    }
                }
            }
            let ex = match n.kind.as_str() {
                "fabricator" => "Turns raw materials into useful things.",
                "locker" => "The colony's lockers. Whatever you put in stays safe, even if you don't.",
                "pit" => "A collapsed lava tube. A rope goes down into the dark, and something down there glows.",
                "ladder" => "Daylight, a long way up.",
                "rift" => "A tear into the void. Stars where the ground should be, and things crawling out.",
                k => node_def(k).map_or("", |d| d.examine),
            };
            o.push(Opt::new(format!("Examine {name}"), UiAct::Say(ex.into())).on(t));
            o
        }
    }
}

/// What's under the cursor, as menu options (first = left-click default): every thing under it
/// (best first, see `pick::under`), then "Walk here", then astronauts (clicking a person walks
/// there, like stepping up to them), then Cancel.
fn options_at(sc: &Scape, cam: (&Camera, &GlobalTransform), cursor: Vec2, g: Gnd, pr: &pick::PickRects) -> Vec<Opt> {
    let (camera, gt) = cam;
    let mut opts = Vec::new();
    let mut people = Vec::new();
    for t in pick::under(pr, sc, cursor, cam, g) {
        let o = target_options(sc, t);
        // other astronauts come after "Walk here" (clicking them walks up to them); colonists talk
        let human = matches!(t, pick::Target::Player(id) if sc.players.get(&id).is_some_and(|a| a.role.is_none()));
        if human {
            people.extend(o);
        } else {
            opts.extend(o);
        }
    }
    if let Ok(ray) = camera.viewport_to_world(gt, cursor) {
        if let Some(p) = pick::ray_ground(g, ray) {
            let t = tile_of(p.x, p.z);
            // a click on cave rock walks to the nearest floor next to it
            let t = match sc.cave.as_deref() {
                Some(c) if !c.floor_at(t.x, t.y) => nearest_floor(c, t).unwrap_or(t),
                _ => t,
            };
            opts.push(Opt::new("Walk here".into(), UiAct::Walk(t)));
        }
    }
    opts.extend(people);
    opts.push(Opt::new("Cancel".into(), UiAct::Cancel));
    opts
}

/// What a plain left click at `cursor` would do (clicktest).
fn left_pick(sc: &Scape, cam: (&Camera, &GlobalTransform), cursor: Vec2, g: Gnd, pr: &pick::PickRects) -> pick::Picked {
    use pick::Picked;
    match options_at(sc, cam, cursor, g, pr).first() {
        Some(Opt { target: Some(t), .. }) => Picked::T(*t),
        Some(Opt { act: UiAct::Walk(_), .. }) => Picked::Ground,
        _ => Picked::Nothing,
    }
}

fn ground_pick(g: Gnd, ray: Ray3d) -> Option<Vec3> {
    pick::ray_ground(g, ray)
}

/// Nearest cave floor tile to a wall tile (a click on the rock).
fn nearest_floor(c: &cave::CaveMap, t: IVec2) -> Option<IVec2> {
    for r in 1..=4 {
        let mut best: Option<(f32, IVec2)> = None;
        for dz in -r..=r {
            for dx in -r..=r {
                let q = t + IVec2::new(dx, dz);
                if c.floor_at(q.x, q.y) {
                    let d = ((dx * dx + dz * dz) as f32).sqrt();
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, q));
                    }
                }
            }
        }
        if let Some((_, q)) = best {
            return Some(q);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn world_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    scape: Option<ResMut<Scape>>,
    site: Option<Res<Site>>,
    menu: Option<Res<Menu>>,
    hover: Res<HoverMap>,
    nodes: Query<(), With<Node>>,
    win: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    pr: Res<pick::PickRects>,
    mut st: ResMut<UiState>,
    mut out: MessageWriter<UiMsg>,
) {
    let (Some(sc), Some(site)) = (scape, site) else { return };
    st.hover_target = None;
    if menu.is_some_and(|m| m.open) || st.name_entry.is_some() {
        st.hover.clear();
        return;
    }
    let Ok(cam) = cam.single() else { return };
    // test script: open the right-click menu at a point / on the nearest thing of a kind
    if let Some(req) = st.ctx_request.take() {
        let at = match req {
            ui::CtxReq::At(p) => Some(p),
            ui::CtxReq::Kind(k) => {
                let me = sc.my_pos().unwrap_or_default();
                let near = |p: Vec3| p.distance_squared(me);
                let n = sc.nodes.values().filter(|n| n.kind == k).map(|n| (n.pos + Vec3::Y * 0.5, near(n.pos)));
                let a = sc.npcs.values().filter(|a| a.kind == k && a.anim != anim::DEAD).map(|a| (a.pos + Vec3::Y * a.height * 0.5, near(a.pos)));
                // nearest one that is on screen
                let mut v: Vec<(Vec3, f32)> = n.chain(a).collect();
                v.sort_by(|x, y| x.1.total_cmp(&y.1));
                v.into_iter().filter_map(|(p, _)| cam.0.world_to_viewport(cam.1, p).ok()).find(|s| s.x > 40.0 && s.y > 40.0 && s.x < 1000.0 && s.y < 520.0)
            }
        };
        if let Some(p) = at {
            let opts = options_at(&sc, cam, p, sc.gnd(&site), &pr);
            st.hover = opts.first().map(|o| o.label.clone()).unwrap_or_default();
            st.hover_target = opts.first().and_then(|o| o.target);
            st.ctx = Some(CtxMenu { at: p, opts });
        }
        return;
    }
    let Ok(w) = win.single() else { return };
    let Some(cursor) = w.cursor_position().or(st.fake_cursor) else {
        st.over_ui = false;
        if st.ctx.is_none() {
            st.hover.clear();
        }
        return;
    };
    st.cursor = cursor;
    let over_ui = hover.get(&PointerId::Mouse).is_some_and(|m| m.keys().any(|e| nodes.contains(*e)));
    st.over_ui = over_ui;
    if over_ui {
        if st.ctx.is_none() {
            st.hover.clear();
        }
        return;
    }
    let left = mouse.just_pressed(MouseButton::Left);
    let right = mouse.just_pressed(MouseButton::Right);
    if st.ctx.is_some() {
        if left || right {
            st.ctx = None; // clicked away from the menu
        }
        return;
    }
    let opts = options_at(&sc, cam, cursor, sc.gnd(&site), &pr);
    st.hover_target = opts.first().and_then(|o| o.target);
    let n = opts.len().saturating_sub(1);
    st.hover = match opts.first() {
        Some(o) if o.act != UiAct::Cancel => {
            if n > 1 { format!("{} / {} more options", o.label, n - 1) } else { o.label.clone() }
        }
        _ => String::new(),
    };
    if left {
        if let Some(o) = opts.into_iter().next() {
            let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
            out.write(UiMsg { act: o.act, alt: shift });
        }
    } else if right {
        st.ctx = Some(CtxMenu { at: cursor, opts });
    }
}

fn ui_actions(mut msgs: MessageReader<UiMsg>, scape: Option<ResMut<Scape>>, mut st: ResMut<UiState>, mut orbit: Option<ResMut<Orbit>>) {
    let Some(mut sc) = scape else {
        msgs.clear();
        return;
    };
    for m in msgs.read().cloned().collect::<Vec<_>>() {
        if !matches!(m.act, UiAct::Slot(_) | UiAct::Tab(_)) {
            st.ctx = None;
        }
        match m.act {
            UiAct::Walk(t) => {
                sc.walk_to(t, m.alt);
            }
            UiAct::Node(id) => sc.act_node(id),
            UiAct::Attack(id) => sc.attack(id),
            UiAct::Say(s) => sc.say(s, GAME),
            UiAct::Cancel => {}
            UiAct::Tab(t) => {
                st.tab = t;
                sc.dirty = true;
            }
            UiAct::Slot(i) => {
                let Some(item) = sc.inv.get(i).cloned().flatten() else { continue };
                // with the locker or a trade open, a click puts the item in
                let opts = if sc.trade.is_some() {
                    ui::move_options(i, &item.item, "Offer")
                } else if sc.bank.is_some() {
                    ui::move_options(i, &item.item, "Deposit")
                } else {
                    ui::item_options(i, &item.item)
                };
                if m.alt {
                    st.ctx = Some(CtxMenu { at: st.cursor, opts });
                } else if let Some(o) = opts.into_iter().next() {
                    st.ctx = None;
                    match o.act {
                        UiAct::Item(s, v) => sc.item(s, v),
                        UiAct::Deposit(s, q) => sc.bank_deposit(s, q),
                        UiAct::Offer(s, q) => sc.trade_offer(s, q),
                        _ => {}
                    }
                }
            }
            UiAct::Talk(id) => sc.talk(id),
            UiAct::TradeWith(id) => sc.trade_with(id),
            UiAct::Choose(o) => sc.choose(&o),
            UiAct::Deposit(s, q) => sc.bank_deposit(s, q),
            UiAct::Withdraw(i, q) => {
                if m.alt {
                    if let Some(it) = sc.bank.as_ref().and_then(|b| b.get(i)).cloned() {
                        st.ctx = Some(CtxMenu { at: st.cursor, opts: ui::withdraw_options(i, &it.item) });
                    }
                } else {
                    sc.bank_withdraw(i, q);
                }
            }
            UiAct::CloseBank => {
                sc.bank = None;
                sc.dirty = true;
            }
            UiAct::Offer(s, q) => sc.trade_offer(s, q),
            UiAct::Unoffer(i) => sc.trade_remove(i),
            UiAct::TradeAccept => sc.trade_accept(),
            UiAct::TradeDecline => sc.trade_decline(),
            UiAct::TradeReq(yes) => {
                if let Some((id, _)) = sc.trade_req.take() {
                    if yes {
                        sc.send(C2S::TradeRequest { id });
                    }
                }
                sc.dirty = true;
            }
            UiAct::Rub(k) => sc.rub(k),
            UiAct::CloseLamp => {
                sc.lamp = None;
                sc.dirty = true;
            }
            UiAct::UnequipGear(g) => {
                if g == "weapon" {
                    sc.unequip();
                } else {
                    sc.unequip_gear(g);
                }
            }
            UiAct::Item(s, v) => sc.item(s, v),
            UiAct::Make(r) => sc.craft(r),
            UiAct::CloseFab => {
                st.fab_open = false;
                sc.dirty = true;
            }
            UiAct::Run => {
                sc.run = !sc.run;
                sc.dirty = true;
            }
            UiAct::Compass => {
                if let Some(o) = orbit.as_mut() {
                    o.yaw = 0.0;
                }
            }
        }
    }
    // walked away from the fabricator / locker: close its panel
    if st.fab_open && sc.fab_near().is_none() && sc.me.is_some() {
        st.fab_open = false;
        sc.dirty = true;
    }
    if sc.bank.is_some() && sc.near_kind("locker").is_none() && sc.me.is_some() {
        sc.bank = None;
        sc.dirty = true;
    }
    if sc.lamp.is_some_and(|i| sc.inv.get(i).cloned().flatten().is_none_or(|s| s.item != "xp_lamp")) {
        sc.lamp = None;
        sc.dirty = true;
    }
}

// ------------------------------------------------------------------------------------ motion

fn move_actors(scape: Option<ResMut<Scape>>, site: Option<Res<Site>>, time: Res<Time>, mut focus: Option<ResMut<Focus>>, mut q: Query<(&mut Transform, &mut look::Rig)>) {
    let (Some(mut sc), Some(site)) = (scape, site) else { return };
    let dt = time.delta_secs().min(0.1);
    let Scape { players, npcs, me, tick_s, cave, .. } = &mut *sc;
    let (tick, me) = (*tick_s, *me);
    let g = match cave.as_deref() {
        Some(c) => Gnd::Cave(c),
        None => Gnd::Site(&site),
    };
    let underground = cave.is_some();
    for (id, a) in players.iter_mut().map(|(i, a)| (Some(*i), a)).chain(npcs.iter_mut().map(|(_, a)| (None, a))) {
        a.combat_t += dt;
        if a.t < 1.0 {
            a.t = (a.t + dt / tick).min(1.0);
        }
        let p = a.from.lerp(a.to, a.t);
        let y = g.floor(p.x, p.z).unwrap_or(p.y);
        a.pos = Vec3::new(p.x, y, p.z);
        let dir = match a.face {
            Some(f) => (f - a.pos).with_y(0.0),
            None => (a.to - a.from).with_y(0.0),
        };
        if dir.length_squared() > 1e-4 {
            let want = (-dir.x).atan2(-dir.z);
            let mut d = want - a.yaw;
            d = (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            a.yaw += d * (1.0 - (-12.0 * dt).exp());
        }
        if let Ok((mut t, mut rig)) = q.get_mut(a.e) {
            t.translation = a.pos;
            t.rotation = Quat::from_rotation_y(a.yaw);
            rig.anim = a.anim;
        }
        // the terrain's detail window stays where the pit is while we're underground
        if id.is_some() && id == me && !underground {
            if let Some(f) = focus.as_mut() {
                f.0 = a.pos;
            }
        }
    }
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    mut rigs: Query<(Entity, &mut look::Rig, &mut Transform, Option<&Recoil>), Without<look::Part>>,
    children: Query<&Children>,
    mut parts: Query<(&look::Part, &mut Transform, &mut Visibility), Without<look::Rig>>,
    scape: Option<Res<Scape>>,
) {
    let dt = time.delta_secs().min(0.1);
    let me_weapon = scape.as_ref().map(|s| s.weapon.clone());
    let me_e = scape.as_ref().and_then(|s| s.me.and_then(|m| s.players.get(&m)).map(|a| a.e));
    for (e, mut rig, mut rt, rec) in &mut rigs {
        if rec.is_some() {
            rig.recoil = 1.0;
            commands.entity(e).remove::<Recoil>();
        }
        rig.recoil = (rig.recoil - dt * 5.0).max(0.0);
        let speed = match rig.anim {
            look_anim if look_anim == anim::RUN => 11.0,
            look_anim if look_anim == anim::WALK => 7.0,
            look_anim if look_anim == anim::MINE || look_anim == anim::SURVEY => 4.2,
            _ => 3.0,
        } / rig.scale.max(0.5).sqrt();
        rig.phase += dt * speed;
        // death: topple and sink
        if rig.anim == anim::DEAD {
            rig.dead_t += dt;
            let k = (rig.dead_t / 0.5).min(1.0);
            rt.rotation *= Quat::from_rotation_z(k * 1.45);
            rt.translation.y -= ((rig.dead_t - 1.5).max(0.0) * 0.8).min(3.0) * rig.scale;
        } else {
            rig.dead_t = 0.0;
        }
        let gun = rig.anim == anim::ATTACK || (Some(e) == me_e && me_weapon.as_ref().is_some_and(|w| w.is_some()));
        for d in children.iter_descendants(e) {
            let Ok((p, mut t, mut v)) = parts.get_mut(d) else { continue };
            match p.limb {
                look::Limb::Tool => {
                    let want = if matches!(rig.anim, anim::MINE | anim::SURVEY) { Visibility::Inherited } else { Visibility::Hidden };
                    if *v != want {
                        *v = want;
                    }
                }
                look::Limb::Gun => {
                    let want = if gun && !matches!(rig.anim, anim::MINE | anim::SURVEY) { Visibility::Inherited } else { Visibility::Hidden };
                    if *v != want {
                        *v = want;
                    }
                }
                look::Limb::Shard(i) => {
                    let a = rig.phase * 0.35 + i as f32 * std::f32::consts::TAU / 5.0;
                    t.translation = Vec3::new(a.cos() * 0.95, 1.45 + 0.35 * (a * 2.0 + i as f32).sin(), a.sin() * 0.95);
                    t.rotation = Quat::from_rotation_y(a * 2.0) * Quat::from_rotation_z(0.4);
                }
                look::Limb::Hover => {
                    t.translation.y = p.k + 0.18 * (rig.phase * 0.5).sin();
                }
                look::Limb::Seg(i) => {
                    let amp = if matches!(rig.anim, anim::WALK | anim::RUN) { 0.24 } else { 0.08 };
                    let k = i as f32;
                    t.translation.x = amp * (rig.phase * 1.3 - k * 0.9).sin();
                    t.translation.z = p.k;
                    t.rotation = Quat::from_rotation_y(0.5 * amp * (rig.phase * 1.3 - k * 0.9).cos());
                    if rig.anim == anim::ATTACK && i == 0 {
                        t.translation.y = 0.33 + 0.25 * rig.recoil;
                    }
                }
                l => {
                    let target = p.rest * look::pose(l, rig.anim, rig.phase + p.k, rig.recoil);
                    t.rotation = t.rotation.slerp(target, 1.0 - (-14.0 * dt).exp());
                }
            }
        }
    }
}

fn orbit_camera(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time<Real>>,
    orbit: Option<ResMut<Orbit>>,
    scape: Option<ResMut<Scape>>,
    site: Option<Res<Site>>,
    menu: Option<Res<Menu>>,
    st: Res<UiState>,
    mut cam: Query<&mut Transform, With<Camera3d>>,
) {
    let (Some(mut o), Some(mut sc), Some(site), Ok(mut cam)) = (orbit, scape, site, cam.single_mut()) else { return };
    if std::mem::take(&mut sc.cam_reset) {
        // a new place: look down into the tube, or the usual view up top
        (o.pitch, o.dist) = if sc.cave.is_some() { (1.0, 13.0) } else { (0.62, 15.0) };
        o.look = None;
    }
    let dt = time.delta_secs().min(0.1);
    let free = menu.is_none_or(|m| !m.open) && st.typing.is_none() && st.name_entry.is_none();
    if free {
        let k = |a: KeyCode| keys.pressed(a) as i32 as f32;
        o.yaw += (k(KeyCode::ArrowLeft) - k(KeyCode::ArrowRight)) * 1.8 * dt;
        o.pitch += (k(KeyCode::ArrowUp) - k(KeyCode::ArrowDown)) * 1.0 * dt;
        if mouse.pressed(MouseButton::Middle) {
            o.yaw -= motion.delta.x * 0.006;
            o.pitch += motion.delta.y * 0.004;
        }
        if !st.over_ui {
            let notches = match scroll.unit {
                MouseScrollUnit::Line => scroll.delta.y,
                MouseScrollUnit::Pixel => scroll.delta.y / 100.0,
            };
            o.dist = (o.dist * 0.88f32.powf(notches)).clamp(4.0, 60.0);
        }
    }
    let underground = sc.cave.clone();
    // in the tube: from above, over the walls (low angles would put the camera in the rock)
    if underground.is_some() {
        o.pitch = o.pitch.clamp(0.8, 1.45);
        o.dist = o.dist.clamp(5.0, 28.0);
    }
    o.pitch = o.pitch.clamp(0.22, 1.45);
    let Some(me) = sc.my_pos() else { return };
    let want = me + Vec3::Y * 1.2;
    let look = match o.look {
        Some(l) if l.distance(want) < 30.0 => l.lerp(want, 1.0 - (-10.0 * dt).exp()),
        _ => want,
    };
    o.look = Some(look);
    let dir = Vec3::new(o.yaw.sin() * o.pitch.cos(), o.pitch.sin(), o.yaw.cos() * o.pitch.cos());
    let mut p = look + dir * o.dist;
    if let Some(cv) = underground {
        // never inside the rock: rise until the camera and the line to you clear it
        let blocked = |p: Vec3| {
            (0..=20).any(|i| {
                let q = look.lerp(p, i as f32 / 20.0);
                i > 1 && cv.solid_h(q.x, q.z) > q.y - 0.35
            })
        };
        for _ in 0..24 {
            if p.y < cv.solid_h(p.x, p.z) + 1.5 || blocked(p) {
                p.y += 0.75;
            } else {
                break;
            }
        }
        cam.translation = p;
        cam.look_at(look, Vec3::Y);
        return;
    }
    // stay above the ground and keep the character in sight
    for _ in 0..12 {
        let ground = site.height(p.x, p.z).unwrap_or(f32::MIN);
        if p.y < ground + 1.2 || crate::camera::line_clearance(&site, look, p, 24) < 0.3 {
            p.y += 1.0;
        } else {
            break;
        }
    }
    cam.translation = p;
    cam.look_at(look, Vec3::Y);
}

// ------------------------------------------------------------------------------------ fx

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn effects(
    mut commands: Commands,
    time: Res<Time>,
    scape: Option<ResMut<Scape>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut kit: ResMut<Kit>,
    mut markers: Query<(Entity, &mut look::Marker, &mut Transform), (Without<look::Shot>, Without<look::Flash>, Without<look::RiftSpark>)>,
    mut shots: Query<(Entity, &mut look::Shot, &mut Transform), (Without<look::Flash>, Without<look::RiftSpark>)>,
    mut flashes: Query<(Entity, &mut look::Flash, &mut Transform), Without<look::RiftSpark>>,
    mut sparks: Query<(&mut look::RiftSpark, &mut Transform)>,
    mut node_vis: Query<(&ChildOf, &mut Visibility, Has<look::NodeFull>), Or<(With<look::NodeFull>, With<look::NodeEmpty>)>>,
) {
    let Some(mut sc) = scape else { return };
    let dt = time.delta_secs().min(0.1);
    let mut cx = Ctx { commands: &mut commands, meshes: &mut meshes, mats: &mut mats, kit: &mut kit };
    if let Some((at, red)) = sc.marker.take() {
        for (e, _, _) in &markers {
            cx.commands.entity(e).despawn();
        }
        look::marker(&mut cx, at, red);
    }
    for (a, b, plasma, _) in std::mem::take(&mut sc.pending_shots) {
        look::shot(&mut cx, a, b, plasma);
        look::flash(&mut cx, a);
    }
    for (e, mut m, mut t) in &mut markers {
        m.0 += dt;
        let s = (1.0 - m.0 / 0.9).max(0.0);
        t.scale = Vec3::splat(0.4 + s);
        if m.0 > 0.9 {
            commands.entity(e).despawn();
        }
    }
    for (e, mut s, mut t) in &mut shots {
        s.t += dt;
        let k = (s.t / s.dur).min(1.0);
        t.translation = s.a.lerp(s.b, k);
        if k >= 1.0 {
            commands.entity(e).despawn();
        }
    }
    for (e, mut f, mut t) in &mut flashes {
        f.0 += dt;
        t.scale = Vec3::splat(1.0 + f.0 * 12.0);
        if f.0 > 0.07 {
            commands.entity(e).despawn();
        }
    }
    for (mut s, mut t) in &mut sparks {
        s.0 += dt * 0.45;
        let k = s.0.fract();
        let a = s.0.floor() * 2.4 + s.0 * 0.3;
        t.translation = Vec3::new(a.cos() * 0.9 * (1.0 - k), 0.1 + k * 2.6, a.sin() * 0.9 * (1.0 - k));
        t.scale = Vec3::splat((1.0 - k) * 1.4);
    }
    let up: HashMap<Entity, bool> = sc.nodes.values().map(|n| (n.e, n.up)).collect();
    for (parent, mut v, full) in &mut node_vis {
        if let Some(&u) = up.get(&parent.parent()) {
            let want = if u == full { Visibility::Inherited } else { Visibility::Hidden };
            if *v != want {
                *v = want;
            }
        }
    }
    for s in sc.splats.iter_mut() {
        s.t += dt;
    }
    sc.splats.retain(|s| s.t < 1.2);
    for d in sc.drops.iter_mut() {
        d.t += dt;
    }
    sc.drops.retain(|d| d.t < 1.8);
}

// ------------------------------------------------------------------------------------ test script

/// `--mmo-script` runner: comma-separated `CMD[:ARG][@DELAY]` steps (DELAY = seconds to wait
/// after the step, default 0.5). `wait:S`, `walk:DX,DZ` (tiles from you), `act:KIND` (nearest
/// node of that kind), `attack:KIND` (nearest live creature), `fab` (walk to the nearest fabricator),
/// `goto:KIND` (walk near the nearest node / creature of a kind), `craft:RECIPE`, `slot:N` / `equip:N` / `eat:N` / `drop:N`, `tab:inv|skills|gear`,
/// `chat:TEXT`, `type:TEXT` (open the chat line with TEXT, unsent), `ctx` (right-click menu at
/// the screen centre), `rotate:DEG`, `pitch:DEG`, `zoom:M`, `run:on|off`, `give:ITEM` (offline
/// only, cheat for tests), `xp:SKILL:XP` (offline cheat), `log`, `shot:FILE.png`, `quit`.
/// Clicking (pick.rs), on the nearest drawn thing of a KIND (node / creature kind, `colonist`,
/// `player`): `click:KIND` (left click through the real picking), `hover:KIND` / `unhover` (pretend
/// cursor), `ctxon:KIND` (right-click menu on it), `ctxoverlap` (menu where two things overlap),
/// `report:KIND` (log its screen position, for tools/web_click.mjs), `logpick` (log what's under
/// the cursor), `clicktest:LABEL` (measure picking from this camera, see `pick::click_test`).
#[derive(Resource)]
struct ScriptRun {
    steps: Vec<(String, String, f32)>,
    i: usize,
    next_t: f32,
    shot: Option<Arc<AtomicBool>>,
}

impl ScriptRun {
    fn parse(s: &str) -> Self {
        let steps = s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        // split on commas that start a new step (walk:DX,DZ contains one)
        let mut out: Vec<(String, String, f32)> = Vec::new();
        for item in steps.split(',') {
            if let (Some(last), true) = (out.last_mut(), item.chars().next().is_some_and(|c| c == '-' || c.is_ascii_digit())) {
                let (body, delay) = split_delay(item);
                last.1 = format!("{},{}", last.1, body);
                if let Some(d) = delay {
                    last.2 = d;
                }
                continue;
            }
            let (body, delay) = split_delay(item);
            let (cmd, arg) = body.split_once(':').unwrap_or((body, ""));
            out.push((cmd.to_string(), arg.to_string(), delay.unwrap_or(0.5)));
        }
        ScriptRun { steps: out, i: 0, next_t: 0.0, shot: None }
    }
}

fn split_delay(item: &str) -> (&str, Option<f32>) {
    match item.rsplit_once('@') {
        Some((b, d)) if d.parse::<f32>().is_ok() => (b, d.parse().ok()),
        _ => (item, None),
    }
}

#[allow(clippy::too_many_arguments)]
fn script(
    mut commands: Commands,
    run: Option<ResMut<ScriptRun>>,
    scape: Option<ResMut<Scape>>,
    time: Res<Time<Real>>,
    mut st: ResMut<UiState>,
    mut orbit: Option<ResMut<Orbit>>,
    win: Query<&Window, With<PrimaryWindow>>,
    (pr, site, cam): (Res<pick::PickRects>, Option<Res<Site>>, Query<(&Camera, &GlobalTransform), With<Camera3d>>),
    mut out: MessageWriter<UiMsg>,
    mut exit: MessageWriter<AppExit>,
) {
    let (Some(mut run), Some(mut sc)) = (run, scape) else { return };
    if sc.me.is_none() {
        return;
    }
    let now = time.elapsed_secs();
    if run.next_t == 0.0 {
        run.next_t = now + 1.0;
    }
    if let Some(f) = &run.shot {
        if !f.load(Ordering::Relaxed) {
            return;
        }
        run.shot = None;
    }
    if now < run.next_t || run.i >= run.steps.len() {
        return;
    }
    let (cmd, arg, delay) = run.steps[run.i].clone();
    run.i += 1;
    run.next_t = now + delay;
    info!("mmo-script: step {} {cmd}:{arg}", run.i - 1);
    let me = sc.my_tile().unwrap_or_default();
    let nearest_node = |sc: &Scape, kind: &str| sc.nodes.iter().filter(|(_, n)| n.kind == kind).min_by_key(|(_, n)| chebyshev((n.tile.x, n.tile.y), (me.x, me.y))).map(|(i, _)| *i);
    match cmd.as_str() {
        "wait" => run.next_t = now + arg.parse::<f32>().unwrap_or(1.0),
        "walk" => {
            let v: Vec<i32> = arg.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            if v.len() == 2 {
                sc.walk_to(me + IVec2::new(v[0], v[1]), false);
            }
        }
        "act" => match nearest_node(&sc, &arg) {
            Some(id) => sc.act_node(id),
            None => warn!("mmo-script: no {arg} node"),
        },
        "goto" => {
            // walk to 3 tiles short of the nearest node / live creature of a kind (screenshots)
            let n = sc.nodes.values().filter(|n| n.kind == arg).map(|n| n.tile);
            let c = sc.npcs.values().filter(|a| a.kind == arg && a.anim != anim::DEAD).map(|a| a.tile);
            if let Some(t) = n.chain(c).min_by_key(|t| chebyshev((t.x, t.y), (me.x, me.y))) {
                let d = (me - t).signum();
                sc.walk_to(t + d * 3, true);
            } else {
                warn!("mmo-script: nothing called {arg}");
            }
        }
        "face" => {
            // turn the camera to look past you at the nearest node / creature of a kind
            let n = sc.nodes.values().filter(|n| n.kind == arg).map(|n| n.pos);
            let c = sc.npcs.values().filter(|a| a.kind == arg && a.anim != anim::DEAD).map(|a| a.pos);
            let mp = sc.my_pos().unwrap_or_default();
            if let (Some(t), Some(o)) = (n.chain(c).min_by(|a, b| a.distance_squared(mp).total_cmp(&b.distance_squared(mp))), orbit.as_mut()) {
                let d = mp - t;
                o.yaw = d.x.atan2(d.z) + 0.6; // a little to the side, so you don't hide it
            }
        }
        "fab" => {
            if let Some(id) = nearest_node(&sc, "fabricator") {
                sc.act_node(id);
            }
        }
        "attack" => {
            let id = sc.npcs.iter().filter(|(_, a)| a.kind == arg && a.anim != anim::DEAD).min_by_key(|(_, a)| chebyshev((a.tile.x, a.tile.y), (me.x, me.y))).map(|(i, _)| *i);
            match id {
                Some(id) => sc.attack(id),
                None => warn!("mmo-script: no live {arg}"),
            }
        }
        "craft" => sc.craft(&arg),
        "slot" => {
            out.write(UiMsg { act: UiAct::Slot(arg.parse().unwrap_or(0)), alt: true });
        }
        "equip" | "eat" | "drop" => {
            let v: &'static str = match cmd.as_str() {
                "equip" => "equip",
                "eat" => "eat",
                _ => "drop",
            };
            sc.item(arg.parse().unwrap_or(0), v);
        }
        "tab" => {
            st.tab = match arg.as_str() {
                "skills" => 1,
                "gear" => 2,
                "quests" => 3,
                _ => 0,
            };
            sc.dirty = true;
        }
        "chat" => sc.chat_send(&arg),
        "type" => {
            st.typing = Some(arg.clone());
            sc.chat_dirty = true;
        }
        "ctx" => {
            if arg.is_empty() {
                if let Ok(w) = win.single() {
                    st.ctx_request = Some(ui::CtxReq::At(Vec2::new(w.width() * 0.5, w.height() * 0.5)));
                }
            } else {
                st.ctx_request = Some(ui::CtxReq::Kind(arg.clone()));
            }
        }
        "noctx" => st.ctx = None,
        "unhover" => st.fake_cursor = None,
        "ctxoverlap" => {
            // right-click where two drawn things overlap (shows the menu listing both)
            let sh = &pr.shown;
            let mut at = None;
            'outer: for (i, a) in sh.iter().enumerate() {
                for b in &sh[i + 1..] {
                    let (lo, hi) = (a.min.max(b.min), a.max.min(b.max));
                    let c = (lo + hi) * 0.5;
                    if hi.x - lo.x > 6.0 && hi.y - lo.y > 6.0 && c.x > 60.0 && c.y > 60.0 && c.x < 1000.0 && c.y < 500.0 {
                        at = Some(c);
                        break 'outer;
                    }
                }
            }
            match at {
                Some(c) => st.ctx_request = Some(ui::CtxReq::At(c)),
                None => warn!("mmo-script: nothing overlapping on screen"),
            }
        }
        "logpick" => {
            let wi = win.single().map(|w| format!("{}x{} logical, scale {}, {}x{} px", w.width(), w.height(), w.scale_factor(), w.physical_width(), w.physical_height())).unwrap_or_default();
            info!("mmo-pick cursor={:?} hover='{}' target={:?} window={wi}", st.cursor, st.hover, st.hover_target);
        }
        "click" | "hover" | "ctxon" | "report" => {
            // a left click on the middle of the nearest drawn thing of a kind (creature kinds, node
            // kinds, "colonist"), through the same picking a real click uses
            let mp = sc.my_pos().unwrap_or_default();
            let kind_of = |t: pick::Target| match t {
                pick::Target::Npc(id) => sc.npcs.get(&id).map(|a| a.kind.clone()),
                pick::Target::Player(id) => sc.players.get(&id).map(|a| match &a.role {
                    // "colonist" for any of them, or the role ("guard", "medic", ...)
                    Some(r) if arg == *r => r.clone(),
                    Some(_) => "colonist".into(),
                    None => "player".into(),
                }),
                pick::Target::Node(id) => sc.nodes.get(&id).map(|n| n.kind.clone()),
            };
            let visible = |s: &&pick::Shown| match (site.as_ref(), cam.single()) {
                (Some(site), Ok((c, g))) => !s.hidden(c, g, sc.gnd(site)),
                _ => true,
            };
            let best = pr.shown.iter().filter(|s| kind_of(s.target).as_deref() == Some(arg.as_str())).filter(visible).min_by(|a, b| a.base.distance_squared(mp).total_cmp(&b.base.distance_squared(mp)));
            if cmd == "report" {
                // where it is on screen (logical px = CSS px in the browser), for outside drivers
                match best {
                    Some(s) => info!("mmo-report {arg} {:.1} {:.1} {:?}", s.centre().x, s.centre().y, s.target),
                    None => warn!("mmo-script: no {arg} on screen to report"),
                }
                return;
            }
            if cmd == "ctxon" {
                // right-click on the middle of it
                st.ctx_request = best.map(|s| ui::CtxReq::At(s.centre()));
                return;
            }
            if cmd == "hover" {
                st.fake_cursor = best.map(|s| s.centre());
                if best.is_none() {
                    warn!("mmo-script: no {arg} on screen to hover");
                }
                return;
            }
            match (best, site.as_ref(), cam.single()) {
                (Some(s), Some(site), Ok(cam)) => {
                    let opts = options_at(&sc, cam, s.centre(), sc.gnd(site), &pr);
                    let o = opts.into_iter().next().expect("Cancel is always there");
                    info!("mmo-script: click {arg} at {:?} -> {} (wanted {:?}, got {:?})", s.centre(), o.label, s.target, o.target);
                    out.write(UiMsg { act: o.act, alt: false });
                }
                _ => warn!("mmo-script: no {arg} on screen to click"),
            }
        }
        "talk" => {
            // the nearest colonist (of a role, if given)
            let id = sc.players.iter().filter(|(_, a)| a.role.as_ref().is_some_and(|r| arg.is_empty() || *r == arg)).min_by_key(|(_, a)| chebyshev((a.tile.x, a.tile.y), (me.x, me.y))).map(|(i, _)| *i);
            match id {
                Some(id) => sc.talk(id),
                None => warn!("mmo-script: no colonist {arg}"),
            }
        }
        "choose" => {
            // a dialog option by id, or by number (1 = first)
            let opt = sc.dialog.as_ref().and_then(|d| match arg.parse::<usize>() {
                Ok(n) => d.options.get(n.saturating_sub(1)).map(|o| o.id.clone()),
                Err(_) => d.options.iter().find(|o| o.id == arg).map(|o| o.id.clone()),
            });
            match opt {
                Some(o) => sc.choose(&o),
                None => warn!("mmo-script: no dialog option {arg}"),
            }
        }
        "bank" | "pit" | "ladder" | "locker" => {
            let kind = if cmd == "bank" { "locker" } else { cmd.as_str() };
            match nearest_node(&sc, kind) {
                Some(id) => sc.act_node(id),
                None => warn!("mmo-script: no {kind}"),
            }
        }
        "deposit" | "withdraw" | "offer" => {
            // SLOT[:all|N]
            let (a, q) = arg.split_once(':').unwrap_or((arg.as_str(), "1"));
            let i: usize = a.parse().unwrap_or(0);
            let q = if q == "all" { Qty::All } else { Qty::N(q.parse().unwrap_or(1)) };
            match cmd.as_str() {
                "deposit" => sc.bank_deposit(i, q),
                "withdraw" => sc.bank_withdraw(i, q),
                _ => sc.trade_offer(i, q),
            }
        }
        "closebank" => {
            sc.bank = None;
            sc.dirty = true;
        }
        "trade" => {
            // ask the nearest other (human) player to trade
            let id = sc.players.iter().filter(|(i, a)| a.role.is_none() && Some(**i) != sc.me).min_by_key(|(_, a)| chebyshev((a.tile.x, a.tile.y), (me.x, me.y))).map(|(i, _)| *i);
            match id {
                Some(id) => sc.trade_with(id),
                None => warn!("mmo-script: nobody to trade with"),
            }
        }
        "accept" => {
            if sc.trade_req.is_some() && sc.trade.is_none() {
                out.write(UiMsg { act: UiAct::TradeReq(true), alt: false });
            } else {
                sc.trade_accept();
            }
        }
        "decline" => sc.trade_decline(),
        "wear" => sc.item(arg.parse().unwrap_or(0), "wear"),
        "unwear" => sc.unequip_gear(&arg),
        "lamp" => sc.item(arg.parse().unwrap_or(0), "rub"),
        "rub" => {
            // SLOT:SKILL
            if let Some((a, k)) = arg.split_once(':') {
                sc.lamp = a.parse().ok();
                if let Some(k) = Skill::parse(k) {
                    sc.rub(k);
                }
            }
        }
        "surge" => {
            // offline cheat: start a Rift Surge now ("now") or bring its boss ("boss")
            if let Link::Offline(sim) = &mut sc.link {
                sim.cheat_surge(&arg);
            }
        }
        "clicktest" => commands.insert_resource(pick::ClickTest(Some(if arg.is_empty() { format!("step{}", run.i - 1) } else { arg.clone() }))),
        "rotate" => {
            if let Some(o) = orbit.as_mut() {
                o.yaw = arg.parse::<f32>().unwrap_or(0.0).to_radians();
            }
        }
        "pitch" => {
            if let Some(o) = orbit.as_mut() {
                o.pitch = arg.parse::<f32>().unwrap_or(35.0).to_radians();
            }
        }
        "zoom" => {
            if let Some(o) = orbit.as_mut() {
                o.dist = arg.parse().unwrap_or(15.0);
            }
        }
        "run" => {
            sc.run = arg != "off";
            sc.dirty = true;
        }
        "give" | "xp" => {
            if let Link::Offline(sim) = &mut sc.link {
                let mut msgs = Vec::new();
                if cmd == "give" {
                    msgs = sim.cheat_give(&arg);
                } else if let Some((k, v)) = arg.split_once(':') {
                    msgs = sim.cheat_xp(k, v.parse().unwrap_or(0.0));
                }
                for m in msgs {
                    match m {
                        S2C::Inv { mut inv } => {
                            inv.resize(INV_SLOTS, None);
                            sc.inv = inv;
                        }
                        S2C::Quest { id, state, step, progress, qp, .. } => {
                            sc.quests.insert(id, QuestSt { s: state, step, p: progress });
                            sc.qp = qp;
                        }
                        S2C::Xp { skill, xp, .. } => {
                            if let Some(s) = Skill::parse(&skill) {
                                sc.skills.set(s, xp);
                            }
                        }
                        S2C::Hp { hp, max } => {
                            sc.hp = hp;
                            sc.max_hp = max;
                        }
                        _ => {}
                    }
                }
                sc.dirty = true;
            }
        }
        "log" => {
            let me_a = sc.me.and_then(|m| sc.players.get(&m));
            let alive = sc.npcs.values().filter(|a| a.anim != anim::DEAD).count();
            info!(
                "mmo-script: log2: room={} cave={} gear={:?} armour={} qp={} quests={:?} bank={:?} trade={:?} dialog={:?} surge={:?}",
                sc.room,
                sc.cave.is_some(),
                sc.gear,
                sc.armour,
                sc.qp,
                sc.quests.iter().map(|(k, v)| format!("{k}:{}:{}:{}", v.s, v.step, v.p)).collect::<Vec<_>>(),
                sc.bank.as_ref().map(|b| b.iter().map(|s| format!("{}x{}", s.item, s.qty)).collect::<Vec<_>>()),
                sc.trade.as_ref().map(|t| (t.stage, t.mine.len(), t.theirs.len(), t.me_ok, t.them_ok)),
                sc.dialog.as_ref().map(|d| (d.name.clone(), d.options.iter().map(|o| o.id.clone()).collect::<Vec<_>>())),
                sc.surge.as_ref().map(|v| (v.phase.clone(), v.at, v.ends_tick.saturating_sub(sc.tick_n)))
            );
            info!(
                "mmo-script: log: tick={} tile={:?} anim={} hp={}/{} weapon={:?} inv={} skills=[{}] players={} creatures_alive={}/{} nodes_up={}/{} status='{}'",
                sc.tick_n,
                me_a.map(|a| a.tile),
                me_a.map_or(0, |a| a.anim),
                sc.hp,
                sc.max_hp,
                sc.weapon,
                sc.inv.iter().flatten().map(|s| s.item.as_str()).collect::<Vec<_>>().join("+"),
                SKILLS.iter().map(|s| format!("{}:{}({})", s.key(), sc.skills.level(*s), fmt_xp(sc.skills.get(*s)))).collect::<Vec<_>>().join(" "),
                sc.players.len(),
                alive,
                sc.npcs.len(),
                sc.nodes.values().filter(|n| n.up).count(),
                sc.nodes.len(),
                sc.status
            );
            for (t, _) in sc.chat.iter().rev().take(6).collect::<Vec<_>>().into_iter().rev() {
                info!("mmo-script: chat: {t}");
            }
        }
        "shot" => {
            let flag = Arc::new(AtomicBool::new(false));
            let f2 = flag.clone();
            let p = arg.clone();
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(arg.clone())).observe(move |_: On<ScreenshotCaptured>| {
                info!("mmo-script: screenshot saved to {p}");
                f2.store(true, Ordering::Relaxed);
            });
            run.shot = Some(flag);
        }
        "quit" => {
            exit.write(AppExit::Success);
        }
        other => warn!("mmo-script: unknown step {other}"),
    }
}

// ------------------------------------------------------------------------------------ lava tube

/// The headlamp that lights the tube around you.
#[derive(Component)]
pub struct Headlamp;

/// What `cave_env` changed, to put back on the way out.
#[derive(Default)]
pub struct EnvSaved {
    vis: Vec<(Entity, Visibility)>,
    sun: Vec<(Entity, f32)>,
    fog: Vec<(Entity, DistanceFog)>,
    ambient: Option<GlobalAmbientLight>,
    clear: Color,
    lamp: Option<Entity>,
}

/// Underground: hide the terrain and sky, put out the sun, darken the air, light a headlamp.
/// Everything comes back when you climb out (or leave the mode).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn cave_env(
    mut commands: Commands,
    scape: Option<Res<Scape>>,
    render: Option<Res<crate::terrain::TerrainRender>>,
    sky: Query<Entity, With<crate::visuals::SkyDome>>,
    mut vis: Query<&mut Visibility>,
    mut sun: Query<(Entity, &mut DirectionalLight)>,
    mut fog: Query<(Entity, &mut DistanceFog)>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut clear: ResMut<ClearColor>,
    mut lamp: Query<&mut Transform, With<Headlamp>>,
    mut saved: Local<Option<EnvSaved>>,
) {
    let want = scape.as_ref().is_some_and(|s| s.cave.is_some());
    match (want, saved.is_some()) {
        (true, false) => {
            let mut sv = EnvSaved { clear: clear.0, ambient: Some(ambient.clone()), ..default() };
            let hide: Vec<Entity> = render.as_ref().map(|r| r.coarse.values().copied().chain(r.fine.values().map(|f| f.0)).collect()).unwrap_or_default();
            for e in hide.into_iter().chain(sky.iter()) {
                if let Ok(mut v) = vis.get_mut(e) {
                    sv.vis.push((e, *v));
                    *v = Visibility::Hidden;
                }
            }
            for (e, mut l) in &mut sun {
                sv.sun.push((e, l.illuminance));
                l.illuminance = 0.0;
            }
            for (e, mut f) in &mut fog {
                sv.fog.push((e, f.clone()));
                f.color = Color::srgb(0.0, 0.0, 0.0);
                f.directional_light_color = Color::NONE;
                f.falloff = FogFalloff::Linear { start: 20.0, end: 85.0 };
            }
            ambient.color = Color::srgb(0.45, 0.55, 0.85);
            ambient.brightness = 170.0;
            clear.0 = Color::BLACK;
            // tuning: MM_LAMP="intensity,range"
            let (int, range) = std::env::var("MM_LAMP").ok().and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).unwrap_or((2_500_000.0f32, 45.0f32));
            sv.lamp = Some(
                commands
                    .spawn((Headlamp, PointLight { color: Color::srgb(1.0, 0.92, 0.80), intensity: int, range, shadow_maps_enabled: false, ..default() }, Transform::default()))
                    .id(),
            );
            *saved = Some(sv);
        }
        (false, true) => {
            let sv = saved.take().unwrap();
            for (e, v) in sv.vis {
                if let Ok(mut x) = vis.get_mut(e) {
                    *x = v;
                }
            }
            for (e, i) in sv.sun {
                if let Ok((_, mut l)) = sun.get_mut(e) {
                    l.illuminance = i;
                }
            }
            for (e, f) in sv.fog {
                if let Ok((_, mut x)) = fog.get_mut(e) {
                    *x = f;
                }
            }
            if let Some(a) = sv.ambient {
                *ambient = a;
            }
            clear.0 = sv.clear;
            if let Some(l) = sv.lamp {
                if let Ok(mut e) = commands.get_entity(l) {
                    e.despawn();
                }
            }
        }
        _ => {}
    }
    // the lamp rides above your head
    if let (Some(sc), Ok(mut t)) = (scape.as_ref(), lamp.single_mut()) {
        if let Some(p) = sc.my_pos() {
            // high above you: a wide, even pool rather than a hot spot at your feet
            t.translation = p + Vec3::Y * std::env::var("MM_LAMP_H").ok().and_then(|v| v.parse().ok()).unwrap_or(6.5f32);
        }
    }
}

/// Dress my astronaut in the armour I wear (others' armour isn't sent).
#[allow(clippy::too_many_arguments)]
fn dress_me(
    mut commands: Commands,
    scape: Option<ResMut<Scape>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut kit: ResMut<Kit>,
    kids: Query<&Children>,
    parts: Query<&look::Part>,
    pieces: Query<(), With<look::ArmourPiece>>,
) {
    let Some(mut sc) = scape else { return };
    if !sc.gear_dirty {
        return;
    }
    let Some(root) = sc.me.and_then(|m| sc.players.get(&m)).map(|a| a.e) else { return };
    // wait until the model's parts exist
    if kids.get(root).is_err() {
        return;
    }
    let mut arm = None;
    for d in kids.iter_descendants(root) {
        if pieces.contains(d) {
            commands.entity(d).despawn();
        } else if parts.get(d).is_ok_and(|p| p.limb == look::Limb::ArmL) {
            arm = Some(d);
        }
    }
    let gear = sc.gear.clone();
    look::dress(&mut Ctx { commands: &mut commands, meshes: &mut meshes, mats: &mut mats, kit: &mut kit }, root, arm, &gear);
    sc.gear_dirty = false;
}
