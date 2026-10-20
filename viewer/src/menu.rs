//! Main menu and pause menu (bevy_ui): disclaimer, map picker (local sites + the 1,262 live NASA
//! DTMs, with search, filters and a Mars lat/lon map), mode picker, controls, Play.
//!
//! Flow: with no site on the command line / URL (or with `--menu`) the game opens on this menu.
//! PLAY rewrites `Args` (site / live id / mode) and asks main.rs to start a load (`StartLoad`).
//! In game, Esc (or the `||` button, for touch) opens the pause menu over the frozen game
//! (`Time<Virtual>` is paused, so physics stops). Changing map unloads first: every root scene entity
//! spawned after startup is despawned (terrain chunks, buggy, anything another module spawned for the
//! site) and the site resources are removed, which frees their meshes / textures / heightfield;
//! then the usual loading path runs. Other modules that keep per-site state in resources should
//! read the `SiteUnloaded` message and drop it; entities spawned after startup that must survive a
//! map change need the `Keep` marker.
//!
//! Adding a driving mode to the menu: one line in `MODES`. Its card is enabled as soon as
//! `buggy::Mode::parse(key)` knows the key; until then it shows as "coming soon".
//!
//! Test hook: `--menu-script "page:map,search:dunes,site:ID,mode:real,play,loaded@8,shot:a.png,esc,..."`
//! (see `ScriptRun`).

use crate::buggy::{Buggy, Controls, Mode};
use crate::sites::{self, Entry, FILTERS, Where};
use crate::terrain::{Site, TerrainRender};
use crate::{Args, LoadFailed, Loading, RunClock, StartLoad};
use bevy::diagnostic::DiagnosticsStore;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::picking::hover::Hovered;
use bevy::picking::prelude::*;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::tasks::{IoTaskPool, Task, futures::check_ready};
use bevy::window::PrimaryWindow;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ------------------------------------------------------------------------------------ modes

pub struct ModeEntry {
    /// `buggy::Mode::parse` key; the card is enabled when that returns Some
    pub key: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
    /// switching to or from this mode reloads the site (it changes the terrain)
    pub reload: bool,
}

/// The menu's mode list. One line per mode.
pub const MODES: &[ModeEntry] = &[
    ModeEntry { key: "fun", label: "FUN", blurb: "Arcade handling that ignores real physics: big grip and power, Shift boost, Ctrl/J jump, air control, auto-flip.", reload: false },
    ModeEntry { key: "real", label: "REAL", blurb: "Mars as it is: 3.71 m/s2 gravity, loose-sand tyres, a light buggy's real power, no assists. Still a game model.", reload: false },
    ModeEntry { key: "sand", label: "SAND", blurb: "Experimental: moving dunes, simulated. The sand reshapes as you drive, so the terrain stops matching the real data.", reload: true },
];

fn mode_available(i: usize) -> bool {
    MODES.get(i).is_some_and(|m| Mode::parse(m.key).is_some())
}

fn mode_index(m: Mode) -> usize {
    MODES.iter().position(|e| Mode::parse(e.key) == Some(m)).unwrap_or(0)
}

/// Terrain smoothing choices (`--smooth`, Gaussian sigma in m; None = terrain.rs default 2 m).
/// Changing it reloads the site.
const SMOOTHS: [(Option<f32>, &str); 4] = [(Some(0.0), "raw DTM"), (Some(1.0), "1 m"), (None, "2 m (default)"), (Some(4.0), "4 m")];

fn smooth_index(s: Option<f32>) -> usize {
    SMOOTHS.iter().position(|x| x.0 == s).or_else(|| s.filter(|v| *v == crate::terrain::SMOOTH_M).map(|_| 2)).unwrap_or(2)
}

const REPO_URL: &str = "https://github.com/zombodotcom/marsmaker";

// ------------------------------------------------------------------------------------ colours

const ACCENT: Color = Color::srgb(0.96, 0.46, 0.16);
const ACCENT_HI: Color = Color::srgb(1.0, 0.58, 0.28);
const TEXT: Color = Color::srgb(0.98, 0.93, 0.86);
const DIM: Color = Color::srgb(0.72, 0.62, 0.54);
const DARK_TEXT: Color = Color::srgb(0.12, 0.06, 0.03);
const PANEL: Color = Color::srgba(0.13, 0.075, 0.055, 0.96);
const PANEL_EDGE: Color = Color::srgb(0.36, 0.19, 0.11);
const BTN: Color = Color::srgb(0.22, 0.125, 0.085);
const BTN_HOVER: Color = Color::srgb(0.33, 0.18, 0.11);
const BTN_SEL: Color = Color::srgb(0.50, 0.22, 0.08);
const LIVE_DOT: Color = Color::srgba(0.98, 0.55, 0.22, 0.75);
const LOCAL_DOT: Color = Color::srgb(0.45, 0.95, 0.85);

// ------------------------------------------------------------------------------------ state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Pause,
    About,
    Map,
    Mode,
    Controls,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Page::Pause => "PAUSED",
            Page::About => "ABOUT",
            Page::Map => "MAP",
            Page::Mode => "MODE",
            Page::Controls => "CONTROLS",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pause" => Page::Pause,
            "about" => Page::About,
            "map" => Page::Map,
            "mode" => Page::Mode,
            "controls" => Page::Controls,
            _ => return None,
        })
    }
}

/// Source filter: everything, converted sites on disk, live from NASA.
const SOURCES: [&str; 3] = ["all sources", "on disk", "live from NASA"];

#[derive(Resource)]
pub struct Menu {
    pub open: bool,
    page: Page,
    /// a site was started (pause-menu context)
    in_game: bool,
    /// what is loaded / loading now
    playing: Option<Where>,
    playing_mode: usize,
    sel_site: Option<usize>,
    sel_mode: usize,
    sel_smooth: usize,
    playing_smooth: usize,
    query: String,
    filter: usize,
    source: usize,
    filtered: Vec<usize>,
    scroll: usize,
    focus: usize,
    rows: usize,
    narrow: bool,
    /// UI needs a full rebuild / list + details refresh / dots refresh
    dirty: bool,
    dirty_list: bool,
    dirty_dots: bool,
    drag_acc: f32,
    /// graphics quality the renderer started with (`--low`)
    low: bool,
}

impl Menu {
    fn tabs(&self) -> Vec<Page> {
        let mut t = vec![Page::About, Page::Map, Page::Mode, Page::Controls];
        if self.in_game {
            t.insert(0, Page::Pause);
        }
        t
    }
    fn set_page(&mut self, p: Page) {
        if self.page != p || !self.open {
            self.page = p;
            self.focus = 0;
            self.dirty = true;
        }
        self.open = true;
    }
    fn reload_needed(&self, cat: &Catalog) -> bool {
        let site = self.sel_site.and_then(|i| cat.entries.get(i)).map(|e| &e.at);
        !self.in_game || site != self.playing.as_ref() || self.sel_smooth != self.playing_smooth || (self.sel_mode != self.playing_mode && (MODES[self.sel_mode].reload || MODES[self.playing_mode].reload))
    }
}

#[derive(Resource, Default)]
pub struct Catalog {
    pub entries: Vec<Entry>,
    notes: Vec<String>,
    task: Option<Task<(Vec<Entry>, Vec<String>)>>,
    ready: bool,
}

/// Sent when the current site is unloaded (map change); drop any per-site state you keep.
#[derive(Message, Clone, Copy, Debug)]
pub struct SiteUnloaded;

/// Entities spawned after startup that must survive a map change.
#[derive(Component)]
pub struct Keep;

/// Entities that existed after startup (camera, light, HUD, ...): never despawned by a map change.
#[derive(Resource, Default)]
struct Persistent(HashSet<Entity>);

/// Map change in progress: 0 = despawn now, 1..=2 = let asset frees settle, then start the load.
#[derive(Resource)]
struct Switch(u8);

// ------------------------------------------------------------------------------------ UI components

