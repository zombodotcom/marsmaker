//! MarsScape HUD (bevy_ui): chat box, side panel (inventory / skills / gear / quest tabs), minimap
//! with compass, hitpoints and run orbs, fabricator / locker / trade / lamp panels, colonist dialog,
//! the Rift Surge banner, right-click menu, name prompt, and the world-anchored overlays (name tags,
//! health bars, hitsplats) plus XP drops.
//! Chunky dark-brown panels with rust borders: old-school in spirit, all original.

use super::proto::*;
use super::{Orbit, Scape, fmt_xp};
use crate::menu::Menu;
use bevy::asset::RenderAssetUsages;
use bevy::picking::hover::Hovered;
use bevy::picking::prelude::*;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::collections::{HashMap, HashSet};

const PANEL: Color = Color::srgba(0.14, 0.09, 0.06, 0.93);
const EDGE: Color = Color::srgb(0.55, 0.30, 0.15);
const EDGE_HI: Color = Color::srgb(0.85, 0.55, 0.25);
const INSET: Color = Color::srgb(0.09, 0.06, 0.04);
const TEXT: Color = Color::srgb(1.0, 0.93, 0.80);
const DIM: Color = Color::srgb(0.70, 0.60, 0.48);
const GOLD: Color = Color::srgb(1.0, 0.80, 0.30);
const GOOD: Color = Color::srgb(0.45, 0.95, 0.40);
const BAD: Color = Color::srgb(1.0, 0.40, 0.30);
pub const TIP: Color = Color::srgb(0.75, 0.90, 0.70);
/// name tags of colonist bots
const COLONIST: Color = Color::srgb(0.45, 1.0, 0.80);

