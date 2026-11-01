//! MarsScape characters: one rigged, animated glTF space explorer ("Astronaut" by Quaternius, CC0;
//! see viewer/assets/CREDITS.md). It is used for you, for other players and for the colonists.
//!
//! - **Outfits.** Outfits are material colours. Players get their name colour on the trim;
//!   colonists get their job's colours.
//! - **Armour.** Armour tints the suit plates and helmet, and hangs pieces on bones.
//! - **Gear.** The gun and the pick sit in the right hand: they are children of the `Wrist.R` bone.
//! - **Animation.** An `AnimationGraph` holds the clips, and `AnimationTransitions` cross-fades
//!   between them. The clip follows the game state: idle, run (its playback rate matched to the
//!   tile speed), aim and shoot, mine (a pick swing), survey and craft (interact), a flinch when
//!   hit, and death.
//!
//! The model is embedded in the binary, so the browser build fetches nothing extra.

use super::look::{self, ArmourPiece, Ctx, Kit, Limb, Part, Rig};
use super::proto::{Gear, anim};
use bevy::gltf::{GltfMaterialName, GltfMeshName};
use bevy::prelude::*;
use std::f32::consts::{FRAC_PI_2, PI};
use std::time::Duration;

const MODEL: &str = "embedded://marsmaker_viewer/astronaut.glb";

/// Put the model into the embedded asset source (called from the plugin).
pub fn register(app: &mut App) {
    app.world().resource::<bevy::asset::io::embedded::EmbeddedAssetRegistry>().insert_asset(
        std::path::PathBuf::new(),
        std::path::Path::new("marsmaker_viewer/astronaut.glb"),
        include_bytes!("../../assets/astronaut.glb").as_slice(),
    );
}

/// The model's animations, in file order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Clip {
    Idle,
    IdleGun,
    Aim,
    Shoot,
    Run,
    Hit,
    Death,
    Interact,
    Slash,
}
const CLIPS: usize = 9;

/// Model scale: the file is 1.85 m tall with the helmet.
const SCALE: f32 = 1.08;

/// Running speed (m/s) of the Run clip at playback rate 1 and model scale 1. Measured from the
/// planted foot.
const RUN_SPEED: f32 = 2.9 * SCALE;

#[derive(Resource)]
pub struct HeroAssets {
    scene: Handle<WorldAsset>,
    graph: Handle<AnimationGraph>,
    nodes: Vec<AnimationNodeIndex>,
}

impl HeroAssets {
    fn node(&self, c: Clip) -> AnimationNodeIndex {
        self.nodes[c as usize]
    }
}

/// A character's colours.
#[derive(Clone, Copy)]
pub struct Outfit {
    /// undersuit
    pub under: [f32; 3],
    /// joints, gloves and soles
    pub dark: [f32; 3],
    /// the hard plates and the helmet shell
    pub plate: [f32; 3],
    /// piping and trim
    pub trim: [f32; 3],
    /// visor glow
    pub visor: [f32; 3],
}

/// Players wear white plates with their name colour on the trim. Colonists dress for their job.
pub fn outfit(name: &str, role: Option<&str>) -> Outfit {
    let o = |under, dark, plate, trim, visor| Outfit { under, dark, plate, trim, visor };
    match role {
        Some("guard") => o([0.16, 0.17, 0.20], [0.08, 0.08, 0.10], [0.30, 0.33, 0.40], [0.85, 0.16, 0.14], [1.0, 0.30, 0.20]),
        Some("medic") => o([0.80, 0.82, 0.86], [0.30, 0.32, 0.36], [0.96, 0.96, 0.97], [0.85, 0.12, 0.16], [0.40, 1.0, 0.60]),
        Some("miner") => o([0.40, 0.29, 0.20], [0.14, 0.12, 0.10], [0.96, 0.66, 0.12], [0.16, 0.16, 0.16], [1.0, 0.60, 0.20]),
        Some("surveyor") => o([0.26, 0.38, 0.26], [0.12, 0.15, 0.12], [0.78, 0.74, 0.60], [0.15, 0.62, 0.70], [0.30, 0.95, 1.0]),
        Some("quartermaster") => o([0.36, 0.33, 0.24], [0.15, 0.14, 0.11], [0.70, 0.64, 0.48], [0.40, 0.55, 0.25], [1.0, 0.85, 0.40]),
        Some("trader") => o([0.24, 0.14, 0.32], [0.10, 0.07, 0.12], [0.58, 0.34, 0.74], [0.95, 0.74, 0.22], [1.0, 0.80, 0.35]),
        Some(_) => o([0.22, 0.26, 0.28], [0.12, 0.13, 0.15], [0.80, 0.80, 0.78], [0.35, 0.65, 0.40], [0.40, 0.90, 1.0]),
        None => o([0.20, 0.25, 0.27], [0.11, 0.12, 0.14], [0.88, 0.87, 0.85], look::suit_color(name), [0.35, 0.85, 1.0]),
    }
}