#[derive(Message, Clone, Debug, PartialEq)]
enum Msg {
    Page(Page),
    Row(usize),
    Site(usize),
    Mode(usize),
    Smooth(usize),
    /// graphics quality: true = low (`--low`); takes a restart
    Graphics(bool),
    Filter(usize),
    Source(usize),
    Search(String),
    Scroll(i32),
    Esc,
    Play,
    Resume,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))] // no Quit button in the browser
    Quit,
    Link,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Normal,
    Tab,
    Chip,
    Row,
    Card,
}

#[derive(Component, Clone)]
struct Btn {
    msg: Msg,
    kind: Kind,
}

#[derive(Component)]
struct Focusable(usize);
#[derive(Component)]
struct MenuRoot;
#[derive(Component)]
struct PauseButton;
#[derive(Component)]
struct ListBox;
#[derive(Component)]
struct RowTitle(usize);
#[derive(Component)]
struct RowSub(usize);
#[derive(Component)]
struct RowTag(usize);
#[derive(Component)]
struct Details;
#[derive(Component)]
struct SearchText;
#[derive(Component)]
struct ListStatus;
#[derive(Component)]
struct ScrollBar;
#[derive(Component)]
struct MapBox;
#[derive(Component)]
struct Dot(usize);
#[derive(Component)]
struct SelMarker;
#[derive(Component)]
struct Summary;
#[derive(Component)]
struct PlayLabel;

// ------------------------------------------------------------------------------------ plugin

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins(bevy::diagnostic::SystemInformationDiagnosticsPlugin);
        app.add_message::<Msg>()
            .add_message::<SiteUnloaded>()
            .init_resource::<Catalog>()
            .init_resource::<Persistent>()
            .add_systems(Startup, setup_menu)
            .add_systems(PostStartup, snapshot_persistent)
            .add_systems(
                Update,
                (
                    poll_catalog,
                    run_script,
                    keys,
                    apply_msgs,
                    switch_map,
                    pause_time,
                    rebuild,
                    refresh_list,
                    refresh_dots,
                    style_buttons,
                    pause_button_vis,
                )
                    .chain(),
            );
    }
}

/// Run condition for gameplay input / camera: false while a menu is open.
pub fn playing(menu: Option<Res<Menu>>) -> bool {
    menu.is_none_or(|m| !m.open)
}