fn shadow() -> TextShadow {
    TextShadow { offset: Vec2::splat(1.5), color: Color::srgba(0.0, 0.0, 0.0, 0.85) }
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiAct {
    Walk(IVec2),
    Node(u32),
    Attack(u32),
    Say(String),
    Cancel,
    Tab(u8),
    Slot(usize),
    Item(usize, &'static str),
    Make(&'static str),
    CloseFab,
    Run,
    Compass,
    Talk(u32),
    TradeWith(u32),
    /// a dialog option id
    Choose(String),
    Deposit(usize, Qty),
    Withdraw(usize, Qty),
    CloseBank,
    Offer(usize, Qty),
    /// take back an offered item (index into your offer)
    Unoffer(usize),
    TradeAccept,
    TradeDecline,
    /// answer someone's trade request
    TradeReq(bool),
    Rub(Skill),
    CloseLamp,
    UnequipGear(&'static str),
}

#[derive(Message, Clone, Debug)]
pub struct UiMsg {
    pub act: UiAct,
    /// right-click / shift
    pub alt: bool,
}

#[derive(Clone, Debug)]
pub struct Opt {
    pub label: String,
    pub act: UiAct,
    /// the world thing this option is about (hover ring)
    pub target: Option<super::pick::Target>,
}

impl Opt {
    pub fn new(label: String, act: UiAct) -> Self {
        Opt { label, act, target: None }
    }
    pub fn on(mut self, t: super::pick::Target) -> Self {
        self.target = Some(t);
        self
    }
}

/// Script request for a right-click menu: at a screen point, or on the nearest node / creature of a kind.
pub enum CtxReq {
    At(Vec2),
    Kind(String),
}

pub struct CtxMenu {
    pub at: Vec2,
    pub opts: Vec<Opt>,
}

#[derive(Resource, Default)]
pub struct UiState {
    pub tab: u8,
    pub fab_open: bool,
    pub ctx: Option<CtxMenu>,
    pub ctx_request: Option<CtxReq>,
    pub typing: Option<String>,
    pub name_entry: Option<String>,
    pub hover: String,
    /// what a left click would act on right now (hover ring)
    pub hover_target: Option<super::pick::Target>,
    /// test runs: a pretend mouse position (the off-screen test window has no real cursor)
    pub fake_cursor: Option<Vec2>,
    pub hello_sent: bool,
    pub cursor: Vec2,
    pub over_ui: bool,
}

#[derive(Component)]
pub struct ScapeUi;
#[derive(Component)]
pub struct NamePrompt;
#[derive(Component)]
pub struct NameText;
#[derive(Component)]
pub(super) struct ChatLine(usize);
#[derive(Component)]
pub(super) struct ChatInput;
#[derive(Component)]
pub(super) struct SideContent;
#[derive(Component)]
pub(super) struct TabBtn(u8);
#[derive(Component)]
pub(super) struct MiniMap;
#[derive(Component)]
pub(super) struct Needle;
#[derive(Component)]
pub(super) struct HpText;
#[derive(Component)]
pub(super) struct RunText;
#[derive(Component)]
pub(super) struct HoverText;
#[derive(Component)]
pub(super) struct StatusText;
#[derive(Component)]
pub(super) struct CtxRoot;
#[derive(Component)]
pub(super) struct FabRoot;
#[derive(Component)]
pub(super) struct Overlay;
/// HUD panels that world overlays (name tags, bars, splats) must not draw over
#[derive(Component)]
pub(super) struct Blocker;
#[derive(Component)]
pub(super) struct Btn(UiAct);
#[derive(Component)]
pub(super) struct DialogRoot;
#[derive(Component)]
pub(super) struct SurgeRoot;
#[derive(Component)]
pub(super) struct SurgeText;
#[derive(Component)]
pub(super) struct SurgeBar;

#[derive(Resource)]
pub struct UiHandles {
    minimap: Handle<Image>,
}

const CHAT_LINES: usize = 8;
const CHAT_W: f32 = 480.0;

/// Chat history as whole display lines: word-wrapped to the box width (the HUD font is
/// monospace), newest last.
fn chat_lines(chat: &std::collections::VecDeque<(String, Color)>, box_w: f32) -> Vec<(String, Color)> {
    let cols = (((box_w - 18.0) / 8.45).floor() as usize).max(20);
    let mut out = Vec::new();
    for (msg, col) in chat.iter().rev() {
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        for word in msg.split(' ') {
            let mut word = word.to_string();
            while word.chars().count() > cols {
                let head: String = word.chars().take(cols).collect();
                word = word.chars().skip(cols).collect();
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                lines.push(head);
            }
            let len = cur.chars().count() + usize::from(!cur.is_empty()) + word.chars().count();
            if len > cols && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(&word);
        }
        if !cur.is_empty() || lines.is_empty() {
            lines.push(cur);
        }
        for l in lines.into_iter().rev() {
            out.push((l, *col));
            if out.len() >= CHAT_LINES {
                out.reverse();
                return out;
            }
        }
    }
    out.reverse();
    out
}
const MAP_PX: u32 = 128;

fn font(px: f32) -> TextFont {
    TextFont { font_size: bevy::text::FontSize::Px(px), ..default() }
}

fn text(s: impl Into<String>, px: f32, c: Color) -> impl Bundle {
    (Text::new(s), font(px), TextColor(c), Pickable::IGNORE)
}

fn panel(node: Node) -> impl Bundle {
    let mut node = node;
    node.border = UiRect::all(Val::Px(3.0));
    node.border_radius = BorderRadius::all(Val::Px(4.0));
    (node, BackgroundColor(PANEL), BorderColor::all(EDGE))
}

fn on_btn(mut ev: On<PointerClick>, q: Query<&Btn>, mut out: MessageWriter<UiMsg>) {
    let Ok(b) = q.get(ev.entity) else { return };
    ev.propagate(false);
    let alt = match ev.button {
        PointerButton::Primary => false,
        PointerButton::Secondary => true,
        _ => return,
    };
    out.write(UiMsg { act: b.0.clone(), alt });
}

fn button(p: &mut ChildSpawnerCommands, act: UiAct, node: Node, bg: Color) -> Entity {
    let mut node = node;
    node.border = UiRect::all(Val::Px(1.0));
    if node.border_radius == BorderRadius::default() {
        node.border_radius = BorderRadius::all(Val::Px(3.0));
    }
    p.spawn((Btn(act), Hovered::default(), node, BackgroundColor(bg), BorderColor::all(EDGE))).observe(on_btn).id()
}

fn label_button(p: &mut ChildSpawnerCommands, label: &str, act: UiAct, px: f32) -> Entity {
    let e = button(p, act, Node { padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, INSET);
    p.commands().entity(e).with_children(|c| {
        c.spawn(text(label, px, TEXT));
    });
    e
}

pub fn spawn_name_entry(commands: &mut Commands) {
    commands
        .spawn((NamePrompt, ScapeUi, Node { width: Val::Percent(100.0), height: Val::Percent(100.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)), GlobalZIndex(8)))
        .with_children(|r| {
            r.spawn(panel(Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(12.0), padding: UiRect::all(Val::Px(22.0)), width: Val::Px(420.0), max_width: Val::Percent(94.0), ..default() })).with_children(|p| {
                p.spawn(text("MARSSCAPE", 30.0, GOLD));
                p.spawn(text("Choose your astronaut's name", 18.0, TEXT));
                p.spawn((Node { padding: UiRect::all(Val::Px(10.0)), border: UiRect::all(Val::Px(2.0)), ..default() }, BackgroundColor(INSET), BorderColor::all(EDGE))).with_children(|f| {
                    f.spawn((text("_", 22.0, GOLD), NameText));
                });
                p.spawn(text("Letters, digits, space, _ and - (up to 12). Press Enter.", 14.0, DIM));
            });
        });
}

pub fn spawn(commands: &mut Commands, images: &mut Assets<Image>) {
    let img = Image::new_fill(Extent3d { width: MAP_PX, height: MAP_PX, depth_or_array_layers: 1 }, TextureDimension::D2, &[0, 0, 0, 0], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
    let minimap = images.add(img);
    commands.insert_resource(UiHandles { minimap: minimap.clone() });
    // chat box, bottom left
    commands
        .spawn((ScapeUi, Blocker, panel(Node { position_type: PositionType::Absolute, left: Val::Px(8.0), bottom: Val::Px(8.0), width: Val::Px(CHAT_W), max_width: Val::Percent(60.0), flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(6.0)), ..default() }), GlobalZIndex(2)))
        .with_children(|p| {
            // a fixed number of whole lines (pre-wrapped in `chat_lines`), newest at the bottom
            p.spawn(Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(1.0), overflow: Overflow::clip(), ..default() }).with_children(|l| {
                for i in 0..CHAT_LINES {
                    l.spawn((text(" ", 14.0, TEXT), TextLayout::no_wrap(), ChatLine(i)));
                }
            });
            p.spawn((Node { height: Val::Px(2.0), margin: UiRect::vertical(Val::Px(3.0)), ..default() }, BackgroundColor(EDGE)));
            p.spawn((text("", 14.0, DIM), ChatInput));
        });
    // side panel, bottom right
    commands
        .spawn((ScapeUi, Blocker, panel(Node { position_type: PositionType::Absolute, right: Val::Px(8.0), bottom: Val::Px(8.0), width: Val::Px(238.0), flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(6.0)), row_gap: Val::Px(6.0), ..default() }), GlobalZIndex(2)))
        .with_children(|p| {
            p.spawn(Node { column_gap: Val::Px(4.0), ..default() }).with_children(|t| {
                for (i, l) in ["Pack", "Skills", "Gear", "Quests"].iter().enumerate() {
                    let e = label_button(t, l, UiAct::Tab(i as u8), 13.0);
                    t.commands().entity(e).insert(TabBtn(i as u8));
                }
            });
            p.spawn((SideContent, Node { flex_direction: FlexDirection::Column, min_height: Val::Px(310.0), ..default() }));
        });
    // minimap, compass, orbs, top right
    commands
        .spawn((ScapeUi, Blocker, Node { position_type: PositionType::Absolute, right: Val::Px(8.0), top: Val::Px(92.0), flex_direction: FlexDirection::Column, align_items: AlignItems::FlexEnd, row_gap: Val::Px(6.0), ..default() }, Pickable::IGNORE, GlobalZIndex(2)))
        .with_children(|p| {
            p.spawn((Node { column_gap: Val::Px(6.0), align_items: AlignItems::FlexStart, ..default() }, Pickable::IGNORE)).with_children(|r| {
                r.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(6.0), ..default() }, Pickable::IGNORE)).with_children(|c| {
                    let e = button(c, UiAct::Compass, Node { width: Val::Px(40.0), height: Val::Px(40.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, border_radius: BorderRadius::MAX, ..default() }, INSET);
                    c.commands().entity(e).insert(BorderColor::all(EDGE_HI)).with_children(|b| {
                        b.spawn((text("N", 16.0, BAD), Needle, UiTransform::default()));
                    });
                    c.spawn((Node { width: Val::Px(40.0), height: Val::Px(40.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, border: UiRect::all(Val::Px(2.0)), border_radius: BorderRadius::MAX, ..default() }, BackgroundColor(Color::srgb(0.35, 0.08, 0.06)), BorderColor::all(EDGE_HI)))
                        .with_children(|b| {
                            b.spawn((text("10", 15.0, TEXT), HpText));
                        });
                    let e = button(c, UiAct::Run, Node { width: Val::Px(40.0), height: Val::Px(40.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, border_radius: BorderRadius::MAX, ..default() }, INSET);
                    c.commands().entity(e).insert(BorderColor::all(EDGE_HI)).with_children(|b| {
                        b.spawn((text("walk", 12.0, TEXT), RunText));
                    });
                });
                r.spawn((MiniMap, ImageNode::new(minimap), Node { width: Val::Px(150.0), height: Val::Px(150.0), border: UiRect::all(Val::Px(3.0)), border_radius: BorderRadius::MAX, ..default() }, BorderColor::all(EDGE), BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.35))));
            });
        });
    // hover text + status, top left
    commands
        .spawn((ScapeUi, Blocker, Node { position_type: PositionType::Absolute, left: Val::Px(10.0), top: Val::Px(8.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }, Pickable::IGNORE, GlobalZIndex(2)))
        .with_children(|p| {
            p.spawn((text("", 17.0, TEXT), HoverText, shadow()));
            p.spawn((text("", 13.0, DIM), StatusText, shadow()));
        });
    // dynamic layers
    commands.spawn((ScapeUi, Overlay, Node { width: Val::Percent(100.0), height: Val::Percent(100.0), position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, GlobalZIndex(1)));
    commands.spawn((ScapeUi, FabRoot, Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }, Pickable::IGNORE, GlobalZIndex(3)));
    commands.spawn((ScapeUi, CtxRoot, Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, Pickable::IGNORE, GlobalZIndex(4)));
    // colonist dialog / trade request, above the chat box
    commands.spawn((ScapeUi, DialogRoot, Node { position_type: PositionType::Absolute, left: Val::Px(8.0), bottom: Val::Px(212.0), width: Val::Px(CHAT_W), max_width: Val::Percent(60.0), ..default() }, Pickable::IGNORE, GlobalZIndex(3)));
    // Rift Surge banner, top centre
    commands.spawn((ScapeUi, SurgeRoot, Node { position_type: PositionType::Absolute, top: Val::Px(40.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() }, Pickable::IGNORE, GlobalZIndex(2)));
}

fn short(item: &str) -> &str {
    match item {
        "hematite" => "Hematite",
        "ice" => "Ice",
        "water" => "Water",
        "sample" => "Sample",
        "iron_plate" => "Plate",
        "drill_bit" => "Drill",
        "plasma_gun" => "Plasma",
        "void_crystal" => "Crystal",
        "chitin" => "Chitin",
        "rifle" => "Pulse",
        "ancient_relic" => "Relic",
        "xp_lamp" => "Lamp",
        "chitin_helmet" => "Ch. helm",
        "chitin_suit" => "Ch. suit",
        "iron_helmet" => "Ir. helm",
        "iron_suit" => "Ir. suit",
        "void_shield" => "Shield",
        "relic_helmet" => "R. helm",
        "relic_suit" => "R. suit",
        "rift_blade" => "Blade",
        "void_cape" => "Cape",
        other => item_name(other),
    }
}

pub fn item_options(slot: usize, item: &str) -> Vec<Opt> {
    let name = item_name(item);
    let mut v = Vec::new();
    if !weapon(Some(item)).item.is_empty() {
        v.push(Opt::new(format!("Equip {name}"), UiAct::Item(slot, "equip")));
    }
    if armour(item).is_some() {
        v.push(Opt::new(format!("Wear {name}"), UiAct::Item(slot, "wear")));
    }
    if item == "xp_lamp" {
        v.push(Opt::new(format!("Rub {name}"), UiAct::Item(slot, "rub")));
    }
    if item == "medkit" {
        v.push(Opt::new(format!("Eat {name}"), UiAct::Item(slot, "eat")));
    }
    v.push(Opt::new(format!("Drop {name}"), UiAct::Item(slot, "drop")));
    v.push(Opt::new(format!("Examine {name}"), UiAct::Item(slot, "examine")));
    v.push(Opt::new("Cancel".into(), UiAct::Cancel));
    v
}

/// With the locker / a trade open: put the item in ("Deposit" / "Offer").
pub fn move_options(slot: usize, item: &str, verb: &'static str) -> Vec<Opt> {
    let name = item_name(item);
    let act = |q: Qty| if verb == "Offer" { UiAct::Offer(slot, q) } else { UiAct::Deposit(slot, q) };
    let mut v = vec![Opt::new(format!("{verb}-1 {name}"), act(Qty::N(1))), Opt::new(format!("{verb}-5 {name}"), act(Qty::N(5))), Opt::new(format!("{verb}-All {name}"), act(Qty::All))];
    v.push(Opt::new(format!("Examine {name}"), UiAct::Item(slot, "examine")));
    v.push(Opt::new("Cancel".into(), UiAct::Cancel));
    v
}

pub fn withdraw_options(idx: usize, item: &str) -> Vec<Opt> {
    let name = item_name(item);
    vec![
        Opt::new(format!("Withdraw-1 {name}"), UiAct::Withdraw(idx, Qty::N(1))),
        Opt::new(format!("Withdraw-5 {name}"), UiAct::Withdraw(idx, Qty::N(5))),
        Opt::new(format!("Withdraw-All {name}"), UiAct::Withdraw(idx, Qty::All)),
        Opt::new("Cancel".into(), UiAct::Cancel),
    ]
}

fn rgb(c: [f32; 3]) -> Color {
    Color::srgb(c[0], c[1], c[2])
}

fn side_pack(p: &mut ChildSpawnerCommands, sc: &Scape) {
    p.spawn(Node { display: Display::Grid, grid_template_columns: RepeatedGridTrack::flex(4, 1.0), column_gap: Val::Px(3.0), row_gap: Val::Px(3.0), ..default() }).with_children(|g| {
        for (i, s) in sc.inv.iter().enumerate().take(INV_SLOTS) {
            let e = button(g, UiAct::Slot(i), Node { height: Val::Px(40.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Center, justify_content: JustifyContent::Center, row_gap: Val::Px(1.0), ..default() }, INSET);
            if let Some(s) = s {
                let c = rgb(item_color(&s.item));
                g.commands().entity(e).with_children(|b| {
                    b.spawn((Node { width: Val::Px(16.0), height: Val::Px(12.0), border: UiRect::all(Val::Px(1.0)), ..default() }, BackgroundColor(c), BorderColor::all(Color::BLACK), Pickable::IGNORE));
                    let l = if s.qty > 1 { format!("{} x{}", short(&s.item), s.qty) } else { short(&s.item).to_string() };
                    b.spawn(text(l, 11.0, TEXT));
                });
            }
        }
    });
    p.spawn(text("Click an item to use it, right-click for options.", 11.0, DIM)).insert(Node { margin: UiRect::top(Val::Px(4.0)), ..default() });
}

fn side_skills(p: &mut ChildSpawnerCommands, sc: &Scape) {
    let mut total = 0;
    for s in SKILLS {
        let xp = sc.skills.get(s);
        let lvl = level_for_xp(xp);
        total += lvl;
        let (a, b) = (xp_for_level(lvl), xp_for_level((lvl + 1).min(MAX_LEVEL)));
        let frac = if lvl >= MAX_LEVEL || b <= a { 1.0 } else { ((xp - a) / (b - a)).clamp(0.0, 1.0) as f32 };
        p.spawn(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(4.0)), margin: UiRect::bottom(Val::Px(3.0)), ..default() }).insert(BackgroundColor(INSET)).with_children(|r| {
            r.spawn(Node { justify_content: JustifyContent::SpaceBetween, ..default() }).with_children(|h| {
                h.spawn(text(s.label(), 15.0, TEXT));
                h.spawn(text(format!("{lvl} / 99"), 15.0, GOLD));
            });
            r.spawn((Node { height: Val::Px(5.0), margin: UiRect::vertical(Val::Px(2.0)), ..default() }, BackgroundColor(Color::srgb(0.25, 0.12, 0.08)))).with_children(|bar| {
                bar.spawn((Node { width: Val::Percent(frac * 100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(GOOD)));
            });
            let next = if lvl >= MAX_LEVEL { "max".into() } else { format!("{} to go", fmt_xp(b - xp)) };
            r.spawn(text(format!("{} xp, {next}", fmt_xp(xp)), 11.0, DIM));
        });
    }
    p.spawn(text(format!("Total level {total}    Combat level {}", sc.skills.combat_level()), 13.0, GOLD));
}

/// One paperdoll slot: what's in it (click = take it off).
fn gear_slot(p: &mut ChildSpawnerCommands, label: &str, item: Option<&String>, act: UiAct) {
    let e = button(p, act, Node { width: Val::Px(66.0), height: Val::Px(50.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Center, justify_content: JustifyContent::Center, row_gap: Val::Px(2.0), ..default() }, INSET);
    let (sw, l, c) = match item {
        Some(it) => (Some(rgb(item_color(it))), short(it).to_string(), GOLD),
        None => (None, label.to_string(), DIM),
    };
    p.commands().entity(e).with_children(|b| {
        if let Some(sw) = sw {
            b.spawn((Node { width: Val::Px(18.0), height: Val::Px(12.0), border: UiRect::all(Val::Px(1.0)), ..default() }, BackgroundColor(sw), BorderColor::all(Color::BLACK), Pickable::IGNORE));
        }
        b.spawn(text(l, 11.0, c));
    });
}

fn side_gear(p: &mut ChildSpawnerCommands, sc: &Scape) {
    let w = weapon(sc.weapon.as_deref());
    let lvl = sc.skills.level(Skill::Ranged);
    let g = &sc.gear;
    // paperdoll: helmet on top, cape / suit / weapon across, shield below
    p.spawn(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, row_gap: Val::Px(4.0), margin: UiRect::vertical(Val::Px(4.0)), ..default() }).with_children(|d| {
        gear_slot(d, "Helmet", g.helmet.as_ref(), UiAct::UnequipGear("helmet"));
        d.spawn(Node { column_gap: Val::Px(4.0), ..default() }).with_children(|r| {
            gear_slot(r, "Cape", g.cape.as_ref(), UiAct::UnequipGear("cape"));
            gear_slot(r, "Suit", g.suit.as_ref(), UiAct::UnequipGear("suit"));
            gear_slot(r, "Weapon", sc.weapon.as_ref(), UiAct::UnequipGear("weapon"));
        });
        gear_slot(d, "Shield", g.shield.as_ref(), UiAct::UnequipGear("shield"));
    });
    let stats = format!(
        "Armour +{}   Defence {}\nMax hit {}   Accuracy +{}\nRange {} tile{}, every {:.1} s\nHitpoints {} / {}   Combat {}",
        sc.armour,
        sc.skills.level(Skill::Defence),
        max_hit(lvl, w.str),
        w.acc,
        w.range,
        if w.range == 1 { "" } else { "s" },
        w.speed as f32 * 0.6,
        sc.hp,
        sc.max_hp,
        sc.skills.combat_level()
    );
    p.spawn(text(stats, 13.0, TEXT)).insert(Node { margin: UiRect::top(Val::Px(4.0)), ..default() });
    p.spawn(text("Click a slot to take it off. Wear armour from your pack; make it at the fabricator from chitin and iron.", 11.0, DIM)).insert(Node { margin: UiRect::top(Val::Px(6.0)), ..default() });
}

fn side_quests(p: &mut ChildSpawnerCommands, sc: &Scape) {
    p.spawn(Node { justify_content: JustifyContent::SpaceBetween, margin: UiRect::bottom(Val::Px(4.0)), ..default() }).with_children(|h| {
        h.spawn(text("Quests", 15.0, TEXT));
        h.spawn(text(format!("Quest points: {}", sc.qp), 13.0, GOLD));
    });
    for q in QUESTS {
        let st = sc.quests.get(q.id);
        let (col, line) = match st.map(|s| s.s.as_str()) {
            Some("done") => (GOOD, None),
            Some(_) => {
                let s = st.unwrap();
                let step = &q.steps[(s.step as usize).min(q.steps.len() - 1)];
                let have = if step.kind == "deliver" { count(&sc.inv, step.target) } else { s.p };
                (GOLD, Some(format!("{} ({}/{})", step.text, have.min(step.n), step.n)))
            }
            None => (BAD, None),
        };
        p.spawn((Node { flex_direction: FlexDirection::Column, padding: UiRect::axes(Val::Px(5.0), Val::Px(2.0)), margin: UiRect::bottom(Val::Px(2.0)), ..default() }, BackgroundColor(INSET))).with_children(|r| {
            r.spawn(text(q.name, 13.0, col));
            if let Some(l) = line {
                r.spawn(text(l, 11.0, DIM));
            }
        });
    }
    p.spawn(text("Red: not started. Yellow: on it. Green: done. Talk to colonists for work.", 11.0, DIM)).insert(Node { margin: UiRect::top(Val::Px(4.0)), ..default() });
}

/// A grid of item cells (locker / trade lists).
fn item_grid(p: &mut ChildSpawnerCommands, items: &[Slot], cols: u16, act: impl Fn(usize) -> Option<UiAct>) {
    p.spawn(Node { display: Display::Grid, grid_template_columns: RepeatedGridTrack::px(cols, 52.0), column_gap: Val::Px(3.0), row_gap: Val::Px(3.0), ..default() }).with_children(|g| {
        for (i, s) in items.iter().enumerate() {
            let node = Node { height: Val::Px(40.0), flex_direction: FlexDirection::Column, align_items: AlignItems::Center, justify_content: JustifyContent::Center, row_gap: Val::Px(1.0), border: UiRect::all(Val::Px(1.0)), ..default() };
            let e = match act(i) {
                Some(a) => button(g, a, node, INSET),
                None => g.spawn((node, BackgroundColor(INSET), BorderColor::all(EDGE))).id(),
            };
            let c = rgb(item_color(&s.item));
            g.commands().entity(e).with_children(|b| {
                b.spawn((Node { width: Val::Px(16.0), height: Val::Px(12.0), border: UiRect::all(Val::Px(1.0)), ..default() }, BackgroundColor(c), BorderColor::all(Color::BLACK), Pickable::IGNORE));
                let l = if s.qty > 1 { format!("{} x{}", short(&s.item), s.qty) } else { short(&s.item).to_string() };
                b.spawn(text(l, 10.0, TEXT));
            });
        }
    });
}

fn bank_panel(p: &mut ChildSpawnerCommands, items: &[Slot]) {
    p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(12.0)), row_gap: Val::Px(6.0), width: Val::Px(470.0), max_width: Val::Percent(96.0), ..default() }))).with_children(|f| {
        f.spawn(Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, ..default() }).with_children(|h| {
            h.spawn(text(format!("Colony locker  ({}/100)", items.len()), 20.0, GOLD));
            label_button(h, "Close", UiAct::CloseBank, 14.0);
        });
        if items.is_empty() {
            f.spawn(text("Empty. Click items in your pack to store them here.", 13.0, DIM));
        } else {
            item_grid(f, items, 8, |i| Some(UiAct::Withdraw(i, Qty::N(1))));
        }
        f.spawn(text("Click to take one out, right-click for more. Click your pack to put things in. Everything stacks in here.", 11.0, DIM));
    });
}