/// Which part of the outfit a mesh primitive is (by its glTF mesh and material names).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    Under,
    Dark,
    Plate,
    Helm,
    HelmTrim,
    Trim,
    Visor,
}

fn slot(mesh: &str, mat: &str) -> Slot {
    let head = mesh.contains("Head");
    match mat {
        "SciFi_Main" => Slot::Under,
        "SciFi_MainDark" => Slot::Dark,
        "SciFi_Light" if head => Slot::Helm,
        "SciFi_Light" => Slot::Plate,
        "SciFi_Light_Accent" if head => Slot::HelmTrim,
        "SciFi_Light_Accent" => Slot::Trim,
        "Grey" if head => Slot::Visor,
        _ => Slot::Dark,
    }
}

/// Bones that carry things: the right hand (gun, pick), the left forearm (shield), the head (helmet
/// crest) and the chest (pauldrons, cape).
const SOCKETS: [&str; 4] = ["Wrist.R", "LowerArm.L", "Head", "Chest"];
const HAND: usize = 0;
const ARM: usize = 1;
const HEAD: usize = 2;
const CHEST: usize = 3;

#[derive(Component)]
pub struct Hero {
    pub outfit: Outfit,
    /// armour worn (only known for your own character)
    pub gear: Option<Gear>,
    /// the glTF scene under the root, the entity with its `AnimationPlayer`
    model: Option<Entity>,
    player: Option<Entity>,
    ready: bool,
    /// materials and armour need redoing
    dirty: bool,
    meshes: Vec<(Entity, Slot)>,
    /// one child per `SOCKETS` bone, scaled back to metres (the armature is in centimetres)
    sockets: [Option<Entity>; 4],
    cur: Option<Clip>,
    /// a one-shot (shoot / flinch) plays until then
    until: f32,
    shots: u32,
    hits: u32,
    /// seconds since the rig last moved, and its last speed (m/s)
    still: f32,
    pace: f32,
}

impl Hero {
    pub fn new(outfit: Outfit) -> Self {
        Hero { outfit, gear: None, model: None, player: None, ready: false, dirty: true, meshes: Vec::new(), sockets: [None; 4], cur: None, until: 0.0, shots: 0, hits: 0, still: 99.0, pace: RUN_SPEED }
    }
    /// New armour to show.
    pub fn wear(&mut self, gear: &Gear) {
        self.gear = Some(gear.clone());
        self.dirty = true;
    }
}