fn setup_menu(mut commands: Commands, args: Res<Args>, mut cat: ResMut<Catalog>) {
    let (dir, index) = (args.sites_dir.clone(), args.live_index.clone());
    cat.task = Some(IoTaskPool::get().spawn(sites::load_catalog(dir, index)));
    let started = !args.menu;
    let playing = started.then(|| match &args.live {
        Some(id) => Where::Live(id.clone()),
        None => Where::Local(args.site.clone()),
    });
    let mode = mode_index(args.mode);
    commands.insert_resource(Menu {
        open: args.menu,
        page: Page::About,
        in_game: started,
        playing,
        playing_mode: mode,
        sel_site: None,
        sel_mode: mode,
        sel_smooth: smooth_index(args.smooth),
        playing_smooth: smooth_index(args.smooth),
        query: String::new(),
        filter: 0,
        source: 0,
        filtered: Vec::new(),
        scroll: 0,
        focus: 0,
        rows: 8,
        narrow: false,
        dirty: true,
        dirty_list: true,
        dirty_dots: true,
        drag_acc: 0.0,
        low: args.low,
    });
    if let Some(s) = &args.menu_script {
        commands.insert_resource(ScriptRun::parse(s));
    }
    // in-game menu button (touch has no Esc), top right under the page's "All sites" link
    commands
        .spawn((
            PauseButton,
            Btn { msg: Msg::Esc, kind: Kind::Normal },
            Hovered::default(),
            Outline::new(Val::Px(2.0), Val::Px(1.0), Color::NONE),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(46.0),
                right: Val::Px(14.0),
                width: Val::Px(48.0),
                height: Val::Px(40.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(BTN.with_alpha(0.8)),
            BorderColor::all(PANEL_EDGE),
            GlobalZIndex(5),
            Visibility::Hidden,
        ))
        .observe(on_click)
        .with_children(|p| {
            p.spawn(label("||", 18.0, TEXT));
        });
}

fn snapshot_persistent(mut p: ResMut<Persistent>, all: Query<Entity>) {
    p.0 = all.iter().collect();
}

fn poll_catalog(mut cat: ResMut<Catalog>, mut menu: ResMut<Menu>, args: Res<Args>) {
    let Some(task) = cat.task.as_mut() else { return };
    let Some((entries, notes)) = check_ready(task) else { return };
    cat.task = None;
    for n in &notes {
        warn!("site list: {n}");
    }
    info!(
        "site list: {} sites on disk, {} live from NASA",
        entries.iter().filter(|e| !e.is_live()).count(),
        entries.iter().filter(|e| e.is_live()).count()
    );
    cat.entries = entries;
    cat.notes = notes;
    cat.ready = true;
    // select what is playing, or the default site (Russell Crater megadune)
    let want = menu.playing.clone().unwrap_or_else(|| match &args.live {
        Some(id) => Where::Live(id.clone()),
        None => Where::Local(args.site.clone()),
    });
    let name = |w: &Where| match w {
        Where::Live(id) => id.clone(),
        Where::Local(p) => p.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").to_string(),
    };
    let wid = name(&want);
    menu.sel_site = cat
        .entries
        .iter()
        .position(|e| e.at == want)
        .or_else(|| cat.entries.iter().position(|e| e.id == wid && e.is_live() == matches!(want, Where::Live(_))))
        .or_else(|| cat.entries.iter().position(|e| e.id == wid))
        .or_else(|| cat.entries.iter().position(|e| e.id == crate::DEFAULT_SITE_NAME))
        .or((!cat.entries.is_empty()).then_some(0));
    if menu.playing.is_some() {
        // keep `playing` comparable with catalogue entries (same path spelling)
        if let Some(e) = menu.sel_site.and_then(|i| cat.entries.get(i)) {
            if name(&e.at) == wid && e.is_live() == matches!(want, Where::Live(_)) {
                menu.playing = Some(e.at.clone());
            }
        }
    }
    refilter(&mut menu, &cat);
    if let Some(s) = menu.sel_site {
        if let Some(k) = menu.filtered.iter().position(|&i| i == s) {
            menu.scroll = k.saturating_sub(2);
        }
    }
    menu.dirty = true;
}

fn refilter(menu: &mut Menu, cat: &Catalog) {
    let words: Vec<String> = menu.query.to_lowercase().split_whitespace().map(String::from).collect();
    menu.filtered = (0..cat.entries.len())
        .filter(|&i| {
            let e = &cat.entries[i];
            let src = match menu.source {
                1 => !e.is_live(),
                2 => e.is_live(),
                _ => true,
            };
            src && sites::matches(e, menu.filter, &words)
        })
        .collect();
    let max = menu.filtered.len().saturating_sub(menu.rows);
    menu.scroll = menu.scroll.min(max);
    menu.dirty_list = true;
    menu.dirty_dots = true;
}

// ------------------------------------------------------------------------------------ input

fn on_click(mut ev: On<PointerClick>, q: Query<&Btn>, mut out: MessageWriter<Msg>) {
    let Ok(b) = q.get(ev.entity) else { return };
    ev.propagate(false);
    if ev.button != PointerButton::Primary {
        return;
    }
    out.write(b.msg.clone());
    if b.kind == Kind::Row && ev.count >= 2 {
        out.write(Msg::Play);
    }
}

fn on_list_scroll(mut ev: On<PointerScroll>, mut out: MessageWriter<Msg>) {
    ev.propagate(false);
    let lines = match ev.unit {
        bevy::input::mouse::MouseScrollUnit::Line => ev.y,
        bevy::input::mouse::MouseScrollUnit::Pixel => ev.y / 40.0,
    };
    let n = (-lines * 2.0).round() as i32;
    if n != 0 {
        out.write(Msg::Scroll(n));
    }
}

fn on_list_drag(ev: On<PointerDrag>, mut menu: ResMut<Menu>, mut out: MessageWriter<Msg>) {
    menu.drag_acc += ev.delta.y;
    let n = (menu.drag_acc / ROW_H).trunc();
    if n != 0.0 {
        menu.drag_acc -= n * ROW_H;
        out.write(Msg::Scroll(-n as i32));
    }
}

fn on_map_click(ev: On<PointerClick>, menu: Res<Menu>, cat: Res<Catalog>, mut out: MessageWriter<Msg>) {
    let Some(p) = ev.hit.position else { return };
    // normalized node position (-0.5..0.5) -> lon / lat
    let (lon, lat) = ((p.x as f64 + 0.5) * 360.0 - 180.0, 90.0 - (p.y as f64 + 0.5) * 180.0);
    let best = menu
        .filtered
        .iter()
        .map(|&i| {
            let e = &cat.entries[i];
            let d = ((e.lon - lon) / 2.0).powi(2) + (e.lat - lat).powi(2);
            (d, i)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((d, i)) = best {
        if d < 15.0f64.powi(2) {
            out.write(Msg::Site(i));
        }
    }
}

fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut kb: MessageReader<KeyboardInput>,
    mut menu: ResMut<Menu>,
    focus: Query<(&Focusable, &Btn)>,
    mut out: MessageWriter<Msg>,
) {
    let typed: Vec<KeyboardInput> = kb.read().cloned().collect();
    if keys.just_pressed(KeyCode::Escape) {
        out.write(Msg::Esc);
        return;
    }
    if !menu.open {
        return;
    }
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if keys.just_pressed(KeyCode::Tab) {
        let tabs = menu.tabs();
        let i = tabs.iter().position(|p| *p == menu.page).unwrap_or(0);
        let n = tabs.len();
        out.write(Msg::Page(tabs[if shift { (i + n - 1) % n } else { (i + 1) % n }]));
        return;
    }
    if menu.page == Page::Map {
        let cur = menu.sel_site.and_then(|s| menu.filtered.iter().position(|&i| i == s));
        let step = |d: i64| {
            let n = menu.filtered.len() as i64;
            (n > 0).then(|| menu.filtered[cur.map_or(0, |c| (c as i64 + d).clamp(0, n - 1)) as usize])
        };
        let mv = [
            (KeyCode::ArrowDown, 1),
            (KeyCode::ArrowUp, -1),
            (KeyCode::PageDown, menu.rows as i64),
            (KeyCode::PageUp, -(menu.rows as i64)),
        ];
        for (k, d) in mv {
            if keys.just_pressed(k) {
                if let Some(i) = step(d) {
                    out.write(Msg::Site(i));
                }
            }
        }
        if keys.just_pressed(KeyCode::ArrowRight) {
            out.write(Msg::Filter((menu.filter + 1) % FILTERS.len()));
        }
        if keys.just_pressed(KeyCode::ArrowLeft) {
            out.write(Msg::Filter((menu.filter + FILTERS.len() - 1) % FILTERS.len()));
        }
        if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]) {
            out.write(Msg::Play);
        }
        let mut q = menu.query.clone();
        for ev in typed.iter().filter(|e| e.state.is_pressed()) {
            match &ev.logical_key {
                Key::Backspace => {
                    q.pop();
                }
                _ => {
                    if let Some(t) = &ev.text {
                        q.extend(t.chars().filter(|c| !c.is_control()));
                    }
                }
            }
        }
        if q != menu.query && q.len() <= 40 {
            out.write(Msg::Search(q));
        }
        return;
    }
    // other pages: arrows move the focus ring, Enter / Space activates
    let n = focus.iter().count();
    if n == 0 {
        return;
    }
    let next = keys.any_just_pressed([KeyCode::ArrowDown, KeyCode::ArrowRight]);
    let prev = keys.any_just_pressed([KeyCode::ArrowUp, KeyCode::ArrowLeft]);
    if next || prev {
        menu.focus = if next { (menu.focus + 1) % n } else { (menu.focus + n - 1) % n };
    }
    if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter, KeyCode::Space]) {
        if let Some((_, b)) = focus.iter().find(|(f, _)| f.0 == menu.focus) {
            out.write(b.msg.clone());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_msgs(
    mut commands: Commands,
    mut msgs: MessageReader<Msg>,
    mut menu: ResMut<Menu>,
    cat: Res<Catalog>,
    mut args: ResMut<Args>,
    mut buggy: Option<ResMut<Buggy>>,
    loading: Option<Res<Loading>>,
    failed: Option<Res<LoadFailed>>,
    mut ctl: ResMut<Controls>,
    mut exit: MessageWriter<AppExit>,
) {
    for m in msgs.read().cloned().collect::<Vec<_>>() {
        match m {
            Msg::Page(p) => menu.set_page(p),
            Msg::Esc => {
                let game = menu.in_game || loading.is_some() || failed.is_some();
                if !menu.open {
                    menu.set_page(if game { Page::Pause } else { Page::About });
                } else if game && menu.page == Page::Pause && failed.is_none() {
                    menu.open = false;
                    menu.dirty = true;
                } else if game {
                    menu.set_page(Page::Pause);
                }
            }
            Msg::Resume => {
                if menu.in_game {
                    menu.open = false;
                    menu.dirty = true;
                }
            }
            Msg::Row(r) => {
                if let Some(&i) = menu.filtered.get(menu.scroll + r) {
                    menu.sel_site = Some(i);
                    menu.dirty_list = true;
                    menu.dirty_dots = true;
                }
            }
            Msg::Site(i) => {
                if i < cat.entries.len() {
                    menu.sel_site = Some(i);
                    if let Some(k) = menu.filtered.iter().position(|&x| x == i) {
                        if k < menu.scroll {
                            menu.scroll = k;
                        } else if k >= menu.scroll + menu.rows {
                            menu.scroll = k + 1 - menu.rows;
                        }
                    }
                    menu.dirty_list = true;
                    menu.dirty_dots = true;
                }
            }
            Msg::Mode(i) => {
                if mode_available(i) {
                    menu.sel_mode = i;
                    menu.dirty_list = true;
                }
            }
            Msg::Smooth(i) => {
                menu.sel_smooth = i.min(SMOOTHS.len() - 1);
                menu.dirty_list = true;
            }
            Msg::Graphics(low) => {
                if low != args.low {
                    restart_with_quality(low);
                    exit.write(AppExit::Success);
                }
            }
            Msg::Filter(f) => {
                menu.filter = f.min(FILTERS.len() - 1);
                menu.scroll = 0;
                refilter(&mut menu, &cat);
            }
            Msg::Source(s) => {
                menu.source = s.min(SOURCES.len() - 1);
                menu.scroll = 0;
                refilter(&mut menu, &cat);
            }
            Msg::Search(q) => {
                menu.query = q;
                menu.scroll = 0;
                refilter(&mut menu, &cat);
            }
            Msg::Scroll(n) => {
                let max = menu.filtered.len().saturating_sub(menu.rows) as i64;
                menu.scroll = (menu.scroll as i64 + n as i64).clamp(0, max) as usize;
                menu.dirty_list = true;
            }
            Msg::Quit => {
                exit.write(AppExit::Success);
            }
            Msg::Link => open_link(REPO_URL),
            Msg::Play => {
                let Some(e) = menu.sel_site.and_then(|i| cat.entries.get(i)).cloned() else { continue };
                let Some(mode) = Mode::parse(MODES[menu.sel_mode].key) else { continue };
                args.mode = mode;
                args.smooth = SMOOTHS[menu.sel_smooth].0;
                *ctl = Controls::default();
                if !menu.reload_needed(&cat) && failed.is_none() {
                    // same site: just switch the driving mode and carry on
                    if let Some(b) = buggy.as_mut() {
                        if b.tuning.mode != mode {
                            b.set_mode(mode);
                        }
                    }
                    info!("menu: continue on {} in {} mode", e.id, MODES[menu.sel_mode].label);
                } else {
                    info!("menu: load {} ({}) in {} mode", e.id, if e.is_live() { "live from NASA" } else { "on disk" }, MODES[menu.sel_mode].label);
                    match &e.at {
                        Where::Local(p) => {
                            args.site = p.clone();
                            args.live = None;
                        }
                        Where::Live(id) => args.live = Some(id.clone()),
                    }
                    args.spawn = None;
                    commands.insert_resource(Switch(0));
                    menu.playing = Some(e.at.clone());
                    menu.in_game = true;
                }
                menu.playing_mode = menu.sel_mode;
                menu.playing_smooth = menu.sel_smooth;
                menu.open = false;
                menu.dirty = true;
            }
        }
    }
}

/// Graphics HIGH / LOW is chosen when the renderer starts (visuals.rs), so switching restarts:
/// natively the viewer relaunches itself with / without `--low`, in the browser the page reloads
/// with / without `?low`.
fn restart_with_quality(low: bool) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut argv: Vec<String> = std::env::args().skip(1).filter(|a| a != "--low").collect();
        if low {
            argv.push("--low".into());
        }
        if let Ok(exe) = std::env::current_exe() {
            info!("graphics -> {}: restarting {} {argv:?}", if low { "LOW" } else { "HIGH" }, exe.display());
            let _ = std::process::Command::new(exe).args(argv).spawn();
        }
    }
    #[cfg(target_arch = "wasm32")]
    if let Some(w) = web_sys::window() {
        let loc = w.location();
        let search = loc.search().unwrap_or_default();
        let mut kv: Vec<&str> = search.trim_start_matches('?').split('&').filter(|s| !s.is_empty() && *s != "low" && !s.starts_with("low=")).collect();
        if low {
            kv.push("low");
        }
        let _ = loc.set_search(&kv.join("&"));
    }
}