fn trade_panel(p: &mut ChildSpawnerCommands, t: &super::TradeView) {
    p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(12.0)), row_gap: Val::Px(6.0), width: Val::Px(520.0), max_width: Val::Percent(96.0), ..default() }))).with_children(|f| {
        let title = if t.stage >= 2 { format!("Confirm trade with {}", t.name) } else { format!("Trading with {}", t.name) };
        f.spawn(text(title, 20.0, GOLD));
        f.spawn(Node { column_gap: Val::Px(12.0), ..default() }).with_children(|cols| {
            for (mine, list) in [(true, &t.mine), (false, &t.theirs)] {
                cols.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), padding: UiRect::all(Val::Px(6.0)), width: Val::Px(240.0), min_height: Val::Px(150.0), border: UiRect::all(Val::Px(1.0)), ..default() }, BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.25)), BorderColor::all(EDGE))).with_children(|c| {
                    c.spawn(text(if mine { "Your offer".to_string() } else { format!("{}'s offer", t.name) }, 14.0, TEXT));
                    if list.is_empty() {
                        c.spawn(text("(nothing)", 12.0, DIM));
                    } else {
                        let stage = t.stage;
                        item_grid(c, list, 4, |i| (mine && stage < 2).then_some(UiAct::Unoffer(i)));
                    }
                    let ok = if mine { t.me_ok } else { t.them_ok };
                    c.spawn(text(if ok { "Accepted" } else { "Waiting..." }, 12.0, if ok { GOOD } else { DIM }));
                });
            }
        });
        let hint = if t.stage >= 2 { "Check the deal carefully, then accept again. Nothing changes hands until you both do." } else { "Click items in your pack to offer them; click your offer to take them back. Any change resets both accepts." };
        f.spawn(text(hint, 11.0, if t.stage >= 2 { BAD } else { DIM }));
        f.spawn(Node { column_gap: Val::Px(8.0), ..default() }).with_children(|b| {
            label_button(b, if t.stage >= 2 { "Confirm" } else { "Accept" }, UiAct::TradeAccept, 15.0);
            label_button(b, "Decline", UiAct::TradeDecline, 15.0);
        });
    });
}