/// Load the model, put it under new characters, and wire up each one once its scene has spawned
/// (animation graph, outfit materials, sockets, gun and pick).
#[allow(clippy::too_many_arguments)]
pub fn setup(
    mut commands: Commands,
    server: Res<AssetServer>,
    ha: Option<Res<HeroAssets>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut heroes: Query<(Entity, &mut Hero)>,
    kids: Query<&Children>,
    anim_players: Query<(), With<AnimationPlayer>>,
    names: Query<&Name>,
    prims: Query<(Option<&GltfMeshName>, &GltfMaterialName)>,
    (mut meshes, mut mats, mut kit): (ResMut<Assets<Mesh>>, ResMut<Assets<StandardMaterial>>, ResMut<Kit>),
) {
    let Some(ha) = ha else {
        if heroes.is_empty() {
            return;
        }
        let clips: Vec<Handle<AnimationClip>> = (0..CLIPS).map(|i| server.load(GltfAssetLabel::Animation(i).from_asset(MODEL))).collect();
        let (graph, nodes) = AnimationGraph::from_clips(clips);
        commands.insert_resource(HeroAssets { scene: server.load(GltfAssetLabel::Scene(0).from_asset(MODEL)), graph: graphs.add(graph), nodes });
        return;
    };
    for (root, mut h) in &mut heroes {
        if h.ready {
            continue;
        }
        let Some(model) = h.model else {
            // The glTF faces +Z; MarsScape models face -Z. A touch over life size, so the characters
            // read at the usual camera distance. Hidden until it's dressed: its first frame would
            // otherwise draw the file's own materials (and, in the browser, link a shadow pipeline).
            let m = commands.spawn((WorldAssetRoot(ha.scene.clone()), Transform::from_rotation(Quat::from_rotation_y(PI)).with_scale(Vec3::splat(SCALE)), Visibility::Hidden, ChildOf(root))).id();
            h.model = Some(m);
            continue;
        };
        let mut player = None;
        let mut bones = [None; 4];
        let mut prim = Vec::new();
        for d in kids.iter_descendants(model) {
            if anim_players.contains(d) {
                player = Some(d);
            }
            if let Ok(n) = names.get(d) {
                if let Some(i) = SOCKETS.iter().position(|s| *s == n.as_str()) {
                    bones[i] = Some(d);
                }
            }
            if let Ok((mesh, mat)) = prims.get(d) {
                prim.push((d, slot(mesh.map_or("", |m| m.0.as_str()), &mat.0)));
                // Browser: no character shadows. A skinned shadow pipeline is one more WebGL
                // program to link, measured at 1.4 s of frozen page on ANGLE/D3D11.
                #[cfg(target_arch = "wasm32")]
                commands.entity(d).insert(bevy::light::NotShadowCaster);
            }
        }
        // the scene hasn't spawned yet
        let Some(player) = player else { continue };
        commands.entity(player).insert((AnimationGraphHandle(ha.graph.clone()), AnimationTransitions::new()));
        for (i, b) in bones.iter().enumerate() {
            if let Some(b) = b {
                h.sockets[i] = Some(commands.spawn((Transform::from_scale(Vec3::splat(0.01)), Visibility::default(), ChildOf(*b))).id());
            }
        }
        if let Some(hand) = h.sockets[HAND] {
            let mut cx = Ctx { commands: &mut commands, meshes: &mut meshes, mats: &mut mats, kit: &mut kit };
            hand_gear(&mut cx, hand);
        }
        commands.entity(model).insert(Visibility::Inherited);
        h.meshes = prim;
        h.player = Some(player);
        h.ready = true;
        h.dirty = true;
    }
}

/// Where the fist closes around a grip, in the `Wrist.R` bone's frame (metres): along the hand
/// (+Y), toward the palm (-Z). The thumb side is -X.
const FIST: Vec3 = Vec3::new(0.0, 0.085, -0.035);