fn open_link(url: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(url, "_blank");
    }
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// While any menu is open the game clock (and with it physics) stands still.
fn pause_time(menu: Res<Menu>, mut vt: ResMut<Time<Virtual>>, mut was_open: Local<bool>, mut ctl: ResMut<Controls>, mut hud: Query<&mut Visibility, With<crate::Hud>>) {
    // the driving HUD would show through the veil
    let hv = if menu.open { Visibility::Hidden } else { Visibility::Inherited };
    for mut v in &mut hud {
        if *v != hv {
            *v = hv;
        }
    }
    if menu.open && !vt.is_paused() {
        vt.pause();
    } else if !menu.open && vt.is_paused() {
        vt.unpause();
    }
    if *was_open && !menu.open {
        *ctl = Controls::default(); // keys typed in the menu must not reach the buggy
    }
    *was_open = menu.open;
}

// ------------------------------------------------------------------------------------ map switch

#[derive(bevy::ecs::system::SystemParam)]
struct Counts<'w, 's> {
    all: Query<'w, 's, ()>,
    meshes: Res<'w, Assets<Mesh>>,
    images: Res<'w, Assets<Image>>,
    materials: Res<'w, Assets<StandardMaterial>>,
    diag: Option<Res<'w, DiagnosticsStore>>,
}

impl Counts<'_, '_> {
    fn line(&self) -> String {
        let mem = self
            .diag
            .as_ref()
            .and_then(|d| d.get(&bevy::diagnostic::SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE))
            .and_then(|d| d.value())
            .map_or("n/a".into(), |v| format!("{v:.2} GiB"));
        format!(
            "entities={} meshes={} images={} materials={} process_mem={mem}",
            self.all.iter().count(),
            self.meshes.len(),
            self.images.len(),
            self.materials.len()
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn switch_map(
    mut commands: Commands,
    sw: Option<ResMut<Switch>>,
    persistent: Res<Persistent>,
    roots: Query<Entity, (With<Transform>, Without<ChildOf>, Without<Keep>, Without<Camera>, Without<DirectionalLight>, Without<Window>, Without<Node>)>,
    counts: Counts,
    mut clock: ResMut<RunClock>,
    mut unloaded: MessageWriter<SiteUnloaded>,
) {
    let Some(mut sw) = sw else { return };
    match sw.0 {
        0 => {
            info!("map change: before unload: {}", counts.line());
            let mut n = 0;
            for e in roots.iter().filter(|e| !persistent.0.contains(e)) {
                commands.entity(e).despawn();
                n += 1;
            }
            commands.remove_resource::<Site>();
            commands.remove_resource::<Buggy>();
            commands.remove_resource::<TerrainRender>();
            commands.remove_resource::<Loading>();
            commands.remove_resource::<LoadFailed>();
            *clock = RunClock::default();
            unloaded.write(SiteUnloaded);
            info!("map change: despawned {n} root scene entities (with their children)");
        }
        1 => {}
        _ => {
            info!("map change: after unload: {}", counts.line());
            commands.remove_resource::<Switch>();
            commands.insert_resource(StartLoad);
            return;
        }
    }
    sw.0 += 1;
}

// ------------------------------------------------------------------------------------ building

const ROW_H: f32 = 50.0;

fn font(px: f32) -> TextFont {
    TextFont { font_size: bevy::text::FontSize::Px(px), ..default() }
}

fn label(s: impl Into<String>, px: f32, color: Color) -> impl Bundle {
    (Text::new(s), font(px), TextColor(color), Pickable::IGNORE)
}

fn para(s: impl Into<String>, px: f32, color: Color) -> impl Bundle {
    (Text::new(s), font(px), TextColor(color), Pickable::IGNORE, Node { max_width: Val::Percent(100.0), ..default() })
}

struct Builder {
    focus: usize,
}

impl Builder {
    fn button(&mut self, p: &mut ChildSpawnerCommands, text: &str, msg: Msg, kind: Kind, node: Node) -> Entity {
        let (pad, size) = match kind {
            Kind::Primary => (UiRect::axes(Val::Px(26.0), Val::Px(12.0)), 20.0),
            Kind::Tab => (UiRect::axes(Val::Px(14.0), Val::Px(8.0)), 16.0),
            Kind::Chip => (UiRect::axes(Val::Px(11.0), Val::Px(6.0)), 14.0),
            _ => (UiRect::axes(Val::Px(18.0), Val::Px(10.0)), 17.0),
        };
        // the caller's node wins for rows and cards (their own layout); others get centred labels
        let mut node = node;
        node.border = UiRect::all(Val::Px(1.0));
        node.border_radius = BorderRadius::all(Val::Px(if kind == Kind::Chip { 14.0 } else { 7.0 }));
        if !matches!(kind, Kind::Row | Kind::Card) {
            node.padding = pad;
            node.justify_content = JustifyContent::Center;
            node.align_items = AlignItems::Center;
        }
        let focusable = matches!(kind, Kind::Primary | Kind::Normal | Kind::Card | Kind::Chip | Kind::Tab);
        let mut e = p.spawn((
            Btn { msg, kind },
            Hovered::default(),
            Outline::new(Val::Px(2.0), Val::Px(2.0), Color::NONE),
            node,
            BackgroundColor(BTN),
            BorderColor::all(PANEL_EDGE),
        ));
        if focusable {
            e.insert(Focusable(self.focus));
            self.focus += 1;
        }
        e.observe(on_click);
        if !text.is_empty() {
            let c = if kind == Kind::Primary { DARK_TEXT } else { TEXT };
            e.with_children(|p| {
                let mut t = p.spawn(label(text, size, c));
                if kind == Kind::Primary {
                    t.insert(PlayLabel);
                }
            });
        }
        e.id()
    }
}

fn panel(node: Node) -> (Node, BackgroundColor, BorderColor) {
    (node, BackgroundColor(PANEL), BorderColor::all(PANEL_EDGE))
}

#[allow(clippy::too_many_arguments)]
fn rebuild(
    mut commands: Commands,
    mut menu: ResMut<Menu>,
    cat: Res<Catalog>,
    old: Query<Entity, With<MenuRoot>>,
    win: Query<&Window, With<PrimaryWindow>>,
) {
    let size = win.single().map(|w| Vec2::new(w.width(), w.height())).unwrap_or(Vec2::new(1280.0, 720.0));
    let narrow = size.x < 760.0;
    let reserve = if narrow { 540.0 } else { 390.0 };
    let rows = (((size.y - reserve) / ROW_H).floor() as usize).clamp(3, 12);
    if narrow != menu.narrow || rows != menu.rows {
        menu.narrow = narrow;
        menu.rows = rows;
        menu.dirty = true;
    }
    if !menu.dirty {
        return;
    }
    menu.dirty = false;
    menu.dirty_list = true;
    menu.dirty_dots = true;
    for e in &old {
        commands.entity(e).despawn();
    }
    if !menu.open {
        return;
    }
    let m = &*menu;
    let mut b = Builder { focus: 0 };
    let veil = if m.in_game { 0.80 } else { 0.94 };
    commands
        .spawn((
            MenuRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(if narrow { 10.0 } else { 18.0 })),
                row_gap: Val::Px(10.0),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.03, 0.02, veil)),
            GlobalZIndex(10),
        ))
        .with_children(|root| {
            let inner = Node {
                width: Val::Percent(100.0),
                max_width: Val::Px(1180.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                flex_grow: 1.0,
                min_height: Val::Px(0.0),
                ..default()
            };
            root.spawn(inner).with_children(|col| {
                // header
                col.spawn(Node { flex_wrap: FlexWrap::Wrap, align_items: AlignItems::Baseline, column_gap: Val::Px(14.0), ..default() })
                    .with_children(|h| {
                        h.spawn(label("MARSMAKER", if narrow { 28.0 } else { 34.0 }, ACCENT));
                        h.spawn(label("drive a dune buggy on real Mars terrain", 16.0, DIM));
                    });
                // tabs
                col.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(6.0), row_gap: Val::Px(6.0), ..default() })
                    .with_children(|t| {
                        for p in m.tabs() {
                            b.button(t, p.label(), Msg::Page(p), Kind::Tab, Node::default());
                        }
                    });
                // page
                col.spawn(Node { flex_grow: 1.0, min_height: Val::Px(0.0), flex_direction: FlexDirection::Column, ..default() })
                    .with_children(|pg| match m.page {
                        Page::Pause => page_pause(pg, &mut b, m),
                        Page::About => page_about(pg, &mut b),
                        Page::Map => page_map(pg, &mut b, m, &cat),
                        Page::Mode => page_mode(pg, &mut b),
                        Page::Controls => page_controls(pg),
                    });
                // bottom bar
                if m.page != Page::Pause {
                    col.spawn((
                        Node {
                            flex_wrap: FlexWrap::Wrap,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(10.0),
                            row_gap: Val::Px(8.0),
                            padding: UiRect::axes(Val::Px(14.0), Val::Px(10.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(10.0)),
                            ..default()
                        },
                        BackgroundColor(PANEL),
                        BorderColor::all(PANEL_EDGE),
                    ))
                    .with_children(|bar| {
                        bar.spawn((label("", 15.0, TEXT), Summary, Node { flex_grow: 1.0, flex_basis: Val::Px(240.0), ..default() }));
                        if m.in_game {
                            b.button(bar, "RESUME", Msg::Resume, Kind::Normal, Node::default());
                        }
                        b.button(bar, "PLAY", Msg::Play, Kind::Primary, Node::default());
                    });
                }
            });
        });
}