fn lamp_panel(p: &mut ChildSpawnerCommands, sc: &Scape) {
    p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(12.0)), row_gap: Val::Px(6.0), width: Val::Px(360.0), max_width: Val::Percent(96.0), ..default() }))).with_children(|f| {
        f.spawn(text("Insight Lamp", 20.0, GOLD));
        f.spawn(text("Rub it on a skill. Memories that aren't yours flood in.", 12.0, DIM));
        f.spawn(Node { display: Display::Grid, grid_template_columns: RepeatedGridTrack::flex(2, 1.0), column_gap: Val::Px(6.0), row_gap: Val::Px(6.0), ..default() }).with_children(|g| {
            for k in SKILLS {
                let lvl = sc.skills.level(k);
                let node = Node { padding: UiRect::all(Val::Px(6.0)), flex_direction: FlexDirection::Column, border: UiRect::all(Val::Px(1.0)), ..default() };
                // a maxed skill would only waste the lamp: shown, not clickable
                let maxed = lvl >= MAX_LEVEL;
                let e = if maxed { g.spawn((node, BackgroundColor(INSET), BorderColor::all(EDGE))).id() } else { button(g, UiAct::Rub(k), node, INSET) };
                g.commands().entity(e).with_children(|b| {
                    b.spawn(text(format!("{} ({lvl})", k.label()), 14.0, if maxed { DIM } else { TEXT }));
                    if maxed {
                        b.spawn(text("maxed", 12.0, DIM));
                    } else {
                        b.spawn(text(format!("+{} xp", fmt_xp(lamp_xp(lvl))), 12.0, GOOD));
                    }
                });
            }
        });
        label_button(f, "Cancel", UiAct::CloseLamp, 14.0);
    });
}