/// The blaster and the pick, hidden until `animate` shows them.
fn hand_gear(cx: &mut Ctx, hand: Entity) {
    // gun space: barrel -Z, top +Y, origin in the grip. In the hand the barrel runs along the
    // fingers (+Y) and the top is the thumb side (-X).
    let gun_rot = Quat::from_mat3(&Mat3::from_cols(Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y));
    // pick space: haft +Y, head along Z. The haft comes out of the thumb side of the fist and the
    // head points the way the fingers do.
    let pick_rot = Quat::from_mat3(&Mat3::from_cols(Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y));
    let body = cx.m("hg_body", || look::lowpoly(Cuboid::new(0.075, 0.10, 0.30)));
    let rail = cx.m("hg_rail", || look::lowpoly(Cuboid::new(0.035, 0.03, 0.20)));
    let barrel = cx.m("hg_barrel", || look::lowpoly(Cylinder::new(0.024, 0.20).mesh().resolution(8)).rotated_by(Quat::from_rotation_x(FRAC_PI_2)));
    let muzzle = cx.m("hg_muzzle", || look::lowpoly(Cylinder::new(0.036, 0.035).mesh().resolution(8)).rotated_by(Quat::from_rotation_x(FRAC_PI_2)));
    let grip = cx.m("hg_grip", || look::lowpoly(Cuboid::new(0.05, 0.14, 0.065)));
    let cell = cx.m("hg_cell", || look::lowpoly(Cuboid::new(0.084, 0.032, 0.13)));
    let metal = cx.shiny([0.22, 0.23, 0.26]);
    let dark = cx.c([0.10, 0.10, 0.11]);
    let light = cx.shiny([0.70, 0.72, 0.76]);
    let glow = cx.glow([0.30, 0.90, 1.0], 6.0);
    let haft = cx.m("hp_haft", || look::lowpoly(Cylinder::new(0.022, 0.72).mesh().resolution(6)));
    let wrap = cx.m("hp_wrap", || look::lowpoly(Cylinder::new(0.03, 0.16).mesh().resolution(6)));
    let head = cx.m("hp_head", || look::lowpoly(Cuboid::new(0.06, 0.07, 0.24)));
    let spike = cx.m("hp_spike", || look::lowpoly(Cone::new(0.035, 0.2)).rotated_by(Quat::from_rotation_x(FRAC_PI_2)));
    let band = cx.m("hp_band", || look::lowpoly(Cuboid::new(0.066, 0.076, 0.07)));
    let steel = cx.shiny([0.62, 0.62, 0.66]);
    let hot = cx.glow([1.0, 0.55, 0.15], 4.0);
    let at = |p: Vec3, r: Quat| Transform::from_translation(p).with_rotation(r);
    cx.commands.entity(hand).with_children(|s| {
        s.spawn((at(FIST, gun_rot), Visibility::Hidden, Part { limb: Limb::Gun, rest: Quat::IDENTITY, k: 0.0 })).with_children(|g| {
            g.spawn((Mesh3d(body), MeshMaterial3d(metal.clone()), Transform::from_xyz(0.0, 0.08, -0.07)));
            g.spawn((Mesh3d(rail), MeshMaterial3d(light.clone()), Transform::from_xyz(0.0, 0.145, -0.08)));
            g.spawn((Mesh3d(barrel), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.0, 0.09, -0.31)));
            g.spawn((Mesh3d(muzzle), MeshMaterial3d(glow.clone()), Transform::from_xyz(0.0, 0.09, -0.415)));
            g.spawn((Mesh3d(grip), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.0, -0.01, 0.025).with_rotation(Quat::from_rotation_x(-0.25))));
            g.spawn((Mesh3d(cell), MeshMaterial3d(glow.clone()), Transform::from_xyz(0.0, 0.07, -0.04)));
        });
        s.spawn((at(FIST, pick_rot), Visibility::Hidden, Part { limb: Limb::Tool, rest: Quat::IDENTITY, k: 0.0 })).with_children(|p| {
            p.spawn((Mesh3d(haft), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.0, 0.22, 0.0)));
            p.spawn((Mesh3d(wrap), MeshMaterial3d(metal.clone()), Transform::IDENTITY));
            p.spawn((Mesh3d(head), MeshMaterial3d(metal), Transform::from_xyz(0.0, 0.56, 0.0)));
            p.spawn((Mesh3d(spike.clone()), MeshMaterial3d(steel.clone()), Transform::from_xyz(0.0, 0.56, -0.2).with_rotation(Quat::from_rotation_y(PI))));
            p.spawn((Mesh3d(spike), MeshMaterial3d(steel), Transform::from_xyz(0.0, 0.56, 0.2)));
            p.spawn((Mesh3d(band), MeshMaterial3d(hot), Transform::from_xyz(0.0, 0.56, 0.0)));
        });
    });
}

/// Armour colour and whether it's a relic piece (gold, glowing).
fn tier(item: &str) -> ([f32; 3], bool) {
    if item.starts_with("chitin") {
        ([0.40, 0.58, 0.48], false)
    } else if item.starts_with("iron") {
        ([0.56, 0.58, 0.63], false)
    } else {
        ([0.92, 0.70, 0.26], true)
    }
}