fn page_pause(p: &mut ChildSpawnerCommands, b: &mut Builder, m: &Menu) {
    p.spawn(Node { width: Val::Percent(100.0), justify_content: JustifyContent::Center, padding: UiRect::top(Val::Px(20.0)), ..default() })
        .with_children(|c| {
            c.spawn(panel(Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(22.0)),
                row_gap: Val::Px(10.0),
                width: Val::Px(340.0),
                max_width: Val::Percent(100.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(10.0)),
                ..default()
            }))
            .with_children(|c| {
                c.spawn(label("PAUSED", 24.0, ACCENT));
                let full = || Node { width: Val::Percent(100.0), ..default() };
                if m.in_game {
                    b.button(c, "Resume", Msg::Resume, Kind::Normal, full());
                }
                b.button(c, "Change map", Msg::Page(Page::Map), Kind::Normal, full());
                b.button(c, "Change mode", Msg::Page(Page::Mode), Kind::Normal, full());
                b.button(c, "Controls", Msg::Page(Page::Controls), Kind::Normal, full());
                b.button(c, "About / disclaimer", Msg::Page(Page::About), Kind::Normal, full());
                #[cfg(not(target_arch = "wasm32"))]
                b.button(c, "Quit", Msg::Quit, Kind::Normal, full());
                c.spawn(label("Esc resumes", 14.0, DIM));
            });
        });
}

fn page_about(p: &mut ChildSpawnerCommands, b: &mut Builder) {
    p.spawn(panel(Node {
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(Val::Px(22.0)),
        row_gap: Val::Px(12.0),
        max_width: Val::Px(860.0),
        border: UiRect::all(Val::Px(1.0)),
        border_radius: BorderRadius::all(Val::Px(10.0)),
        overflow: Overflow::clip(),
        ..default()
    }))
    .with_children(|c| {
        c.spawn(label("Before you drive", 22.0, ACCENT));
        let items = [
            "The ground is real: HiRISE stereo terrain models and orthoimages from NASA's Mars Reconnaissance Orbiter. Data: NASA/JPL/University of Arizona, public domain (CC0).",
            "Heights are accurate to about a few decimetres, but the resolution is about 1-4 m per post: rocks and ripples smaller than that are not there.",
            "FUN mode ignores real physics on purpose. REAL mode uses Mars gravity and loose-sand tyres, but it is still a game model.",
            "SAND mode is a simulation: the dunes move and the terrain changes, so it no longer matches the real data.",
            "This is a game, not a scientific tool - please don't use it for measurements or mission planning.",
            "Code: MIT licence, github.com/zombodotcom/marsmaker",
        ];
        for s in items {
            c.spawn(Node { column_gap: Val::Px(10.0), ..default() }).with_children(|r| {
                r.spawn(label("*", 16.0, ACCENT));
                r.spawn(para(s, 16.0, TEXT));
            });
        }
        c.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(10.0), row_gap: Val::Px(8.0), margin: UiRect::top(Val::Px(6.0)), ..default() })
            .with_children(|r| {
                b.button(r, "Got it - choose a map", Msg::Page(Page::Map), Kind::Normal, Node::default());
                b.button(r, "Source on GitHub", Msg::Link, Kind::Normal, Node::default());
            });
    });
}