fn dialog_box(p: &mut ChildSpawnerCommands, sc: &Scape) {
    if let Some(d) = &sc.dialog {
        p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(10.0)), row_gap: Val::Px(6.0), width: Val::Percent(100.0), ..default() }))).with_children(|f| {
            f.spawn(Node { column_gap: Val::Px(10.0), ..default() }).with_children(|r| {
                // portrait: their suit colour and initial
                let c = super::look::suit_color(&d.name);
                r.spawn((Node { width: Val::Px(52.0), height: Val::Px(52.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, border: UiRect::all(Val::Px(2.0)), border_radius: BorderRadius::all(Val::Px(4.0)), flex_shrink: 0.0, ..default() }, BackgroundColor(rgb(c)), BorderColor::all(EDGE_HI))).with_children(|b| {
                    b.spawn((text(d.name.chars().next().unwrap_or('?').to_string(), 26.0, Color::WHITE), shadow()));
                });
                r.spawn(Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(3.0), flex_shrink: 1.0, ..default() }).with_children(|c| {
                    let who = match &d.role {
                        Some(role) => format!("{} ({role})", d.name),
                        None => d.name.clone(),
                    };
                    c.spawn(text(who, 15.0, GOLD));
                    c.spawn(text(d.text.clone(), 14.0, TEXT));
                });
            });
            for o in &d.options {
                let e = button(f, UiAct::Choose(o.id.clone()), Node { padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)), ..default() }, INSET);
                f.commands().entity(e).with_children(|b| {
                    b.spawn(text(o.text.clone(), 14.0, Color::srgb(0.55, 0.85, 1.0)));
                });
            }
        });
    } else if let Some((_, name)) = &sc.trade_req {
        p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(10.0)), row_gap: Val::Px(6.0), ..default() }))).with_children(|f| {
            f.spawn(text(format!("{name} wishes to trade with you."), 15.0, super::TRADE));
            f.spawn(Node { column_gap: Val::Px(8.0), ..default() }).with_children(|b| {
                label_button(b, "Trade", UiAct::TradeReq(true), 14.0);
                label_button(b, "Ignore", UiAct::TradeReq(false), 14.0);
            });
        });
    }
}

fn surge_banner(p: &mut ChildSpawnerCommands, sc: &Scape) {
    let Some(sv) = &sc.surge else { return };
    p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, padding: UiRect::axes(Val::Px(18.0), Val::Px(6.0)), row_gap: Val::Px(3.0), min_width: Val::Px(300.0), ..default() }))).insert(BorderColor::all(Color::srgb(0.75, 0.3, 1.0))).with_children(|f| {
        f.spawn((text("RIFT SURGE", 18.0, Color::srgb(0.85, 0.55, 1.0)), shadow()));
        f.spawn((text("", 13.0, TEXT), SurgeText));
        if sv.phase == "boss" {
            f.spawn((Node { width: Val::Px(260.0), height: Val::Px(8.0), ..default() }, BackgroundColor(Color::srgb(0.35, 0.05, 0.05)))).with_children(|b| {
                b.spawn((Node { width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(Color::srgb(0.7, 0.3, 1.0)), SurgeBar));
            });
        }
    });
}

/// The surge banner's line: phase and countdown; and the boss's health (0..1).
fn surge_status(sc: &Scape) -> (String, Option<f32>) {
    let Some(sv) = &sc.surge else { return (String::new(), None) };
    let secs = (sv.ends_tick.saturating_sub(sc.tick_n) as f32 * 0.6).round() as u32;
    let clock = format!("{}:{:02}", secs / 60, secs % 60);
    let boss = sc.npcs.values().filter(|a| a.kind == "leviathan" && a.anim != anim::DEAD).min_by_key(|a| super::chebyshev((a.tile.x, a.tile.y), (sv.at.x, sv.at.y)));
    match sv.phase.as_str() {
        "warning" => (format!("A rift is building near the landing site. It opens in {clock}."), None),
        "open" => (format!("Creatures are pouring out! The big one comes through in {clock}."), None),
        "boss" => (format!("The Void Leviathan is here. It leaves in {clock}."), boss.map(|b| (b.hp as f32 / b.max.max(1) as f32).clamp(0.0, 1.0))),
        p => (p.to_string(), None),
    }
}