/// Apply outfit colours and armour to characters that changed.
#[allow(clippy::too_many_arguments)]
pub fn dress(
    mut commands: Commands,
    mut heroes: Query<&mut Hero>,
    kids: Query<&Children>,
    pieces: Query<(), With<ArmourPiece>>,
    (mut meshes, mut mats, mut kit): (ResMut<Assets<Mesh>>, ResMut<Assets<StandardMaterial>>, ResMut<Kit>),
) {
    for mut h in &mut heroes {
        if !h.ready || !h.dirty {
            continue;
        }
        h.dirty = false;
        let o = h.outfit;
        let gear = h.gear.clone().unwrap_or_default();
        let mut cx = Ctx { commands: &mut commands, meshes: &mut meshes, mats: &mut mats, kit: &mut kit };
        let suit = gear.suit.as_deref().map(tier);
        let helm = gear.helmet.as_deref().map(tier);
        let armour = |cx: &mut Ctx, t: Option<([f32; 3], bool)>, plain: [f32; 3]| match t {
            Some((c, true)) => cx.kit.mat(cx.mats, c, 0.0, 0.3, 0.75),
            Some((c, false)) => cx.kit.mat(cx.mats, c, 0.0, 0.35, 0.55),
            None => cx.kit.mat(cx.mats, plain, 0.0, 0.55, 0.05),
        };
        let plate = armour(&mut cx, suit, o.plate);
        let shell = armour(&mut cx, helm, o.plate);
        let relic = suit.is_some_and(|s| s.1);
        let trim = if relic { cx.glow([1.0, 0.78, 0.35], 1.1) } else { cx.kit.mat(cx.mats, o.trim, 0.0, 0.6, 0.0) };
        // a helmet re-skins the whole helmet frame, not just its shell
        let helm_trim = match helm {
            Some((_, true)) => cx.glow([1.0, 0.78, 0.35], 1.1),
            Some((c, false)) => cx.kit.mat(cx.mats, c.map(|v| v * 0.8), 0.0, 0.35, 0.55),
            None => cx.kit.mat(cx.mats, o.trim, 0.0, 0.6, 0.0),
        };
        let under = cx.kit.mat(cx.mats, o.under, 0.0, 0.85, 0.0);
        let dark = cx.kit.mat(cx.mats, o.dark, 0.0, 0.8, 0.0);
        // dark glass with a faint glow of the outfit's colour (the helmet's hoses share it)
        let visor = cx.kit.mat_lit(cx.mats, [0.05, 0.06, 0.08], o.visor.map(|c| c * 0.22), 0.18, 0.4);
        for (e, s) in h.meshes.iter() {
            let m = match s {
                Slot::Under => under.clone(),
                Slot::Dark => dark.clone(),
                Slot::Plate => plate.clone(),
                Slot::Helm => shell.clone(),
                Slot::HelmTrim => helm_trim.clone(),
                Slot::Trim => trim.clone(),
                Slot::Visor => visor.clone(),
            };
            cx.commands.entity(*e).insert(MeshMaterial3d(m));
        }
        // armour pieces: drop the old ones, hang the new ones on their bones
        for s in h.sockets.iter().flatten() {
            for d in kids.iter_descendants(*s) {
                if pieces.contains(d) {
                    cx.commands.entity(d).despawn();
                }
            }
        }
        if let (Some(t), Some(head)) = (helm, h.sockets[HEAD]) {
            // a low fin along the top of the helmet
            let crest = cx.m("ah_crest", || look::lowpoly(Sphere::new(1.0).mesh().ico(1).unwrap()).scaled_by(Vec3::new(0.03, 0.06, 0.19)));
            let m = if t.1 { cx.glow([1.0, 0.8, 0.35], 4.0) } else { cx.kit.mat(cx.mats, t.0, 0.0, 0.35, 0.55) };
            cx.commands.entity(head).with_children(|p| {
                p.spawn((Mesh3d(crest), MeshMaterial3d(m), Transform::from_xyz(0.0, 0.25, -0.01), ArmourPiece));
            });
        }
        if let (Some(t), Some(chest)) = (suit, h.sockets[CHEST]) {
            let pad = cx.m("as_pad", || look::lowpoly(Sphere::new(0.13).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.15, 0.65, 1.25)));
            let glyph = cx.m("as_glyph", || look::lowpoly(Cuboid::new(0.035, 0.16, 0.02)));
            let glow = cx.glow([1.0, 0.8, 0.35], 5.0);
            cx.commands.entity(chest).with_children(|p| {
                for x in [-0.19f32, 0.19] {
                    p.spawn((Mesh3d(pad.clone()), MeshMaterial3d(plate.clone()), Transform::from_xyz(x, 0.12, -0.01).with_rotation(Quat::from_rotation_z(-x.signum() * 0.35)), ArmourPiece));
                }
                if t.1 {
                    for x in [-0.07f32, 0.0, 0.07] {
                        p.spawn((Mesh3d(glyph.clone()), MeshMaterial3d(glow.clone()), Transform::from_xyz(x, 0.0, 0.17), ArmourPiece));
                    }
                }
            });
        }
        if let (Some(_), Some(arm)) = (&gear.shield, h.sockets[ARM]) {
            // on the outside of the forearm: the bone's +Y runs to the wrist, +Z points outwards
            let disc = cx.m("ash_disc", || look::lowpoly(Cylinder::new(0.19, 0.03).mesh().resolution(6)).rotated_by(Quat::from_rotation_x(FRAC_PI_2)));
            let rim = cx.m("ash_rim", || look::lowpoly(Torus::new(0.18, 0.225).mesh().minor_resolution(4).major_resolution(6)).rotated_by(Quat::from_rotation_x(FRAC_PI_2)));
            // a dark energy pane with a soft cyan glow in a steel frame
            let core = cx.kit.mat_lit(cx.mats, [0.10, 0.25, 0.32], [0.10, 0.45, 0.60], 0.15, 0.3);
            let frame = cx.shiny([0.5, 0.52, 0.58]);
            cx.commands.entity(arm).with_children(|p| {
                p.spawn((Mesh3d(disc), MeshMaterial3d(core), Transform::from_xyz(0.0, 0.12, 0.085), ArmourPiece));
                p.spawn((Mesh3d(rim), MeshMaterial3d(frame), Transform::from_xyz(0.0, 0.12, 0.09), ArmourPiece));
            });
        }
        if let (Some(_), Some(chest)) = (&gear.cape, h.sockets[CHEST]) {
            // hangs from the shoulders down the back (-Z), the hem a little further back
            let cape = cx.m("ac_cape", || look::lowpoly(Cuboid::new(0.56, 1.0, 0.025)).translated_by(Vec3::new(0.0, -0.5, 0.0)));
            let star = cx.m("ar_star", || look::lowpoly(Cuboid::new(0.04, 0.04, 0.01)));
            let cloth = cx.c([0.28, 0.10, 0.45]);
            let glow = cx.glow([0.8, 0.7, 1.0], 6.0);
            cx.commands.entity(chest).with_children(|p| {
                p.spawn((Mesh3d(cape), MeshMaterial3d(cloth), Transform::from_xyz(0.0, 0.13, -0.2).with_rotation(Quat::from_rotation_x(0.12)), ArmourPiece)).with_children(|c| {
                    for i in 0..7 {
                        let (x, y) = (((i * 37) % 5) as f32 * 0.1 - 0.2, -0.15 - ((i * 53) % 8) as f32 * 0.1);
                        c.spawn((Mesh3d(star.clone()), MeshMaterial3d(glow.clone()), Transform::from_xyz(x, y, -0.016)));
                    }
                });
            });
        }
    }
}