fn page_mode(p: &mut ChildSpawnerCommands, b: &mut Builder) {
    p.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(12.0), row_gap: Val::Px(12.0), ..default() })
        .with_children(|r| {
            for (i, m) in MODES.iter().enumerate() {
                let card = b.button(
                    r,
                    "",
                    Msg::Mode(i),
                    Kind::Card,
                    Node {
                        width: Val::Px(300.0),
                        max_width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::FlexStart,
                        justify_content: JustifyContent::FlexStart,
                        row_gap: Val::Px(8.0),
                        padding: UiRect::all(Val::Px(16.0)),
                        ..default()
                    },
                );
                let avail = mode_available(i);
                r.commands().entity(card).with_children(|c| {
                    c.spawn(label(m.label, 26.0, if avail { ACCENT } else { DIM }));
                    c.spawn(para(m.blurb, 15.0, if avail { TEXT } else { DIM }));
                    if !avail {
                        c.spawn(label("coming soon", 14.0, DIM));
                    }
                });
            }
        });
    p.spawn(para("M switches FUN / REAL while driving. Pick a mode, then PLAY (or CONTINUE in game).", 14.0, DIM))
        .insert(Node { margin: UiRect::top(Val::Px(10.0)), ..default() });
    p.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(6.0), row_gap: Val::Px(6.0), align_items: AlignItems::Center, margin: UiRect::top(Val::Px(14.0)), ..default() })
        .with_children(|r| {
            r.spawn(label("Terrain smoothing (removes DTM noise; reloads the map):", 15.0, TEXT)).insert(Node { margin: UiRect::right(Val::Px(6.0)), ..default() });
            for (i, (_, name)) in SMOOTHS.iter().enumerate() {
                b.button(r, name, Msg::Smooth(i), Kind::Chip, Node::default());
            }
        });
    p.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(6.0), row_gap: Val::Px(6.0), align_items: AlignItems::Center, margin: UiRect::top(Val::Px(10.0)), ..default() })
        .with_children(|r| {
            r.spawn(label("Graphics (restarts the viewer):", 15.0, TEXT)).insert(Node { margin: UiRect::right(Val::Px(6.0)), ..default() });
            b.button(r, "HIGH", Msg::Graphics(false), Kind::Chip, Node::default());
            b.button(r, "LOW - faster on weak machines", Msg::Graphics(true), Kind::Chip, Node::default());
        });
}

fn page_controls(p: &mut ChildSpawnerCommands) {
    p.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(12.0), row_gap: Val::Px(12.0), ..default() })
        .with_children(|r| {
            let cols = [
                (
                    "Driving",
                    "W / S or Up / Down   throttle / reverse\nA / D or Left / Right   steer\nSpace   brake\nR   flip upright\nM   switch FUN / REAL\nC   chase / high camera\nMouse wheel   zoom\nEsc   pause menu",
                ),
                ("FUN mode", "Shift   boost\nCtrl or J   jump\nW / S / A / D in the air   pitch / roll"),
                (
                    "Menu",
                    "Tab / Shift+Tab   pages\nUp / Down   choose a site\nLeft / Right   filters\nType   search\nEnter   play\nTouch: tap rows and map dots,\ndrag the list, || button in game",
                ),
            ];
            for (t, body) in cols {
                r.spawn(panel(Node {
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(16.0)),
                    row_gap: Val::Px(8.0),
                    width: Val::Px(350.0),
                    max_width: Val::Percent(100.0),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(10.0)),
                    ..default()
                }))
                .with_children(|c| {
                    c.spawn(label(t, 19.0, ACCENT));
                    c.spawn(para(body, 15.0, TEXT));
                });
            }
        });
}

fn page_map(p: &mut ChildSpawnerCommands, b: &mut Builder, m: &Menu, cat: &Catalog) {
    // search + filter chips
    p.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(6.0), row_gap: Val::Px(6.0), align_items: AlignItems::Center, margin: UiRect::bottom(Val::Px(8.0)), ..default() })
        .with_children(|r| {
            r.spawn((
                Node {
                    min_width: Val::Px(220.0),
                    padding: UiRect::axes(Val::Px(12.0), Val::Px(7.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(7.0)),
                    margin: UiRect::right(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.08, 0.045, 0.03)),
                BorderColor::all(ACCENT.with_alpha(0.6)),
            ))
            .with_children(|s| {
                s.spawn((label("", 15.0, TEXT), SearchText));
            });
            if !m.query.is_empty() {
                b.button(r, "x", Msg::Search(String::new()), Kind::Chip, Node::default());
            }
            for (i, (name, _)) in FILTERS.iter().enumerate() {
                b.button(r, name, Msg::Filter(i), Kind::Chip, Node::default());
            }
            r.spawn(Node { width: Val::Px(8.0), ..default() });
            for (i, s) in SOURCES.iter().enumerate() {
                b.button(r, s, Msg::Source(i), Kind::Chip, Node::default());
            }
        });
    p.spawn(Node { flex_wrap: FlexWrap::Wrap, column_gap: Val::Px(14.0), row_gap: Val::Px(10.0), ..default() })
        .with_children(|r| {
            // list
            r.spawn(Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, flex_basis: Val::Px(360.0), row_gap: Val::Px(6.0), ..default() })
                .with_children(|c| {
                    c.spawn((label("", 14.0, DIM), ListStatus));
                    c.spawn((
                        ListBox,
                        Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), ..default() },
                        BackgroundColor(Color::NONE),
                    ))
                    .observe(on_list_scroll)
                    .observe(on_list_drag)
                    .with_children(|lb| {
                        lb.spawn(Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), flex_grow: 1.0, min_width: Val::Px(0.0), ..default() })
                            .with_children(|rows| {
                                for i in 0..m.rows {
                                    let row = b.button(
                                        rows,
                                        "",
                                        Msg::Row(i),
                                        Kind::Row,
                                        Node {
                                            height: Val::Px(ROW_H - 4.0),
                                            flex_direction: FlexDirection::Column,
                                            align_items: AlignItems::Stretch,
                                            justify_content: JustifyContent::Center,
                                            padding: UiRect::axes(Val::Px(10.0), Val::Px(3.0)),
                                            overflow: Overflow::clip(),
                                            ..default()
                                        },
                                    );
                                    rows.commands().entity(row).with_children(|rc| {
                                        rc.spawn(Node { column_gap: Val::Px(8.0), ..default() }).with_children(|l| {
                                            l.spawn((label("", 12.0, DARK_TEXT), RowTag(i), Node { padding: UiRect::axes(Val::Px(5.0), Val::Px(1.0)), border_radius: BorderRadius::all(Val::Px(4.0)), ..default() }, BackgroundColor(ACCENT)));
                                            l.spawn((label("", 15.0, TEXT), RowTitle(i), TextLayout::no_wrap()));
                                        });
                                        rc.spawn((label("", 13.0, DIM), RowSub(i), TextLayout::no_wrap()));
                                    });
                                }
                            });
                        // scroll bar
                        lb.spawn((Node { width: Val::Px(5.0), flex_direction: FlexDirection::Column, border_radius: BorderRadius::all(Val::Px(3.0)), ..default() }, BackgroundColor(BTN)))
                            .with_children(|sb| {
                                sb.spawn((ScrollBar, Node { width: Val::Percent(100.0), border_radius: BorderRadius::all(Val::Px(3.0)), ..default() }, BackgroundColor(ACCENT.with_alpha(0.7)), Pickable::IGNORE));
                            });
                    });
                });
            // map + details
            let map_display = if m.narrow { Display::None } else { Display::Flex };
            r.spawn(Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, flex_basis: Val::Px(320.0), row_gap: Val::Px(8.0), ..default() })
                .with_children(|c| {
                    c.spawn((
                        MapBox,
                        Node {
                            display: map_display,
                            width: Val::Percent(100.0),
                            aspect_ratio: Some(2.0),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(8.0)),
                            overflow: Overflow::clip(),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.20, 0.10, 0.06)),
                        BorderColor::all(PANEL_EDGE),
                    ))
                    .observe(on_map_click)
                    .with_children(|mp| {
                        // graticule every 30 degrees, equator and prime meridian brighter
                        for k in 1..6 {
                            let strong = k == 3;
                            mp.spawn((
                                Node { position_type: PositionType::Absolute, left: Val::Px(0.0), right: Val::Px(0.0), top: Val::Percent(k as f32 * 100.0 / 6.0), height: Val::Px(1.0), ..default() },
                                BackgroundColor(Color::srgba(1.0, 0.8, 0.6, if strong { 0.22 } else { 0.08 })),
                                Pickable::IGNORE,
                            ));
                        }
                        for k in 1..12 {
                            let strong = k == 6;
                            mp.spawn((
                                Node { position_type: PositionType::Absolute, top: Val::Px(0.0), bottom: Val::Px(0.0), left: Val::Percent(k as f32 * 100.0 / 12.0), width: Val::Px(1.0), ..default() },
                                BackgroundColor(Color::srgba(1.0, 0.8, 0.6, if strong { 0.22 } else { 0.08 })),
                                Pickable::IGNORE,
                            ));
                        }
                        for (i, e) in cat.entries.iter().enumerate() {
                            let (s, col) = if e.is_live() { (4.0, LIVE_DOT) } else { (6.0, LOCAL_DOT) };
                            mp.spawn((
                                Dot(i),
                                Node {
                                    position_type: PositionType::Absolute,
                                    left: Val::Percent(((e.lon + 180.0) / 3.6) as f32),
                                    top: Val::Percent(((90.0 - e.lat) / 1.8) as f32),
                                    width: Val::Px(s),
                                    height: Val::Px(s),
                                    margin: UiRect::all(Val::Px(-s / 2.0)),
                                    border_radius: BorderRadius::MAX,
                                    ..default()
                                },
                                BackgroundColor(col),
                                Pickable::IGNORE,
                            ));
                        }
                        mp.spawn((
                            SelMarker,
                            Node {
                                position_type: PositionType::Absolute,
                                width: Val::Px(16.0),
                                height: Val::Px(16.0),
                                margin: UiRect::all(Val::Px(-8.0)),
                                border: UiRect::all(Val::Px(2.0)),
                                border_radius: BorderRadius::MAX,
                                ..default()
                            },
                            BorderColor::all(Color::WHITE),
                            Visibility::Hidden,
                            Pickable::IGNORE,
                        ));
                        mp.spawn((label("N", 11.0, DIM), Node { position_type: PositionType::Absolute, left: Val::Px(6.0), top: Val::Px(3.0), ..default() }));
                        mp.spawn((
                            label("orange: live from NASA   cyan: on disk", 11.0, DIM),
                            Node { position_type: PositionType::Absolute, left: Val::Px(6.0), bottom: Val::Px(3.0), ..default() },
                        ));
                    });
                    c.spawn(panel(Node {
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(Val::Px(12.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    }))
                    .with_children(|d| {
                        d.spawn((para("", 14.0, TEXT), Details));
                    });
                });
        });
}