fn fab_panel(p: &mut ChildSpawnerCommands, sc: &Scape) {
    p.spawn((Blocker, panel(Node { flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(10.0)), row_gap: Val::Px(4.0), width: Val::Px(620.0), max_width: Val::Percent(96.0), ..default() }))).with_children(|f| {
        f.spawn(Node { justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center, margin: UiRect::bottom(Val::Px(4.0)), ..default() }).with_children(|h| {
            h.spawn(text("Fabricator", 20.0, GOLD));
            label_button(h, "Close", UiAct::CloseFab, 14.0);
        });
        let lvl = sc.skills.level(Skill::Fabrication);
        // two columns: tools and weapons, armour
        f.spawn(Node { display: Display::Grid, grid_template_columns: RepeatedGridTrack::flex(2, 1.0), column_gap: Val::Px(4.0), row_gap: Val::Px(3.0), ..default() }).with_children(|f| {
        for r in RECIPES {
            let has = r.inputs.iter().all(|(it, q)| count(&sc.inv, it) >= *q);
            let known = r.quest.is_none_or(|q| sc.quests.get(q).is_some_and(|s| s.s == "done"));
            let ok = lvl >= r.level && known;
            f.spawn((Node { align_items: AlignItems::Center, column_gap: Val::Px(6.0), padding: UiRect::axes(Val::Px(5.0), Val::Px(2.0)), ..default() }, BackgroundColor(INSET))).with_children(|row| {
                row.spawn((Node { width: Val::Px(12.0), height: Val::Px(12.0), flex_shrink: 0.0, ..default() }, BackgroundColor(rgb(item_color(r.id)))));
                row.spawn(Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, ..default() }).with_children(|c| {
                    let lock = if known { String::new() } else { " - quest".into() };
                    c.spawn(text(format!("{} (lvl {}{lock})", item_name(r.id), r.level), 12.0, if ok { TEXT } else { BAD }));
                    let need: Vec<String> = r.inputs.iter().map(|(it, q)| format!("{q} {} ({})", short(it), count(&sc.inv, it))).collect();
                    c.spawn(text(need.join(" + "), 10.0, if has { GOOD } else { DIM }));
                });
                if ok && has {
                    label_button(row, "Make", UiAct::Make(r.id), 12.0);
                }
            });
        }
        });
        f.spawn(text("Making keeps going while you have the materials. Walk away to stop.", 11.0, DIM));
    });
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn refresh(
    mut commands: Commands,
    scape: Option<ResMut<Scape>>,
    st: Res<UiState>,
    menu: Option<Res<Menu>>,
    orbit: Option<Res<Orbit>>,
    mut roots: Query<&mut Visibility, (With<ScapeUi>, Without<NamePrompt>)>,
    mut lines: Query<(&ChatLine, &mut Text, &mut TextColor), (Without<ChatInput>, Without<HoverText>, Without<StatusText>, Without<HpText>, Without<RunText>)>,
    mut input: Query<&mut Text, (With<ChatInput>, Without<ChatLine>, Without<HoverText>, Without<StatusText>, Without<HpText>, Without<RunText>)>,
    mut misc: Query<(&mut Text, Has<HoverText>, Has<StatusText>, Has<HpText>, Has<RunText>, Has<SurgeText>), (Without<ChatLine>, Without<ChatInput>)>,
    layers: Query<(Entity, Has<SideContent>, Has<FabRoot>, Has<CtxRoot>, Has<DialogRoot>, Has<SurgeRoot>), Or<(With<SideContent>, With<FabRoot>, With<CtxRoot>, With<DialogRoot>, With<SurgeRoot>)>>,
    children: Query<&Children>,
    mut tabs: Query<(&TabBtn, &mut BackgroundColor, &Hovered)>,
    mut btns: Query<(&Hovered, &mut BorderColor), With<Btn>>,
    (mut needle, mut boss_bar): (Query<&mut UiTransform, With<Needle>>, Query<&mut Node, With<SurgeBar>>),
    win: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut last: Local<(u8, bool, Option<(Vec2, usize, String)>)>,
) {
    let Some(mut sc) = scape else { return };
    let hidden = menu.as_ref().is_some_and(|m| m.open);
    let v = if hidden { Visibility::Hidden } else { Visibility::Inherited };
    for mut r in &mut roots {
        if *r != v {
            *r = v;
        }
    }
    if sc.chat_dirty {
        sc.chat_dirty = false;
        let box_w = win.single().map_or(CHAT_W, |w| CHAT_W.min(w.width() * 0.6));
        let shown = chat_lines(&sc.chat, box_w);
        let pad = CHAT_LINES - shown.len();
        for (l, mut t, mut c) in &mut lines {
            let (s, col) = l.0.checked_sub(pad).and_then(|i| shown.get(i)).cloned().unwrap_or((" ".into(), TEXT));
            t.0 = s;
            c.0 = col;
        }
        if let Ok(mut t) = input.single_mut() {
            t.0 = match &st.typing {
                Some(s) => format!("{}: {s}*", sc.name),
                None => "Press Enter to chat".into(),
            };
        }
    }
    let hp_frac = sc.hp as f32 / sc.max_hp.max(1) as f32;
    let (surge_line, boss) = surge_status(&sc);
    if let (Some(f), Ok(mut n)) = (boss, boss_bar.single_mut()) {
        n.width = Val::Percent(f * 100.0);
    }
    for (mut t, hover, status, hp, run, surge) in &mut misc {
        let want = if hover {
            st.hover.clone()
        } else if status {
            let bots = sc.players.values().filter(|a| a.role.is_some()).count();
            let others = sc.players.len().saturating_sub(1 + bots);
            if sc.cave.is_some() {
                format!("MarsScape - {} - {} other player{} down here", sc.status, others, if others == 1 { "" } else { "s" })
            } else {
                format!("MarsScape - {} - {} other player{} and {bots} colonist{} here", sc.status, others, if others == 1 { "" } else { "s" }, if bots == 1 { "" } else { "s" })
            }
        } else if hp {
            format!("{}", sc.hp)
        } else if run {
            (if sc.run { "RUN" } else { "walk" }).into()
        } else if surge {
            surge_line.clone()
        } else {
            continue;
        };
        if t.0 != want {
            t.0 = want;
        }
    }
    let _ = hp_frac;
    if let (Ok(mut n), Some(o)) = (needle.single_mut(), orbit.as_ref()) {
        n.rotation = Rot2::radians(-o.yaw);
    }
    for (tb, mut bg, h) in &mut tabs {
        let c = if tb.0 == st.tab { EDGE } else if h.get() { Color::srgb(0.25, 0.15, 0.09) } else { INSET };
        if bg.0 != c {
            bg.0 = c;
        }
    }
    for (h, mut b) in &mut btns {
        let c = if h.get() { EDGE_HI } else { EDGE };
        if b.top != c {
            *b = BorderColor::all(c);
        }
    }
    let tab_changed = last.0 != st.tab || last.1 != st.fab_open;
    if sc.dirty || tab_changed {
        last.0 = st.tab;
        last.1 = st.fab_open;
        sc.dirty = false;
        if let Some((root, ..)) = layers.iter().find(|l| l.1) {
            for c in children.get(root).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                commands.entity(c).despawn();
            }
            commands.entity(root).with_children(|p| match st.tab {
                1 => side_skills(p, &sc),
                2 => side_gear(p, &sc),
                3 => side_quests(p, &sc),
                _ => side_pack(p, &sc),
            });
        }
        // the centre panel: a trade beats the locker beats a lamp beats the fabricator
        if let Some((root, ..)) = layers.iter().find(|l| l.2) {
            for c in children.get(root).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                commands.entity(c).despawn();
            }
            if let Some(t) = &sc.trade {
                commands.entity(root).with_children(|p| trade_panel(p, t));
            } else if let Some(b) = &sc.bank {
                commands.entity(root).with_children(|p| bank_panel(p, b));
            } else if sc.lamp.is_some() {
                commands.entity(root).with_children(|p| lamp_panel(p, &sc));
            } else if st.fab_open {
                commands.entity(root).with_children(|p| fab_panel(p, &sc));
            }
        }
        if let Some((root, ..)) = layers.iter().find(|l| l.4) {
            for c in children.get(root).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                commands.entity(c).despawn();
            }
            commands.entity(root).with_children(|p| dialog_box(p, &sc));
        }
        if let Some((root, ..)) = layers.iter().find(|l| l.5) {
            for c in children.get(root).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                commands.entity(c).despawn();
            }
            commands.entity(root).with_children(|p| surge_banner(p, &sc));
        }
    }
    let sig = st.ctx.as_ref().map(|c| (c.at, c.opts.len(), c.opts.first().map_or(String::new(), |o| o.label.clone())));
    if sig != last.2 {
        last.2 = sig;
        if let Some((root, ..)) = layers.iter().find(|l| l.3) {
            for c in children.get(root).map(|c| c.iter().collect::<Vec<_>>()).unwrap_or_default() {
                commands.entity(c).despawn();
            }
            if let Some(m) = &st.ctx {
                // keep the whole menu on screen
                let size = win.single().map_or(Vec2::new(1280.0, 720.0), |w| Vec2::new(w.width(), w.height()));
                let h = 30.0 + m.opts.len() as f32 * 27.0;
                let (x, y) = ((m.at.x - 70.0).clamp(4.0, (size.x - 230.0).max(4.0)), (m.at.y - 6.0).clamp(4.0, (size.y - h).max(4.0)));
                commands.entity(root).with_children(|p| {
                    p.spawn((Blocker, panel(Node { position_type: PositionType::Absolute, left: Val::Px(x), top: Val::Px(y), flex_direction: FlexDirection::Column, padding: UiRect::all(Val::Px(3.0)), min_width: Val::Px(150.0), ..default() })))
                        .with_children(|c| {
                            c.spawn(text("Choose option", 13.0, DIM)).insert(Node { margin: UiRect::new(Val::Px(4.0), Val::Px(4.0), Val::Px(1.0), Val::Px(3.0)), ..default() });
                            for o in &m.opts {
                                let e = button(c, o.act.clone(), Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)), ..default() }, INSET);
                                let col = match o.act {
                                    UiAct::Attack(_) => BAD,
                                    UiAct::Node(_) | UiAct::Item(_, "equip" | "wear" | "eat" | "rub") | UiAct::Talk(_) => GOLD,
                                    UiAct::TradeWith(_) => super::TRADE,
                                    UiAct::Cancel => DIM,
                                    _ => TEXT,
                                };
                                c.commands().entity(e).insert(BorderColor::all(Color::NONE)).with_children(|b| {
                                    b.spawn(text(o.label.clone(), 15.0, col));
                                });
                            }
                        });
                });
            }
        }
    }
}