/// Play the clip the game state calls for, cross-fading from the last one.
pub fn drive(time: Res<Time>, ha: Option<Res<HeroAssets>>, mut heroes: Query<(&mut Hero, &Rig)>, mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>) {
    let Some(ha) = ha else { return };
    let now = time.elapsed_secs();
    let dt = time.delta_secs();
    for (mut h, rig) in &mut heroes {
        let Some(pe) = h.player else { continue };
        let Ok((mut p, mut tr)) = players.get_mut(pe) else { continue };
        let moving = rig.speed > 0.2;
        if moving {
            h.pace = rig.speed;
            h.still = 0.0;
        } else {
            h.still += dt;
        }
        // keep running through the moment between two steps when the next tick is a bit late
        let running = rig.anim != anim::DEAD && (moving || (h.still < 0.15 && matches!(rig.anim, anim::WALK | anim::RUN)));
        let base = if rig.anim == anim::DEAD {
            Clip::Death
        } else if running {
            Clip::Run
        } else {
            match rig.anim {
                anim::ATTACK => Clip::Aim,
                anim::MINE => Clip::Slash,
                anim::SURVEY | anim::CRAFT => Clip::Interact,
                _ if rig.armed => Clip::IdleGun,
                _ => Clip::Idle,
            }
        };
        let mut play = |h: &mut Hero, c: Clip, fade: f32| {
            let a = tr.play(&mut p, ha.node(c), Duration::from_secs_f32(fade.max(0.001)));
            if !matches!(c, Clip::Shoot | Clip::Hit | Clip::Death) {
                a.repeat();
            }
            if c == Clip::Slash {
                a.set_speed(0.8);
            }
            h.cur = Some(c);
        };
        let calm = base != Clip::Death && !running;
        if rig.shots != h.shots {
            h.shots = rig.shots;
            if calm {
                play(&mut h, Clip::Shoot, 0.06);
                h.until = now + 0.5;
            }
        }
        if rig.hits != h.hits {
            h.hits = rig.hits;
            if calm && h.cur != Some(Clip::Shoot) {
                play(&mut h, Clip::Hit, 0.06);
                h.until = now + 0.42;
            }
        }
        if h.cur.is_none() {
            play(&mut h, base, 0.0);
        } else if (now >= h.until || base == Clip::Death || running) && h.cur != Some(base) {
            let fade = match base {
                Clip::Run => 0.18,
                Clip::Death => 0.12,
                _ => 0.25,
            };
            play(&mut h, base, fade);
            h.until = 0.0;
        }
        if h.cur == Some(Clip::Run) {
            if let Some(a) = p.animation_mut(ha.node(Clip::Run)) {
                a.set_speed((h.pace / RUN_SPEED).clamp(0.8, 2.3));
            }
        }
    }
}