// ------------------------------------------------------------------------------------ refresh

fn fmt_lat_lon(e: &Entry) -> String {
    format!(
        "{:.2}{} {:.2}{}",
        e.lat.abs(),
        if e.lat >= 0.0 { "N" } else { "S" },
        e.lon.abs(),
        if e.lon >= 0.0 { "E" } else { "W" }
    )
}

fn describe(e: &Entry) -> String {
    let mut s = fmt_lat_lon(e);
    if let Some((w, h)) = e.size_km {
        s += &format!("   {w:.1} x {h:.1} km");
    }
    if let Some(r) = e.res_m {
        s += &format!("   {r:.1} m/post");
    }
    s
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}...", s.chars().take(n.saturating_sub(3)).collect::<String>()) }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn refresh_list(
    mut menu: ResMut<Menu>,
    cat: Res<Catalog>,
    mut texts: ParamSet<(
        Query<(&RowTitle, &mut Text)>,
        Query<(&RowSub, &mut Text)>,
        Query<(&RowTag, &mut Text, &mut BackgroundColor)>,
        Query<&mut Text, With<Details>>,
        Query<&mut Text, With<SearchText>>,
        Query<&mut Text, With<ListStatus>>,
        Query<&mut Text, With<Summary>>,
        Query<&mut Text, With<PlayLabel>>,
    )>,
    mut rows: Query<(&Btn, &mut Visibility)>,
    mut bar: Query<&mut Node, With<ScrollBar>>,
    time: Res<Time<Real>>,
) {
    // blinking caret
    let caret = if (time.elapsed_secs() * 2.0) as u32 % 2 == 0 { "_" } else { " " };
    if let Ok(mut t) = texts.p4().single_mut() {
        let s = if menu.query.is_empty() { format!("type to search: name, crater, dunes...{caret}") } else { format!("{}{caret}", menu.query) };
        if t.0 != s {
            t.0 = s;
        }
    }
    if !menu.dirty_list {
        return;
    }
    menu.dirty_list = false;
    let m = &*menu;
    let width_chars = if m.narrow { 34 } else { 56 };
    let row_entry = |i: usize| m.filtered.get(m.scroll + i).and_then(|&k| cat.entries.get(k));
    for (r, mut t) in texts.p0().iter_mut() {
        t.0 = row_entry(r.0).map(|e| clip(&e.title, width_chars)).unwrap_or_default();
    }
    for (r, mut t) in texts.p1().iter_mut() {
        t.0 = row_entry(r.0).map(|e| clip(&format!("{}   {}", fmt_lat_lon(e), e.id), width_chars + 8)).unwrap_or_default();
    }
    for (r, mut t, mut bg) in texts.p2().iter_mut() {
        let e = row_entry(r.0);
        t.0 = e.map(|e| if e.is_live() { "LIVE" } else { "DISK" }).unwrap_or_default().into();
        bg.0 = if e.is_some_and(|e| !e.is_live()) { LOCAL_DOT } else { ACCENT };
    }
    for (b, mut v) in &mut rows {
        if let Msg::Row(i) = b.msg {
            *v = if row_entry(i).is_some() { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
    let sel = m.sel_site.and_then(|i| cat.entries.get(i));
    if let Ok(mut t) = texts.p3().single_mut() {
        t.0 = match sel {
            Some(e) => format!(
                "{}\n{}   {}\n{}",
                e.title,
                e.id,
                describe(e),
                if e.is_live() {
                    format!("Streams from NASA's public HiRISE bucket{}", e.mb.map_or(String::new(), |mb| format!(" (about {:.0} MB of DTM tiles at most)", mb.min(200.0))))
                } else {
                    "Converted site on disk: full resolution".into()
                }
            ),
            None => "No site selected".into(),
        };
    }
    if let Ok(mut t) = texts.p5().single_mut() {
        t.0 = if !cat.ready {
            "Loading the site list...".into()
        } else {
            let local = cat.entries.iter().filter(|e| !e.is_live()).count();
            let mut s = format!("{} shown of {} ({local} on disk, {} live from NASA)", m.filtered.len(), cat.entries.len(), cat.entries.len() - local);
            if let Some(n) = cat.notes.first() {
                s += &format!("  - {}", clip(n, 60));
            }
            s
        };
    }
    if let Ok(mut t) = texts.p6().single_mut() {
        t.0 = format!(
            "{}   |   mode: {}",
            sel.map_or("no map selected".into(), |e| clip(&e.title, if m.narrow { 30 } else { 60 })),
            MODES[m.sel_mode].label
        );
    }
    let play = if !m.in_game {
        "PLAY"
    } else if m.reload_needed(&cat) {
        "LOAD MAP"
    } else {
        "CONTINUE"
    };
    for mut t in texts.p7().iter_mut() {
        t.0 = play.into();
    }
    if let Ok(mut n) = bar.single_mut() {
        let total = m.filtered.len().max(1) as f32;
        let frac = (m.rows as f32 / total).min(1.0);
        n.height = Val::Percent(frac * 100.0);
        n.margin.top = Val::Percent(m.scroll as f32 / total * 100.0);
    }
}

fn refresh_dots(
    mut menu: ResMut<Menu>,
    cat: Res<Catalog>,
    mut dots: Query<(&Dot, &mut Visibility), Without<SelMarker>>,
    mut marker: Query<(&mut Node, &mut Visibility), With<SelMarker>>,
) {
    if !menu.dirty_dots {
        return;
    }
    menu.dirty_dots = false;
    let shown: HashSet<usize> = menu.filtered.iter().copied().collect();
    for (d, mut v) in &mut dots {
        let want = if shown.contains(&d.0) { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
    if let Ok((mut n, mut v)) = marker.single_mut() {
        match menu.sel_site.and_then(|i| cat.entries.get(i)) {
            Some(e) => {
                n.left = Val::Percent(((e.lon + 180.0) / 3.6) as f32);
                n.top = Val::Percent(((90.0 - e.lat) / 1.8) as f32);
                *v = Visibility::Inherited;
            }
            None => *v = Visibility::Hidden,
        }
    }
}

#[allow(clippy::type_complexity)]
fn style_buttons(
    menu: Res<Menu>,
    cat: Res<Catalog>,
    mut q: Query<(&Btn, &Hovered, Option<&Focusable>, &mut BackgroundColor, &mut BorderColor, &mut Outline), Without<PauseButton>>,
) {
    let m = &*menu;
    for (b, hov, f, mut bg, mut border, mut outline) in &mut q {
        let selected = match &b.msg {
            Msg::Page(p) => b.kind == Kind::Tab && m.page == *p,
            Msg::Filter(i) => m.filter == *i,
            Msg::Source(i) => m.source == *i,
            Msg::Mode(i) => m.sel_mode == *i,
            Msg::Smooth(i) => m.sel_smooth == *i,
            Msg::Graphics(low) => m.low == *low,
            Msg::Row(i) => m.filtered.get(m.scroll + i).is_some() && m.filtered.get(m.scroll + i) == m.sel_site.as_ref(),
            _ => false,
        };
        let disabled = match &b.msg {
            Msg::Mode(i) => !mode_available(*i),
            Msg::Play => m.sel_site.is_none() || !cat.ready,
            _ => false,
        };
        let h = hov.get() && !disabled;
        let c = match b.kind {
            Kind::Primary if disabled => BTN,
            Kind::Primary if h => ACCENT_HI,
            Kind::Primary => ACCENT,
            _ if selected => BTN_SEL,
            _ if h => BTN_HOVER,
            _ if disabled => Color::srgb(0.16, 0.10, 0.08),
            _ => BTN,
        };
        if bg.0 != c {
            bg.0 = c;
        }
        let bc = if selected { ACCENT } else { PANEL_EDGE };
        if border.top != bc {
            *border = BorderColor::all(bc);
        }
        let oc = if f.is_some_and(|f| f.0 == m.focus) && m.page != Page::Map { ACCENT_HI } else { Color::NONE };
        if outline.color != oc {
            outline.color = oc;
        }
    }
}

fn pause_button_vis(menu: Res<Menu>, mut q: Query<(&mut Visibility, &Hovered, &mut BackgroundColor), With<PauseButton>>) {
    let Ok((mut v, h, mut bg)) = q.single_mut() else { return };
    let want = if menu.in_game && !menu.open { Visibility::Inherited } else { Visibility::Hidden };
    if *v != want {
        *v = want;
    }
    let c = if h.get() { BTN_HOVER } else { BTN.with_alpha(0.8) };
    if bg.0 != c {
        bg.0 = c;
    }
}

// ------------------------------------------------------------------------------------ test script

/// `--menu-script` runner: comma-separated steps `CMD[:ARG][@DELAY]`, run one after another;
/// DELAY (s, default 0.3) is the wait after the step. Steps:
/// `page:map|mode|about|controls|pause`, `search:TEXT`, `filter:NAME`, `source:all|disk|live`,
/// `site:ID` (on-disk entry preferred) / `site:live:ID`, `mode:fun|real|sand`, `play`, `esc`,
/// `resume`, `loaded` (wait until the site is loaded and driving), `drive:on|off` (autodrive),
/// `shot:FILE.png` (waits for the file), `stats` (entity / asset / memory counts), `wait:S`, `quit`.
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
            .map(|item| {
                let (body, delay) = match item.rsplit_once('@') {
                    Some((b, d)) if d.parse::<f32>().is_ok() => (b, d.parse().unwrap()),
                    _ => (item, 0.3),
                };
                let (cmd, arg) = body.split_once(':').unwrap_or((body, ""));
                (cmd.to_string(), arg.to_string(), delay)
            })
            .collect();
        ScriptRun { steps, i: 0, next_t: 1.0, shot: None }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_script(
    mut commands: Commands,
    run: Option<ResMut<ScriptRun>>,
    time: Res<Time<Real>>,
    cat: Res<Catalog>,
    menu: Res<Menu>,
    site: Option<Res<Site>>,
    loading: Option<Res<Loading>>,
    failed: Option<Res<LoadFailed>>,
    switch: Option<Res<Switch>>,
    clock: Res<RunClock>,
    mut args: ResMut<Args>,
    counts: Counts,
    mut out: MessageWriter<Msg>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(mut run) = run else { return };
    let now = time.elapsed_secs();
    if now > 900.0 {
        error!("menu-script: timed out at step {}", run.i);
        exit.write(AppExit::error());
        return;
    }
    if let Some(flag) = &run.shot {
        if !flag.load(Ordering::Relaxed) {
            return;
        }
        run.shot = None;
    }
    if now < run.next_t || run.i >= run.steps.len() {
        return;
    }
    let (cmd, arg, delay) = run.steps[run.i].clone();
    let mut done = true;
    match cmd.as_str() {
        "page" => {
            out.write(Msg::Page(Page::parse(&arg).unwrap_or_else(|| panic!("menu-script: page {arg}"))));
        }
        "search" => {
            out.write(Msg::Search(arg.clone()));
        }
        "filter" => {
            out.write(Msg::Filter(FILTERS.iter().position(|f| f.0 == arg).unwrap_or_else(|| panic!("menu-script: filter {arg}"))));
        }
        "source" => {
            out.write(Msg::Source(match arg.as_str() {
                "disk" => 1,
                "live" => 2,
                _ => 0,
            }));
        }
        "site" => {
            if !cat.ready {
                done = false;
            } else {
                let (live, id) = match arg.strip_prefix("live:") {
                    Some(id) => (true, id),
                    None => (false, arg.as_str()),
                };
                let i = cat
                    .entries
                    .iter()
                    .position(|e| e.id == id && (live == e.is_live()))
                    .or_else(|| if live { None } else { cat.entries.iter().position(|e| e.id == id) })
                    .unwrap_or_else(|| panic!("menu-script: site {arg} is not in the catalogue"));
                out.write(Msg::Site(i));
            }
        }
        "mode" => {
            out.write(Msg::Mode(MODES.iter().position(|m| m.key == arg).unwrap_or_else(|| panic!("menu-script: mode {arg}"))));
        }
        "smooth" => {
            out.write(Msg::Smooth(SMOOTHS.iter().position(|s| s.1.starts_with(arg.as_str())).unwrap_or_else(|| panic!("menu-script: smooth {arg}"))));
        }
        "play" => {
            out.write(Msg::Play);
        }
        "esc" => {
            out.write(Msg::Esc);
        }
        "resume" => {
            out.write(Msg::Resume);
        }
        "loaded" => {
            if failed.is_some() {
                error!("menu-script: load failed");
                exit.write(AppExit::error());
            }
            done = switch.is_none() && loading.is_none() && site.is_some() && clock.start.is_some() && !menu.open;
            if done {
                info!("menu-script: loaded: {}", counts.line());
            }
        }
        "drive" => args.autodrive = arg != "off",
        "stats" => info!("menu-script: stats: {}", counts.line()),
        "wait" => run.next_t = now + arg.parse::<f32>().unwrap_or(1.0),
        "shot" => {
            let flag = Arc::new(AtomicBool::new(false));
            let f2 = flag.clone();
            let p = arg.clone();
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(arg.clone())).observe(move |_: On<ScreenshotCaptured>| {
                info!("menu-script: screenshot saved to {p}");
                f2.store(true, Ordering::Relaxed);
            });
            run.shot = Some(flag);
        }
        "quit" => {
            info!("menu-script: done: {}", counts.line());
            exit.write(AppExit::Success);
        }
        other => panic!("menu-script: unknown step {other}"),
    }
    if done {
        info!("menu-script: step {} {cmd}:{arg}", run.i);
        run.i += 1;
        if cmd != "wait" {
            run.next_t = now + delay;
        }
    }
}