/// One world-anchored overlay: what to draw and where (logical px, top-left corner).
enum Ov {
    Tag(String, Color),
    Bar(f32),
    Splat(i32, bool),
    Drop(String),
}

/// World-anchored overlays: name tags, health bars, hitsplats; XP drops by the minimap. Overlays
/// that would land on a HUD panel are hidden (they'd draw over it).
#[allow(clippy::too_many_arguments)]
pub fn overlays(
    mut commands: Commands,
    scape: Option<Res<Scape>>,
    menu: Option<Res<Menu>>,
    cam: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    root: Query<Entity, With<Overlay>>,
    win: Query<&Window, With<bevy::window::PrimaryWindow>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &InheritedVisibility), With<Blocker>>,
    mut nodes: Query<&mut Node, Without<Overlay>>,
    mut pool: Local<HashMap<String, (Entity, Option<Entity>)>>,
) {
    let (Some(sc), Ok(root), Ok((camera, gt))) = (scape, root.single(), cam.single()) else {
        pool.clear();
        return;
    };
    let (width, scale) = win.single().map_or((1280.0, 1.0), |w| (w.width(), w.scale_factor()));
    let hidden = menu.is_some_and(|m| m.open);
    let panels: Vec<(ComputedNode, UiGlobalTransform)> = blockers.iter().filter(|b| b.2.get()).map(|b| (*b.0, *b.1)).collect();
    let covered = |p: Vec2| panels.iter().any(|(n, t)| n.contains_point(*t, p * scale));
    let mut want: Vec<(String, Vec2, Ov)> = Vec::new();
    if !hidden {
        let me = sc.me;
        for (id, a) in sc.players.iter() {
            let Ok(p) = camera.world_to_viewport(gt, a.pos + Vec3::Y * (a.height + 0.25)) else { continue };
            let name = match &a.role {
                Some(r) => format!("{} ({r})", a.name),
                None => a.name.clone(),
            };
            let col = if Some(*id) == me {
                GOLD
            } else if a.role.is_some() {
                COLONIST
            } else {
                TEXT
            };
            // the whole tag (up to two lines), not just its anchor, must stay clear of panels
            let corners = [Vec2::new(-60.0, -22.0), Vec2::new(60.0, -22.0), Vec2::new(-60.0, 16.0), Vec2::new(60.0, 16.0), Vec2::ZERO];
            if !corners.iter().any(|c| covered(p + *c)) {
                want.push((format!("name{id}:{name}"), p - Vec2::new(70.0, 20.0), Ov::Tag(name, col)));
            }
        }
        let bars = sc.players.iter().map(|(i, a)| (format!("pb{i}"), a)).chain(sc.npcs.iter().map(|(i, a)| (format!("nb{i}"), a)));
        for (key, a) in bars {
            if a.combat_t >= 6.0 || a.anim == anim::DEAD || a.max <= 0 {
                continue;
            }
            let Ok(p) = camera.world_to_viewport(gt, a.pos + Vec3::Y * (a.height + if a.npc { 0.2 } else { 0.05 })) else { continue };
            if !covered(p - Vec2::new(20.0, 0.0)) && !covered(p + Vec2::new(20.0, 4.0)) {
                want.push((key, p - Vec2::new(20.0, 4.0), Ov::Bar((a.hp as f32 / a.max as f32).clamp(0.0, 1.0))));
            }
        }
        // hitsplats: up to 4 per target in the classic cluster (centre, above, left, right)
        const SLOTS: [Vec2; 4] = [Vec2::new(0.0, 0.0), Vec2::new(0.0, -24.0), Vec2::new(-20.0, -12.0), Vec2::new(20.0, -12.0)];
        let mut per: HashMap<Who, Vec<&super::Splat>> = HashMap::new();
        for s in &sc.splats {
            per.entry(s.who).or_default().push(s);
        }
        for (who, mut list) in per {
            list.sort_by_key(|s| s.k);
            let skip = list.len().saturating_sub(SLOTS.len());
            let a = match who {
                Who::P(i) => sc.players.get(&i),
                Who::N(i) => sc.npcs.get(&i),
            };
            let Some(a) = a else { continue };
            let Ok(p) = camera.world_to_viewport(gt, a.pos + Vec3::Y * a.height * 0.55) else { continue };
            for s in list.into_iter().skip(skip) {
                // a splat keeps the slot it got (k-based), so older ones don't jump around
                let at = p + SLOTS[(s.k as usize) % SLOTS.len()] - Vec2::new(13.0, 11.0);
                if !covered(at + Vec2::new(13.0, 11.0)) {
                    want.push((format!("hs{}", s.k), at, Ov::Splat(s.dmg, s.heal)));
                }
            }
        }
        for (i, d) in sc.drops.iter().enumerate() {
            let y = 150.0 - d.t * 40.0 + i as f32 * 20.0;
            want.push((format!("xp{i}:{}", d.text), Vec2::new(width - 380.0, y), Ov::Drop(d.text.clone())));
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    for (key, at, ov) in want {
        let ent = *pool.entry(key.clone()).or_insert_with(|| {
            // spawned already in place (a fresh node can't be moved until next frame)
            let node = |w: f32, h: f32| Node {
                position_type: PositionType::Absolute,
                left: Val::Px(at.x),
                top: Val::Px(at.y),
                width: if w > 0.0 { Val::Px(w) } else { Val::Auto },
                height: if h > 0.0 { Val::Px(h) } else { Val::Auto },
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            };
            let mut out = (Entity::PLACEHOLDER, None);
            commands.entity(root).with_children(|c| {
                out = match &ov {
                    Ov::Tag(name, col) => (c.spawn((node(140.0, 0.0), Pickable::IGNORE)).with_children(|t| {
                        t.spawn((text(name.clone(), 14.0, *col), shadow()));
                    }).id(), None),
                    Ov::Bar(f) => {
                        let mut bar = None;
                        let e = c
                            .spawn((node(40.0, 5.0), BackgroundColor(Color::srgb(0.85, 0.08, 0.05)), Pickable::IGNORE))
                            .with_children(|b| {
                                let mut n = node(0.0, 0.0);
                                n.position_type = PositionType::Relative;
                                n.left = Val::Auto;
                                n.top = Val::Auto;
                                n.width = Val::Percent(f * 100.0);
                                n.height = Val::Percent(100.0);
                                bar = Some(b.spawn((n, BackgroundColor(Color::srgb(0.1, 0.85, 0.1)), Pickable::IGNORE)).id());
                            })
                            .id();
                        (e, bar)
                    }
                    Ov::Splat(dmg, heal) => {
                        // red damage, blue miss, green heal
                        let bg = if *heal {
                            Color::srgb(0.10, 0.60, 0.15)
                        } else if *dmg > 0 {
                            Color::srgb(0.75, 0.05, 0.05)
                        } else {
                            Color::srgb(0.15, 0.35, 0.85)
                        };
                        let mut n = node(26.0, 22.0);
                        n.border = UiRect::all(Val::Px(1.0));
                        n.border_radius = BorderRadius::all(Val::Px(6.0));
                        (c.spawn((n, BackgroundColor(bg), BorderColor::all(Color::BLACK), Pickable::IGNORE)).with_children(|t| {
                            t.spawn(text(format!("{dmg}"), 15.0, Color::WHITE));
                        }).id(), None)
                    }
                    Ov::Drop(label) => (c.spawn((node(0.0, 0.0), Pickable::IGNORE)).with_children(|t| {
                        t.spawn((text(label.clone(), 16.0, Color::WHITE), shadow()));
                    }).id(), None),
                };
            });
            out
        });
        if let Ok(mut n) = nodes.get_mut(ent.0) {
            n.left = Val::Px(at.x);
            n.top = Val::Px(at.y);
        }
        if let (Ov::Bar(f), Some(bar)) = (&ov, ent.1) {
            if let Ok(mut n) = nodes.get_mut(bar) {
                n.width = Val::Percent(f * 100.0);
            }
        }
        seen.insert(key);
    }
    pool.retain(|k, (e, _)| {
        let keep = seen.contains(k);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
}

/// Rotating minimap (up = camera forward): ground shading from the walk grid, dots for nodes,
/// creatures and astronauts. Redrawn a few times a second.
pub fn minimap(scape: Option<ResMut<Scape>>, orbit: Option<Res<Orbit>>, handles: Option<Res<UiHandles>>, mut images: ResMut<Assets<Image>>, time: Res<Time<Real>>, mut next: Local<f32>) {
    let (Some(mut sc), Some(o), Some(h)) = (scape, orbit, handles) else { return };
    let now = time.elapsed_secs();
    if now < *next {
        return;
    }
    *next = now + 0.2;
    let Some(me) = sc.my_pos() else { return };
    let n = MAP_PX as i32;
    let half = n as f32 * 0.5;
    let mpp = 0.6f32; // metres per minimap pixel
    // map up = camera forward (the camera looks along -dir)
    let (s, c) = (-o.yaw).sin_cos();
    let rot = |dx: f32, dy: f32| Vec2::new(dx * c - dy * s, dx * s + dy * c);
    let mut px = vec![0u8; (n * n * 4) as usize];
    let base = sc.grid.height(super::grid::tile_of(me.x, me.z));
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
            let r = (dx * dx + dy * dy).sqrt();
            if r > half {
                continue;
            }
            let w = rot(dx, dy) * mpp;
            let t = super::grid::tile_of(me.x + w.x, me.z + w.y);
            let hgt = sc.grid.height(t);
            let i = ((y * n + x) * 4) as usize;
            if !hgt.is_finite() {
                px[i..i + 4].copy_from_slice(&[10, 6, 5, 255]);
                continue;
            }
            let hw = sc.grid.height(t + IVec2::new(-1, -1));
            let shade = if hw.is_finite() { ((hw - hgt) * 0.35).clamp(-0.35, 0.35) } else { 0.0 };
            let rel = if base.is_finite() { ((hgt - base) / 40.0).clamp(-0.25, 0.25) } else { 0.0 };
            let walk = sc.grid.ground_ok(t);
            let (mut rr, mut gg, mut bb) = if walk { (0.62, 0.40, 0.27) } else { (0.40, 0.22, 0.16) };
            let k = 1.0 + shade + rel;
            rr *= k;
            gg *= k;
            bb *= k;
            if r > half - 2.0 {
                (rr, gg, bb) = (0.3, 0.17, 0.08);
            }
            px[i] = (rr.clamp(0.0, 1.0) * 255.0) as u8;
            px[i + 1] = (gg.clamp(0.0, 1.0) * 255.0) as u8;
            px[i + 2] = (bb.clamp(0.0, 1.0) * 255.0) as u8;
            px[i + 3] = 255;
        }
    }
    // dots: world -> map pixel
    let inv = |p: Vec3| {
        let d = Vec2::new(p.x - me.x, p.z - me.z) / mpp;
        let m = Vec2::new(d.x * c + d.y * s, -d.x * s + d.y * c);
        (m.x + half, m.y + half)
    };
    let mut dot = |p: Vec3, col: [u8; 3], size: i32| {
        let (fx, fy) = inv(p);
        for oy in 0..size {
            for ox in 0..size {
                let (x, y) = (fx as i32 + ox - size / 2, fy as i32 + oy - size / 2);
                let (dx, dy) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
                if x < 0 || y < 0 || x >= n || y >= n || dx * dx + dy * dy > (half - 3.0) * (half - 3.0) {
                    continue;
                }
                let i = ((y * n + x) * 4) as usize;
                px[i..i + 3].copy_from_slice(&col);
            }
        }
    };
    for nd in sc.nodes.values() {
        let col = match nd.kind.as_str() {
            "fabricator" => [80, 230, 255],
            "rift" => [255, 60, 20],
            "locker" => [80, 255, 120],
            "pit" | "ladder" => [255, 200, 60],
            "void_vein" => [190, 90, 255],
            "fossil" => [255, 220, 150],
            "ice" => [220, 240, 255],
            "hematite" => [150, 90, 90],
            "outcrop" => [240, 210, 150],
            _ => [200, 120, 70],
        };
        dot(nd.pos, if nd.up { col } else { [90, 70, 60] }, 4);
    }
    for a in sc.npcs.values().filter(|a| a.anim != anim::DEAD) {
        dot(a.pos, [255, 230, 30], 3);
    }
    // the Rift Surge: a pulsing magenta blob
    if let (Some(sv), true) = (&sc.surge, sc.cave.is_none()) {
        let (x, z) = super::grid::tile_center(sv.at);
        let size = 6 + ((now * 4.0).sin() * 2.0) as i32;
        dot(Vec3::new(x, me.y, z), [255, 60, 255], size);
    }
    let my = sc.me;
    for (i, a) in sc.players.iter() {
        if Some(*i) != my {
            dot(a.pos, [255, 255, 255], 3);
        }
    }
    dot(me, [255, 255, 255], 4);
    if let Some(mut img) = images.get_mut(&h.minimap) {
        img.data = Some(px);
    }
    let _ = &mut sc;
}
