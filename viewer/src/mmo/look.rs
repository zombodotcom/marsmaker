//! MarsScape models: procedural low-poly (flat-shaded) astronauts, void creatures, resource nodes, the
//! fabricator, rifts, the click marker and projectiles. Every model is a root entity facing -Z
//! with posable child parts (`Part`) that `animate` swings. All materials are plain opaque
//! StandardMaterials (colour / emissive / roughness only), so they share the buggy's pipelines
//! and don't add shader compiles in the browser.

use super::proto::anim;
use crate::visuals::Fx;
use bevy::prelude::*;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Limb {
    LegL,
    LegR,
    ArmL,
    ArmR,
    /// pick / scanner in the right hand (gathering)
    Tool,
    /// gun in the right hand (combat)
    Gun,
    /// quadruped legs: front-left, front-right, back-left, back-right
    Fl,
    Fr,
    Bl,
    Br,
    Tail,
    Head,
    /// crawler legs 0..6 (even = left), tripod gait
    Crawl(u8),
    /// floater tentacles; `Part::k` is the sway phase offset
    Tentacle(u8),
    /// crystal shards orbiting a golem
    Shard(u8),
    /// a floater's body; `Part::k` is its rest height
    Hover,
    /// worm body segment `i` (0 = head end); `animate` sways it sideways
    Seg(u8),
}

#[derive(Component)]
pub struct Part {
    pub limb: Limb,
    pub rest: Quat,
    /// per-part parameter (see `Limb`)
    pub k: f32,
}

/// Root of an animated model; `scale` sizes the gait.
#[derive(Component)]
pub struct Rig {
    pub phase: f32,
    pub scale: f32,
    pub anim: u8,
    /// seconds since death (creatures fall over, then sink)
    pub dead_t: f32,
    pub recoil: f32,
}

#[derive(Component)]
pub struct NodeFull;
#[derive(Component)]
pub struct NodeEmpty;
#[derive(Component)]
pub struct RiftSpark(pub f32);

/// Shared meshes / materials, made on first use.
#[derive(Resource, Default)]
pub struct Kit {
    meshes: HashMap<&'static str, Handle<Mesh>>,
    mats: HashMap<String, Handle<StandardMaterial>>,
}

fn lowpoly(m: impl Into<Mesh>) -> Mesh {
    let mut m: Mesh = m.into();
    m.duplicate_vertices();
    m.compute_flat_normals();
    m
}

impl Kit {
    pub fn mesh(&mut self, meshes: &mut Assets<Mesh>, key: &'static str, f: impl FnOnce() -> Mesh) -> Handle<Mesh> {
        self.meshes.entry(key).or_insert_with(|| meshes.add(f())).clone()
    }
    pub fn mat(&mut self, mats: &mut Assets<StandardMaterial>, rgb: [f32; 3], glow: f32, rough: f32, metal: f32) -> Handle<StandardMaterial> {
        let key = format!("{:.3},{:.3},{:.3},{glow:.2},{rough:.2},{metal:.2}", rgb[0], rgb[1], rgb[2]);
        self.mats
            .entry(key)
            .or_insert_with(|| {
                let c = Color::srgb(rgb[0], rgb[1], rgb[2]);
                mats.add(StandardMaterial {
                    base_color: c,
                    emissive: if glow > 0.0 { (c.to_linear() * glow).into() } else { LinearRgba::BLACK },
                    perceptual_roughness: rough,
                    metallic: metal,
                    ..default()
                })
            })
            .clone()
    }
}

pub struct Ctx<'a, 'w, 's> {
    pub commands: &'a mut Commands<'w, 's>,
    pub meshes: &'a mut Assets<Mesh>,
    pub mats: &'a mut Assets<StandardMaterial>,
    pub kit: &'a mut Kit,
}

impl Ctx<'_, '_, '_> {
    fn m(&mut self, key: &'static str, f: impl FnOnce() -> Mesh) -> Handle<Mesh> {
        self.kit.mesh(self.meshes, key, f)
    }
    fn c(&mut self, rgb: [f32; 3]) -> Handle<StandardMaterial> {
        self.kit.mat(self.mats, rgb, 0.0, 0.85, 0.0)
    }
    fn glow(&mut self, rgb: [f32; 3], g: f32) -> Handle<StandardMaterial> {
        self.kit.mat(self.mats, rgb, g, 0.6, 0.0)
    }
    fn shiny(&mut self, rgb: [f32; 3]) -> Handle<StandardMaterial> {
        self.kit.mat(self.mats, rgb, 0.0, 0.25, 0.6)
    }
}

fn piece(p: &mut ChildSpawnerCommands, mesh: Handle<Mesh>, mat: Handle<StandardMaterial>, t: Transform) -> Entity {
    p.spawn((Mesh3d(mesh), MeshMaterial3d(mat), t)).id()
}

fn limb(p: &mut ChildSpawnerCommands, limb: Limb, mesh: Handle<Mesh>, mat: Handle<StandardMaterial>, at: Vec3) -> Entity {
    p.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(at), Part { limb, rest: Quat::IDENTITY, k: 0.0 })).id()
}

/// Suit colour from the name (stable across sessions and clients).
pub fn suit_color(name: &str) -> [f32; 3] {
    let mut h = 2166136261u32;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    const SUITS: [[f32; 3]; 8] = [
        [0.92, 0.45, 0.12],
        [0.20, 0.45, 0.85],
        [0.85, 0.82, 0.78],
        [0.25, 0.62, 0.35],
        [0.80, 0.22, 0.25],
        [0.95, 0.78, 0.20],
        [0.55, 0.35, 0.75],
        [0.20, 0.65, 0.70],
    ];
    SUITS[(h % 8) as usize]
}

pub fn gun_color(weapon: Option<&str>) -> [f32; 3] {
    match weapon {
        Some("rifle") => [0.38, 0.32, 0.24],
        Some("plasma_gun") => [0.25, 0.85, 0.75],
        _ => [0.22, 0.22, 0.25],
    }
}

/// An astronaut, 1.8 m tall.
pub fn astronaut(cx: &mut Ctx, name: &str, at: Vec3) -> Entity {
    let suit = cx.c(suit_color(name));
    let white = cx.c([0.97, 0.97, 0.96]);
    let grey = cx.c([0.35, 0.35, 0.38]);
    let dark = cx.c([0.16, 0.15, 0.15]);
    let visor = cx.shiny([0.75, 0.55, 0.20]);
    let leg = cx.m("a_leg", || lowpoly(Cuboid::new(0.22, 0.82, 0.25)).translated_by(Vec3::new(0.0, -0.41, 0.0)));
    let boot = cx.m("a_boot", || lowpoly(Cuboid::new(0.26, 0.16, 0.34)).translated_by(Vec3::new(0.0, -0.8, -0.04)));
    let arm = cx.m("a_arm", || lowpoly(Cuboid::new(0.17, 0.62, 0.19)).translated_by(Vec3::new(0.0, -0.29, 0.0)));
    let glove = cx.m("a_glove", || lowpoly(Cuboid::new(0.19, 0.14, 0.2)).translated_by(Vec3::new(0.0, -0.64, 0.0)));
    let torso = cx.m("a_torso", || lowpoly(Cuboid::new(0.52, 0.64, 0.34)));
    let pack = cx.m("a_pack", || lowpoly(Cuboid::new(0.44, 0.54, 0.22)));
    let helmet = cx.m("a_helmet", || lowpoly(Sphere::new(0.25).mesh().ico(1).unwrap()));
    let face = cx.m("a_visor", || lowpoly(Sphere::new(0.2).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.0, 0.82, 0.62)));
    let collar = cx.m("a_collar", || lowpoly(Cylinder::new(0.2, 0.09)));
    let aerial = cx.m("a_aerial", || lowpoly(Cuboid::new(0.03, 0.42, 0.03)));
    let pick = cx.m("a_pick", || lowpoly(Cuboid::new(0.06, 0.06, 0.55)).translated_by(Vec3::new(0.0, -0.66, -0.22)));
    let pick_head = cx.m("a_pickhead", || lowpoly(Cuboid::new(0.36, 0.08, 0.08)).translated_by(Vec3::new(0.0, -0.66, -0.5)));
    let gun = cx.m("a_gun", || lowpoly(Cuboid::new(0.09, 0.12, 0.5)).translated_by(Vec3::new(0.0, -0.64, -0.2)));
    let metal = cx.shiny([0.6, 0.6, 0.62]);
    let gunmat = cx.c(gun_color(None));
    cx.commands
        .spawn((Transform::from_translation(at), Visibility::default(), Rig { phase: 0.0, scale: 1.0, anim: anim::IDLE, dead_t: 0.0, recoil: 0.0 }))
        .with_children(|p| {
            piece(p, torso, suit.clone(), Transform::from_xyz(0.0, 1.18, 0.0));
            piece(p, pack, white.clone(), Transform::from_xyz(0.0, 1.22, 0.27));
            piece(p, collar, grey.clone(), Transform::from_xyz(0.0, 1.52, 0.0));
            piece(p, helmet, white.clone(), Transform::from_xyz(0.0, 1.71, 0.0));
            piece(p, face, visor, Transform::from_xyz(0.0, 1.70, -0.17));
            piece(p, aerial, grey.clone(), Transform::from_xyz(0.14, 1.66, 0.33));
            for (l, x) in [(Limb::LegL, -0.13), (Limb::LegR, 0.13)] {
                let e = limb(p, l, leg.clone(), suit.clone(), Vec3::new(x, 0.86, 0.0));
                p.commands().entity(e).with_children(|c| {
                    piece(c, boot.clone(), dark.clone(), Transform::IDENTITY);
                });
            }
            for (l, x) in [(Limb::ArmL, -0.35), (Limb::ArmR, 0.35)] {
                let e = limb(p, l, arm.clone(), suit.clone(), Vec3::new(x, 1.46, 0.0));
                p.commands().entity(e).with_children(|c| {
                    piece(c, glove.clone(), white.clone(), Transform::IDENTITY);
                    if l == Limb::ArmR {
                        c.spawn((Transform::IDENTITY, Visibility::Hidden, Part { limb: Limb::Tool, rest: Quat::IDENTITY, k: 0.0 })).with_children(|t| {
                            piece(t, pick.clone(), dark.clone(), Transform::IDENTITY);
                            piece(t, pick_head.clone(), metal.clone(), Transform::IDENTITY);
                        });
                        c.spawn((Mesh3d(gun.clone()), MeshMaterial3d(gunmat.clone()), Transform::IDENTITY, Visibility::Hidden, Part { limb: Limb::Gun, rest: Quat::IDENTITY, k: 0.0 }));
                    }
                });
            }
        })
        .id()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Plan {
    /// small six-legged crawler
    Crawler,
    /// lean four-legged predator
    Quad,
    /// hulking rock biped with crystal shards orbiting it
    Golem,
    /// floating tentacled eye cluster
    Floater,
    /// segmented glowing worm
    Worm,
}

/// How each creature kind looks. Names, levels and stats are in proto::CREATURES; a re-skin is
/// one row here plus one there.
pub struct EnemyLook {
    pub kind: &'static str,
    pub plan: Plan,
    /// model scale and on-screen height (m, for health bars and picking)
    pub scale: f32,
    pub height: f32,
    pub skin: [f32; 3],
    /// claws, ridges, shell
    pub dark: [f32; 3],
    /// glowing accents (bioluminescence, crystals, rim)
    pub glow: [f32; 3],
    pub eye: [f32; 3],
    /// crystal spikes on the back (crawlers)
    pub spikes: u8,
}

pub const ENEMY_LOOKS: &[EnemyLook] = &[
    EnemyLook { kind: "skitter", plan: Plan::Crawler, scale: 0.8, height: 0.9, skin: [0.30, 0.12, 0.45], dark: [0.12, 0.05, 0.20], glow: [0.20, 0.95, 1.0], eye: [0.55, 1.0, 1.0], spikes: 0 },
    EnemyLook { kind: "stalker", plan: Plan::Quad, scale: 0.9, height: 1.3, skin: [0.78, 0.56, 0.36], dark: [0.42, 0.28, 0.18], glow: [0.35, 1.0, 0.75], eye: [0.45, 1.0, 0.8], spikes: 0 },
    EnemyLook { kind: "golem", plan: Plan::Golem, scale: 1.4, height: 2.9, skin: [0.22, 0.19, 0.19], dark: [0.12, 0.10, 0.11], glow: [0.70, 0.35, 1.0], eye: [0.80, 0.50, 1.0], spikes: 0 },
    EnemyLook { kind: "leviathan", plan: Plan::Floater, scale: 2.0, height: 6.0, skin: [0.12, 0.06, 0.20], dark: [0.06, 0.03, 0.10], glow: [0.60, 0.30, 1.0], eye: [1.0, 0.80, 0.35], spikes: 0 },
    // lava-tube cave
    EnemyLook { kind: "glowworm", plan: Plan::Worm, scale: 1.0, height: 0.9, skin: [0.20, 0.16, 0.12], dark: [0.10, 0.08, 0.07], glow: [0.55, 1.0, 0.35], eye: [0.75, 1.0, 0.5], spikes: 0 },
    EnemyLook { kind: "crystal_horror", plan: Plan::Crawler, scale: 1.8, height: 2.0, skin: [0.62, 0.72, 0.80], dark: [0.18, 0.20, 0.26], glow: [0.30, 0.85, 1.0], eye: [1.0, 0.35, 0.85], spikes: 7 },
];

pub fn enemy_look(kind: &str) -> &'static EnemyLook {
    ENEMY_LOOKS.iter().find(|l| l.kind == kind).unwrap_or(&ENEMY_LOOKS[0])
}

/// A creature model from its `EnemyLook` row.
pub fn creature(cx: &mut Ctx, kind: &str, at: Vec3) -> Entity {
    let lk = enemy_look(kind);
    let root = cx.commands.spawn((Transform::from_translation(at), Visibility::default(), Rig { phase: 0.0, scale: lk.scale, anim: anim::IDLE, dead_t: 0.0, recoil: 0.0 })).id();
    let skin = cx.c(lk.skin);
    let dark = cx.c(lk.dark);
    let glow = cx.glow(lk.glow, 5.0);
    let eye = cx.glow(lk.eye, 8.0);
    let eyem = cx.m("c_eye", || Sphere::new(0.05).mesh().ico(0).unwrap());
    let s = lk.scale;
    match lk.plan {
        Plan::Crawler => {
            let body = cx.m("k_body", || lowpoly(Sphere::new(0.42).mesh().ico(1).unwrap()).scaled_by(Vec3::new(0.9, 0.42, 1.25)));
            let head = cx.m("k_head", || lowpoly(Sphere::new(0.18).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.1, 0.8, 1.0)));
            let spot = cx.m("k_spot", || lowpoly(Sphere::new(0.07).mesh().ico(0).unwrap()));
            let mandible = cx.m("k_mand", || lowpoly(Cone::new(0.04, 0.2)).rotated_by(Quat::from_rotation_x(-1.9)));
            let crystal = cx.m("k_crystal", || lowpoly(Cone::new(0.07, 0.42)).translated_by(Vec3::new(0.0, 0.21, 0.0)));
            // a leg: out to the side and down to the ground, pivoting at the body
            let leg = cx.m("k_leg", || {
                let upper = lowpoly(Cuboid::new(0.42, 0.06, 0.06)).translated_by(Vec3::new(0.21, 0.0, 0.0)).rotated_by(Quat::from_rotation_z(0.55));
                let lower = lowpoly(Cuboid::new(0.06, 0.42, 0.06)).translated_by(Vec3::new(0.36, 0.02, 0.0)).rotated_by(Quat::from_rotation_z(-0.25));
                let mut m = upper;
                m.merge(&lower).ok();
                m
            });
            cx.commands.entity(root).with_children(|r| {
                r.spawn((Transform::from_scale(Vec3::splat(s)), Visibility::default())).with_children(|p| {
                    piece(p, body, skin.clone(), Transform::from_xyz(0.0, 0.42, 0.05));
                    for (x, z) in [(0.0, -0.15), (0.16, 0.12), (-0.16, 0.12), (0.0, 0.3), (0.09, -0.32), (-0.09, -0.32)] {
                        piece(p, spot.clone(), glow.clone(), Transform::from_xyz(x, 0.58, z));
                    }
                    let h = p.spawn((Mesh3d(head), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.0, 0.42, -0.55), Part { limb: Limb::Head, rest: Quat::IDENTITY, k: 0.0 })).id();
                    p.commands().entity(h).with_children(|c| {
                        for sx in [-1.0, 1.0] {
                            piece(c, eyem.clone(), eye.clone(), Transform::from_xyz(0.08 * sx, 0.06, -0.14));
                            piece(c, mandible.clone(), dark.clone(), Transform::from_xyz(0.07 * sx, -0.06, -0.14));
                        }
                    });
                    for k in 0..lk.spikes {
                        let a = k as f32 * 2.4;
                        let (x, z) = (a.cos() * 0.16, -0.25 + k as f32 * 0.09);
                        piece(p, crystal.clone(), glow.clone(), Transform::from_xyz(x, 0.58, z).with_rotation(Quat::from_rotation_z(a.cos() * 0.5) * Quat::from_rotation_x(0.35 - k as f32 * 0.1)).with_scale(Vec3::splat(0.7 + (k % 3) as f32 * 0.25)));
                    }
                    for i in 0..6u8 {
                        let side = if i % 2 == 0 { -1.0 } else { 1.0 };
                        let z = -0.3 + (i / 2) as f32 * 0.3;
                        // left legs are the right legs turned around
                        let base = if side < 0.0 { Quat::from_rotation_y(std::f32::consts::PI) } else { Quat::IDENTITY };
                        let fan = Quat::from_rotation_y(((i / 2) as f32 - 1.0) * 0.45 * -side);
                        p.spawn((Mesh3d(leg.clone()), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.25 * side, 0.4, z).with_rotation(fan * base), Part { limb: Limb::Crawl(i), rest: fan * base, k: 0.0 }));
                    }
                });
            });
        }
        Plan::Quad => {
            let body = cx.m("q_body", || lowpoly(Cuboid::new(0.42, 0.40, 1.30)));
            let chest = cx.m("q_chest", || lowpoly(Sphere::new(0.32).mesh().ico(1).unwrap()).scaled_by(Vec3::new(0.8, 0.9, 1.0)));
            let head = cx.m("q_head", || lowpoly(Cuboid::new(0.30, 0.28, 0.58)));
            let leg = cx.m("q_leg", || lowpoly(Cuboid::new(0.12, 0.62, 0.14)).translated_by(Vec3::new(0.0, -0.31, 0.0)));
            let tail = cx.m("q_tail", || lowpoly(Cuboid::new(0.07, 0.07, 0.8)).translated_by(Vec3::new(0.0, 0.0, 0.4)));
            let ridge = cx.m("q_ridge", || lowpoly(Cone::new(0.06, 0.22)));
            let stripe = cx.m("q_stripe", || lowpoly(Cuboid::new(0.44, 0.06, 0.10)));
            cx.commands.entity(root).with_children(|r| {
                r.spawn((Transform::from_scale(Vec3::splat(s)), Visibility::default())).with_children(|p| {
                    piece(p, body, skin.clone(), Transform::from_xyz(0.0, 0.82, 0.05));
                    piece(p, chest, skin.clone(), Transform::from_xyz(0.0, 0.86, -0.48));
                    for i in 0..5 {
                        piece(p, ridge.clone(), dark.clone(), Transform::from_xyz(0.0, 1.06, -0.45 + i as f32 * 0.24));
                        piece(p, stripe.clone(), dark.clone(), Transform::from_xyz(0.0, 0.98, -0.40 + i as f32 * 0.24).with_rotation(Quat::from_rotation_x(0.3)));
                    }
                    let h = p.spawn((Mesh3d(head), MeshMaterial3d(skin.clone()), Transform::from_xyz(0.0, 1.0, -0.86), Part { limb: Limb::Head, rest: Quat::IDENTITY, k: 0.0 })).id();
                    p.commands().entity(h).with_children(|c| {
                        for sx in [-1.0, 1.0] {
                            piece(c, eyem.clone(), eye.clone(), Transform::from_xyz(0.1 * sx, 0.07, -0.27).with_scale(Vec3::new(1.4, 0.8, 1.0)));
                        }
                    });
                    for (l, x, z) in [(Limb::Fl, -0.17, -0.5), (Limb::Fr, 0.17, -0.5), (Limb::Bl, -0.17, 0.52), (Limb::Br, 0.17, 0.52)] {
                        limb(p, l, leg.clone(), skin.clone(), Vec3::new(x, 0.66, z));
                    }
                    p.spawn((Mesh3d(tail), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.0, 0.92, 0.7), Part { limb: Limb::Tail, rest: Quat::from_rotation_x(0.35), k: 0.0 }));
                });
            });
        }
        Plan::Golem => {
            let torso = cx.m("g_torso", || lowpoly(Sphere::new(0.55).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.15, 1.0, 0.85)));
            let head = cx.m("g_head", || lowpoly(Sphere::new(0.24).mesh().ico(0).unwrap()));
            let arm = cx.m("g_arm", || lowpoly(Cuboid::new(0.3, 0.75, 0.3)).translated_by(Vec3::new(0.0, -0.36, 0.0)));
            let fist = cx.m("g_fist", || lowpoly(Sphere::new(0.24).mesh().ico(0).unwrap()).translated_by(Vec3::new(0.0, -0.82, 0.0)));
            let leg = cx.m("g_leg", || lowpoly(Cuboid::new(0.32, 0.62, 0.34)).translated_by(Vec3::new(0.0, -0.31, 0.0)));
            let seam = cx.m("g_seam", || lowpoly(Cuboid::new(0.06, 0.5, 0.04)));
            let shard = cx.m("g_shard", || lowpoly(Cone::new(0.09, 0.42)));
            cx.commands.entity(root).with_children(|r| {
                r.spawn((Transform::from_scale(Vec3::splat(s)), Visibility::default())).with_children(|p| {
                    piece(p, torso, skin.clone(), Transform::from_xyz(0.0, 1.25, 0.0).with_rotation(Quat::from_rotation_x(-0.2)));
                    // glowing seams on the chest
                    for (x, a) in [(-0.18, 0.3), (0.12, -0.4), (0.0, 0.05)] {
                        piece(p, seam.clone(), glow.clone(), Transform::from_xyz(x, 1.3, -0.45).with_rotation(Quat::from_rotation_z(a)));
                    }
                    let h = p.spawn((Mesh3d(head), MeshMaterial3d(dark.clone()), Transform::from_xyz(0.0, 1.85, -0.15), Part { limb: Limb::Head, rest: Quat::IDENTITY, k: 0.0 })).id();
                    p.commands().entity(h).with_children(|c| {
                        for sx in [-1.0, 1.0] {
                            piece(c, eyem.clone(), eye.clone(), Transform::from_xyz(0.09 * sx, 0.03, -0.21));
                        }
                    });
                    for (l, x) in [(Limb::LegL, -0.25), (Limb::LegR, 0.25)] {
                        limb(p, l, leg.clone(), dark.clone(), Vec3::new(x, 0.66, 0.0));
                    }
                    for (l, x) in [(Limb::ArmL, -0.72), (Limb::ArmR, 0.72)] {
                        let e = limb(p, l, arm.clone(), skin.clone(), Vec3::new(x, 1.55, -0.05));
                        p.commands().entity(e).with_children(|c| {
                            piece(c, fist.clone(), dark.clone(), Transform::IDENTITY);
                        });
                    }
                    for i in 0..5u8 {
                        p.spawn((Mesh3d(shard.clone()), MeshMaterial3d(glow.clone()), Transform::from_xyz(0.0, 1.4, 0.0), Part { limb: Limb::Shard(i), rest: Quat::IDENTITY, k: 0.0 }));
                    }
                });
            });
        }
        Plan::Worm => {
            let seg = cx.m("w_seg", || lowpoly(Sphere::new(0.3).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.0, 0.8, 1.15)));
            let spot = cx.m("k_spot", || lowpoly(Sphere::new(0.07).mesh().ico(0).unwrap()));
            let fang = cx.m("w_fang", || lowpoly(Cone::new(0.035, 0.18)).rotated_by(Quat::from_rotation_x(-1.6)));
            cx.commands.entity(root).with_children(|r| {
                r.spawn((Transform::from_scale(Vec3::splat(s)), Visibility::default())).with_children(|p| {
                    for i in 0..7u8 {
                        let k = i as f32;
                        let sz = if i == 0 { 1.1 } else { 1.0 - k * 0.09 };
                        let z = -0.62 + k * 0.4;
                        p.spawn((Mesh3d(seg.clone()), MeshMaterial3d(skin.clone()), Transform::from_xyz(0.0, 0.3 * sz, z).with_scale(Vec3::splat(sz)), Visibility::default(), Part { limb: Limb::Seg(i), rest: Quat::IDENTITY, k: z }))
                            .with_children(|c| {
                                // a glowing band on every segment, eyes and fangs on the head
                                for (x, y) in [(0.0, 0.24), (0.2, 0.12), (-0.2, 0.12)] {
                                    piece(c, spot.clone(), glow.clone(), Transform::from_xyz(x, y, 0.0).with_scale(Vec3::splat(1.2)));
                                }
                                if i == 0 {
                                    for sx in [-1.0, 1.0] {
                                        piece(c, eyem.clone(), eye.clone(), Transform::from_xyz(0.12 * sx, 0.1, -0.3).with_scale(Vec3::splat(1.6)));
                                        piece(c, fang.clone(), dark.clone(), Transform::from_xyz(0.08 * sx, -0.08, -0.32));
                                    }
                                }
                            });
                    }
                });
            });
        }
        Plan::Floater => {
            let body = cx.m("l_body", || lowpoly(Sphere::new(0.62).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.0, 0.82, 1.1)));
            let rim = cx.m("l_rim", || lowpoly(Torus::new(0.55, 0.66)));
            // tentacles curl outwards as they hang
            let tent = cx.m("l_tent", || {
                let mut m = lowpoly(Cuboid::new(0.13, 0.55, 0.13)).translated_by(Vec3::new(0.0, -0.27, 0.0));
                m.merge(&lowpoly(Cuboid::new(0.10, 0.5, 0.10)).translated_by(Vec3::new(0.0, -0.25, 0.0)).rotated_by(Quat::from_rotation_x(0.35)).translated_by(Vec3::new(0.0, -0.52, 0.0))).ok();
                m.merge(&lowpoly(Cuboid::new(0.07, 0.45, 0.07)).translated_by(Vec3::new(0.0, -0.22, 0.0)).rotated_by(Quat::from_rotation_x(0.8)).translated_by(Vec3::new(0.0, -0.98, 0.17))).ok();
                m
            });
            let big_eye = cx.m("l_eye", || lowpoly(Sphere::new(0.13).mesh().ico(1).unwrap()));
            let spot = cx.m("k_spot", || lowpoly(Sphere::new(0.07).mesh().ico(0).unwrap()));
            cx.commands.entity(root).with_children(|r| {
                r.spawn((Transform::from_scale(Vec3::splat(s)), Visibility::default())).with_children(|p| {
                    // everything hangs off a hovering body that bobs (Limb::Hover, k = rest height)
                    p.spawn((Transform::from_xyz(0.0, 2.6, 0.0), Visibility::default(), Part { limb: Limb::Hover, rest: Quat::IDENTITY, k: 2.6 })).with_children(|b| {
                        piece(b, body, skin.clone(), Transform::IDENTITY);
                        piece(b, rim, glow.clone(), Transform::from_xyz(0.0, -0.1, 0.0).with_rotation(Quat::from_rotation_x(0.15)));
                        // a cluster of eyes on the front
                        for (x, y, sc) in [(0.0, 0.08, 1.3), (-0.22, 0.2, 0.8), (0.22, 0.2, 0.8), (-0.3, -0.06, 0.7), (0.3, -0.06, 0.7), (-0.12, -0.2, 0.6), (0.12, -0.2, 0.6)] {
                            piece(b, big_eye.clone(), eye.clone(), Transform::from_xyz(x, y, -0.6).with_scale(Vec3::splat(sc)));
                        }
                        for i in 0..8 {
                            let a = i as f32 * 0.785;
                            piece(b, spot.clone(), glow.clone(), Transform::from_xyz(a.cos() * 0.45, 0.42, a.sin() * 0.45));
                        }
                        for i in 0..7u8 {
                            let a = i as f32 * 0.9 + 0.3;
                            let base = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2 - a);
                            b.spawn((Mesh3d(tent.clone()), MeshMaterial3d(dark.clone()), Transform::from_xyz(a.cos() * 0.4, -0.35, a.sin() * 0.4).with_rotation(base), Part { limb: Limb::Tentacle(i), rest: base, k: i as f32 * 0.9 }));
                        }
                    });
                });
            });
        }
    }
    root
}

/// Resource nodes, the fabricator and rifts. Gatherable nodes get a `NodeFull` and a
/// `NodeEmpty` child group; the caller toggles their visibility.
pub fn node(cx: &mut Ctx, kind: &str, at: Vec3, seed: u32) -> Entity {
    let rot = Quat::from_rotation_y(seed as f32 * 2.399);
    let root = cx.commands.spawn((Transform::from_translation(at).with_rotation(rot), Visibility::default())).id();
    let rock = cx.m("n_rock", || lowpoly(Sphere::new(0.5).mesh().ico(1).unwrap()));
    let slab = cx.m("n_slab", || lowpoly(Cuboid::new(1.4, 0.32, 1.0)));
    let shard = cx.m("n_shard", || lowpoly(Cuboid::new(0.25, 0.9, 0.25)));
    let bead = cx.m("n_bead", || Sphere::new(0.09).mesh().ico(0).unwrap());
    let rubble = cx.m("n_rubble", || lowpoly(Sphere::new(0.35).mesh().ico(0).unwrap()).scaled_by(Vec3::new(1.4, 0.25, 1.2)));
    let spent = cx.c([0.30, 0.22, 0.18]);
    let mut full: Vec<(Handle<Mesh>, Handle<StandardMaterial>, Transform)> = Vec::new();
    let mut empty: Vec<(Handle<Mesh>, Handle<StandardMaterial>, Transform)> = vec![(rubble.clone(), spent.clone(), Transform::from_xyz(0.0, 0.05, 0.0))];
    match kind {
        "regolith" => {
            let m = cx.c([0.66, 0.38, 0.22]);
            let m2 = cx.c([0.55, 0.30, 0.18]);
            full.push((rock.clone(), m.clone(), Transform::from_xyz(0.0, 0.1, 0.0).with_scale(Vec3::new(1.6, 0.7, 1.4))));
            full.push((rock.clone(), m2.clone(), Transform::from_xyz(0.45, 0.05, 0.25).with_scale(Vec3::new(0.8, 0.5, 0.8))));
            full.push((rock.clone(), m2, Transform::from_xyz(-0.4, 0.02, -0.3).with_scale(Vec3::new(0.7, 0.4, 0.6))));
        }
        "ice" => {
            let base = cx.c([0.42, 0.30, 0.24]);
            let ice = cx.kit.mat(cx.mats, [0.80, 0.92, 1.0], 0.15, 0.15, 0.1);
            full.push((rock.clone(), base, Transform::from_xyz(0.0, 0.0, 0.0).with_scale(Vec3::new(1.5, 0.5, 1.3))));
            for i in 0..5 {
                let a = i as f32 * 1.3;
                full.push((shard.clone(), ice.clone(), Transform::from_xyz(a.cos() * 0.35, 0.35, a.sin() * 0.3).with_rotation(Quat::from_rotation_z(0.35 * a.sin()) * Quat::from_rotation_x(0.3 * a.cos())).with_scale(Vec3::splat(0.6 + 0.12 * i as f32))));
            }
            empty.push((rock.clone(), spent.clone(), Transform::from_xyz(0.0, 0.0, 0.0).with_scale(Vec3::new(1.3, 0.3, 1.1))));
        }
        "hematite" => {
            let rockm = cx.c([0.30, 0.20, 0.19]);
            let ore = cx.shiny([0.50, 0.30, 0.30]);
            full.push((rock.clone(), rockm.clone(), Transform::from_xyz(0.0, 0.25, 0.0).with_scale(Vec3::new(1.5, 1.1, 1.3))));
            for i in 0..9 {
                let a = i as f32 * 2.1;
                full.push((bead.clone(), ore.clone(), Transform::from_xyz(a.cos() * 0.62, 0.3 + 0.25 * (a * 1.7).sin(), a.sin() * 0.55)));
            }
            empty.push((rock.clone(), rockm, Transform::from_xyz(0.0, 0.15, 0.0).with_scale(Vec3::new(1.3, 0.7, 1.1))));
        }
        "outcrop" => {
            let tans = [[0.74, 0.56, 0.40], [0.62, 0.44, 0.32], [0.80, 0.64, 0.46], [0.56, 0.38, 0.28]];
            for (i, t) in tans.iter().enumerate() {
                let m = cx.c(*t);
                let tr = Transform::from_xyz(0.05 * i as f32, 0.16 + i as f32 * 0.3, -0.04 * i as f32).with_scale(Vec3::new(1.0 - 0.12 * i as f32, 1.0, 1.0 - 0.1 * i as f32));
                if i < 2 {
                    empty.push((slab.clone(), m.clone(), tr));
                }
                full.push((slab.clone(), m, tr));
            }
        }
        "void_vein" => {
            let basalt = cx.c([0.16, 0.14, 0.15]);
            let crystal = cx.glow([0.62, 0.32, 1.0], 6.0);
            let crystal2 = cx.glow([0.30, 0.80, 1.0], 5.0);
            let spike = cx.m("v_spike", || lowpoly(Cone::new(0.11, 0.7)).translated_by(Vec3::new(0.0, 0.35, 0.0)));
            full.push((rock.clone(), basalt.clone(), Transform::from_xyz(0.0, 0.2, 0.0).with_scale(Vec3::new(1.5, 0.9, 1.3))));
            for i in 0..7 {
                let a = i as f32 * 0.9 + seed as f32;
                let tilt = Quat::from_rotation_z(0.5 * a.cos()) * Quat::from_rotation_x(0.5 * a.sin());
                let m = if i % 3 == 0 { crystal2.clone() } else { crystal.clone() };
                full.push((spike.clone(), m, Transform::from_xyz(a.cos() * 0.4, 0.35 + 0.1 * (i % 2) as f32, a.sin() * 0.35).with_rotation(tilt).with_scale(Vec3::splat(0.7 + 0.15 * (i % 3) as f32))));
            }
            empty.push((rock.clone(), basalt, Transform::from_xyz(0.0, 0.15, 0.0).with_scale(Vec3::new(1.4, 0.7, 1.2))));
            cx.commands.entity(root).with_children(|p| {
                p.spawn((PointLight { color: Color::srgb(0.65, 0.4, 1.0), intensity: 60_000.0, range: 7.0, shadow_maps_enabled: false, ..default() }, Transform::from_xyz(0.0, 1.2, 0.0)));
            });
        }
        "fossil" => {
            let stone = cx.c([0.62, 0.56, 0.46]);
            let glyph = cx.glow([1.0, 0.75, 0.30], 3.0);
            let ring = cx.m("fo_ring", || lowpoly(Torus::new(0.05, 0.11).mesh().minor_resolution(4).major_resolution(10)));
            full.push((slab.clone(), stone.clone(), Transform::from_xyz(0.0, 0.16, 0.0).with_scale(Vec3::new(1.0, 1.0, 1.1))));
            full.push((slab.clone(), stone.clone(), Transform::from_xyz(0.1, 0.42, -0.05).with_scale(Vec3::new(0.75, 0.8, 0.8)).with_rotation(Quat::from_rotation_z(0.08))));
            // a spiral of shell rings: the old Martians' glyph
            for i in 0..5 {
                let k = i as f32;
                full.push((ring.clone(), glyph.clone(), Transform::from_xyz(-0.15 + k * 0.08, 0.6, -0.05 + k * 0.02).with_scale(Vec3::splat(0.6 + k * 0.45))));
            }
            empty.push((slab.clone(), stone, Transform::from_xyz(0.0, 0.16, 0.0)));
            cx.commands.entity(root).with_children(|p| {
                p.spawn((PointLight { color: Color::srgb(1.0, 0.75, 0.35), intensity: 25_000.0, range: 5.0, shadow_maps_enabled: false, ..default() }, Transform::from_xyz(0.0, 1.0, 0.0)));
            });
        }
        "locker" => {
            let body = cx.shiny([0.52, 0.55, 0.60]);
            let dark = cx.c([0.20, 0.21, 0.24]);
            let lamp = cx.glow([0.3, 1.0, 0.45], 4.0);
            let cab = cx.m("lk_cab", || lowpoly(Cuboid::new(1.3, 1.9, 0.75)));
            let door = cx.m("lk_door", || lowpoly(Cuboid::new(0.6, 1.7, 0.04)));
            let handle = cx.m("lk_handle", || lowpoly(Cuboid::new(0.05, 0.3, 0.06)));
            let strip = cx.m("lk_strip", || lowpoly(Cuboid::new(1.0, 0.07, 0.03)));
            cx.commands.entity(root).with_children(|p| {
                piece(p, cab, body.clone(), Transform::from_xyz(0.0, 0.95, 0.0));
                for x in [-0.31f32, 0.31] {
                    piece(p, door.clone(), dark.clone(), Transform::from_xyz(x, 0.95, -0.39));
                    piece(p, handle.clone(), body.clone(), Transform::from_xyz(x * 0.4, 0.95, -0.43));
                }
                piece(p, strip, lamp, Transform::from_xyz(0.0, 1.75, -0.42));
            });
            return root;
        }
        "pit" => {
            let hole = cx.kit.mat(cx.mats, [0.01, 0.01, 0.01], 0.0, 1.0, 0.0);
            let rim = cx.c([0.30, 0.20, 0.16]);
            let wood = cx.c([0.45, 0.32, 0.20]);
            let lamp = cx.glow([1.0, 0.65, 0.2], 6.0);
            let disc = cx.m("p_hole", || lowpoly(Cylinder::new(1.15, 0.06).mesh().resolution(14)));
            let post = cx.m("p_post", || lowpoly(Cuboid::new(0.1, 1.9, 0.1)));
            let beam = cx.m("p_beam", || lowpoly(Cuboid::new(1.9, 0.1, 0.1)));
            let rope = cx.m("p_rope", || lowpoly(Cuboid::new(0.04, 1.4, 0.04)));
            cx.commands.entity(root).with_children(|p| {
                piece(p, disc, hole, Transform::from_xyz(0.0, 0.05, 0.0));
                for i in 0..9 {
                    let a = i as f32 * 0.7;
                    piece(p, rock.clone(), rim.clone(), Transform::from_xyz(a.cos() * 1.3, 0.05, a.sin() * 1.3).with_scale(Vec3::new(0.6, 0.35, 0.5)));
                }
                // a winch frame over the hole, a rope going down, a lamp
                for x in [-0.95f32, 0.95] {
                    piece(p, post.clone(), wood.clone(), Transform::from_xyz(x, 0.95, 0.0));
                }
                piece(p, beam, wood.clone(), Transform::from_xyz(0.0, 1.9, 0.0));
                piece(p, rope, wood, Transform::from_xyz(0.0, 1.2, 0.0));
                piece(p, bead.clone(), lamp, Transform::from_xyz(0.95, 2.0, 0.0).with_scale(Vec3::splat(1.6)));
            });
            return root;
        }
        "ladder" => {
            let metal = cx.shiny([0.55, 0.52, 0.48]);
            let beam = cx.glow([1.0, 0.78, 0.45], 1.6);
            let rail = cx.m("ld_rail", || lowpoly(Cuboid::new(0.07, 6.0, 0.07)));
            let rung = cx.m("ld_rung", || lowpoly(Cuboid::new(0.6, 0.05, 0.05)));
            cx.commands.entity(root).with_children(|p| {
                for x in [-0.3f32, 0.3] {
                    piece(p, rail.clone(), metal.clone(), Transform::from_xyz(x, 3.0, 0.0));
                }
                for i in 0..14 {
                    piece(p, rung.clone(), metal.clone(), Transform::from_xyz(0.0, 0.3 + i as f32 * 0.42, 0.0));
                }
                // daylight from the pit above: a warm glow at the top of the ladder and below it
                piece(p, bead.clone(), beam, Transform::from_xyz(0.0, 6.05, 0.0).with_scale(Vec3::splat(2.5)));
                p.spawn((PointLight { color: Color::srgb(1.0, 0.82, 0.6), intensity: 500_000.0, range: 18.0, shadow_maps_enabled: false, ..default() }, Transform::from_xyz(0.0, 4.5, 0.0)));
            });
            return root;
        }
        "fabricator" => {
            let grey = cx.c([0.62, 0.62, 0.64]);
            let dark = cx.c([0.22, 0.22, 0.25]);
            let panel = cx.glow([0.2, 0.9, 1.0], 3.0);
            let lamp = cx.glow([1.0, 0.6, 0.1], 5.0);
            let base = cx.m("f_base", || lowpoly(Cuboid::new(1.7, 0.7, 1.3)));
            let top = cx.m("f_top", || lowpoly(Cuboid::new(1.2, 0.6, 1.0)));
            let tank = cx.m("f_tank", || lowpoly(Cylinder::new(0.3, 1.2)));
            let screen = cx.m("f_screen", || lowpoly(Cuboid::new(0.7, 0.35, 0.04)));
            let mast = cx.m("f_mast", || lowpoly(Cuboid::new(0.05, 1.2, 0.05)));
            cx.commands.entity(root).with_children(|p| {
                piece(p, base, grey.clone(), Transform::from_xyz(0.0, 0.35, 0.0));
                piece(p, top, dark.clone(), Transform::from_xyz(-0.15, 1.0, 0.0));
                piece(p, tank, grey.clone(), Transform::from_xyz(0.62, 0.95, 0.25));
                piece(p, screen, panel, Transform::from_xyz(-0.15, 1.05, -0.52));
                piece(p, mast, dark, Transform::from_xyz(0.62, 2.0, 0.25));
                piece(p, bead.clone(), lamp, Transform::from_xyz(0.62, 2.62, 0.25).with_scale(Vec3::splat(1.3)));
            });
            return root;
        }
        "rift" => {
            let void = cx.kit.mat(cx.mats, [0.02, 0.01, 0.05], 1.0, 0.9, 0.0);
            let cyan = cx.glow([0.25, 0.95, 1.0], 8.0);
            let purple = cx.glow([0.70, 0.30, 1.0], 8.0);
            let star = cx.glow([1.0, 1.0, 1.0], 12.0);
            let neb_a = cx.glow([0.55, 0.15, 0.75], 1.2);
            let neb_b = cx.glow([0.10, 0.30, 0.80], 1.2);
            let scorch = cx.c([0.10, 0.07, 0.09]);
            let slab = cx.m("r_slab", || lowpoly(Cuboid::new(1.0, 1.0, 0.06)));
            let edge = cx.m("r_edge", || lowpoly(Cuboid::new(0.07, 1.0, 0.09)));
            let dot = cx.m("r_star", || lowpoly(Cuboid::new(0.035, 0.035, 0.012)));
            let neb = cx.m("r_neb", || lowpoly(Sphere::new(0.3).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.0, 0.7, 0.04)));
            let spark = cx.m("r_spark", || lowpoly(Cuboid::new(0.08, 0.08, 0.08)));
            let line = cx.m("r_crack", || lowpoly(Cuboid::new(0.12, 0.05, 1.6)));
            // the tear's outline: a jagged lens 1.4 m wide, 3 m tall, its base 0.15 m up
            let (w, h, y0) = (0.7f32, 1.5f32, 1.65f32);
            let pts: Vec<Vec2> = (0..14)
                .map(|i| {
                    let a = i as f32 / 14.0 * std::f32::consts::TAU;
                    let j = 1.0 + 0.16 * ((i * 7 + seed as usize) % 5) as f32 / 4.0 - 0.08;
                    // pointed at the top and bottom like a slit
                    Vec2::new(a.sin() * w * j * a.sin().abs().sqrt(), y0 + a.cos() * h)
                })
                .collect();
            cx.commands.entity(root).with_children(|p| {
                piece(p, rubble.clone(), scorch, Transform::from_xyz(0.0, 0.0, 0.0).with_scale(Vec3::new(3.0, 1.0, 2.2)));
                for i in 0..4 {
                    let a = i as f32 * 1.5 + 0.4;
                    piece(p, line.clone(), if i % 2 == 0 { cyan.clone() } else { purple.clone() }, Transform::from_xyz(a.cos() * 0.6, 0.04, a.sin() * 0.4).with_rotation(Quat::from_rotation_y(a)));
                }
                // the void inside: horizontal strips filling the lens
                for k in 0..10 {
                    let y = y0 - h + (k as f32 + 0.5) * (2.0 * h / 10.0);
                    let t = ((y - y0) / h).clamp(-1.0, 1.0);
                    let half = w * (1.0 - t * t).sqrt().powf(1.2) * 0.98;
                    piece(p, slab.clone(), void.clone(), Transform::from_xyz(0.0, y, 0.0).with_scale(Vec3::new(2.0 * half.max(0.04), 2.0 * h / 10.0 + 0.01, 1.0)));
                }
                // stars and nebula on both faces
                for side in [-1.0f32, 1.0] {
                    for i in 0..16u32 {
                        let u = ((i * 37 + seed * 11) % 97) as f32 / 97.0 * 2.0 - 1.0;
                        let v = ((i * 53 + seed * 7) % 89) as f32 / 89.0 * 2.0 - 1.0;
                        let y = y0 + v * h * 0.85;
                        let t = (y - y0) / h;
                        let x = u * w * (1.0 - t * t).max(0.0).sqrt() * 0.8;
                        piece(p, dot.clone(), star.clone(), Transform::from_xyz(x, y, 0.035 * side).with_scale(Vec3::splat(0.6 + (i % 3) as f32 * 0.4)));
                    }
                    piece(p, neb.clone(), neb_a.clone(), Transform::from_xyz(-0.12, y0 + 0.35, 0.034 * side).with_rotation(Quat::from_rotation_z(0.6)));
                    piece(p, neb.clone(), neb_b.clone(), Transform::from_xyz(0.1, y0 - 0.45, 0.033 * side).with_rotation(Quat::from_rotation_z(-0.4)).with_scale(Vec3::new(0.8, 1.3, 1.0)));
                }
                // glowing edges, alternating cyan / purple
                for i in 0..pts.len() {
                    let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                    let mid = (a + b) * 0.5;
                    let d = b - a;
                    let ang = (-d.x).atan2(d.y);
                    let mat = if i % 2 == 0 { cyan.clone() } else { purple.clone() };
                    piece(p, edge.clone(), mat, Transform::from_xyz(mid.x, mid.y, 0.0).with_rotation(Quat::from_rotation_z(ang)).with_scale(Vec3::new(1.0, d.length() + 0.05, 1.0)));
                }
                for i in 0..12 {
                    let mat = if i % 2 == 0 { cyan.clone() } else { purple.clone() };
                    p.spawn((Mesh3d(spark.clone()), MeshMaterial3d(mat), Transform::from_xyz(0.0, 0.0, 0.0), RiftSpark(i as f32 * 0.31)));
                }
            });
            return root;
        }
        _ => {
            let m = cx.c([0.5, 0.5, 0.5]);
            full.push((rock.clone(), m, Transform::IDENTITY));
        }
    }
    cx.commands.entity(root).with_children(|p| {
        p.spawn((Transform::IDENTITY, Visibility::default(), NodeFull)).with_children(|g| {
            for (m, mat, t) in full {
                piece(g, m, mat, t);
            }
        });
        p.spawn((Transform::IDENTITY, Visibility::Hidden, NodeEmpty)).with_children(|g| {
            for (m, mat, t) in empty {
                piece(g, m, mat, t);
            }
        });
    });
    root
}

/// The lava tube's rock, floor and glowing lichen, under one root.
pub fn cave(cx: &mut Ctx, map: &super::cave::CaveMap) -> Entity {
    let m = super::cave::build_meshes(map);
    // dusty pale floor, darker basalt walls (lit faces against dim tops), glowing lichen
    let mats = [cx.kit.mat(cx.mats, [0.55, 0.48, 0.42], 0.0, 0.95, 0.0), cx.kit.mat(cx.mats, [0.40, 0.35, 0.33], 0.0, 0.85, 0.0), cx.glow([0.25, 0.95, 0.75], 4.0)];
    let meshes = [m.floor, m.rock, m.lichen].map(|x| cx.meshes.add(x));
    cx.commands
        .spawn((Transform::default(), Visibility::default(), super::cave::CaveGeo))
        .with_children(|p| {
            for (mesh, mat) in meshes.into_iter().zip(mats) {
                piece(p, mesh, mat, Transform::IDENTITY);
            }
        })
        .id()
}

/// Armour worn on an astronaut (respawned when the gear changes).
#[derive(Component)]
pub struct ArmourPiece;

/// Put `gear` on the astronaut `root` (its left arm `arm_l` carries the shield).
pub fn dress(cx: &mut Ctx, root: Entity, arm_l: Option<Entity>, gear: &super::proto::Gear) {
    let tier = |item: &str| -> ([f32; 3], f32) {
        if item.starts_with("chitin") {
            ([0.40, 0.58, 0.48], 0.0)
        } else if item.starts_with("iron") {
            ([0.56, 0.58, 0.63], 0.0)
        } else {
            ([0.92, 0.70, 0.26], 1.0)
        }
    };
    let mut parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>, Transform)> = Vec::new();
    if let Some(h) = &gear.helmet {
        let (c, relic) = tier(h);
        let mat = if relic > 0.0 { cx.kit.mat(cx.mats, c, 0.0, 0.3, 0.7) } else { cx.shiny(c) };
        let shell = cx.m("ar_helm", || lowpoly(Sphere::new(0.27).mesh().ico(1).unwrap()).scaled_by(Vec3::new(1.0, 0.75, 1.0)));
        let crest = cx.m("ar_crest", || lowpoly(Cuboid::new(0.06, 0.12, 0.42)));
        parts.push((shell, mat.clone(), Transform::from_xyz(0.0, 1.8, 0.02)));
        parts.push((crest, mat, Transform::from_xyz(0.0, 1.98, 0.02)));
        if relic > 0.0 {
            let band = cx.m("ar_band", || lowpoly(Torus::new(0.25, 0.28).mesh().minor_resolution(4).major_resolution(12)));
            let glow = cx.glow([1.0, 0.8, 0.35], 5.0);
            parts.push((band, glow, Transform::from_xyz(0.0, 1.74, 0.0)));
        }
    }
    if let Some(s) = &gear.suit {
        let (c, relic) = tier(s);
        let mat = if relic > 0.0 { cx.kit.mat(cx.mats, c, 0.0, 0.3, 0.7) } else { cx.shiny(c) };
        let plate = cx.m("ar_plate", || lowpoly(Cuboid::new(0.56, 0.46, 0.4)));
        let pad = cx.m("ar_pad", || lowpoly(Sphere::new(0.15).mesh().ico(0).unwrap()).scaled_by(Vec3::new(1.2, 0.7, 1.0)));
        let belt = cx.m("ar_belt", || lowpoly(Cuboid::new(0.55, 0.08, 0.38)));
        parts.push((plate, mat.clone(), Transform::from_xyz(0.0, 1.24, -0.01)));
        parts.push((pad.clone(), mat.clone(), Transform::from_xyz(-0.36, 1.5, 0.0)));
        parts.push((pad, mat.clone(), Transform::from_xyz(0.36, 1.5, 0.0)));
        parts.push((belt, mat, Transform::from_xyz(0.0, 0.9, 0.0)));
        if relic > 0.0 {
            let glyph = cx.m("ar_glyph", || lowpoly(Cuboid::new(0.05, 0.3, 0.02)));
            let glow = cx.glow([1.0, 0.8, 0.35], 5.0);
            for x in [-0.12f32, 0.0, 0.12] {
                parts.push((glyph.clone(), glow.clone(), Transform::from_xyz(x, 1.25, -0.215)));
            }
        }
    }
    if gear.cape.is_some() {
        let cape = cx.m("ar_cape", || lowpoly(Cuboid::new(0.6, 1.05, 0.03)));
        let star = cx.m("ar_star", || lowpoly(Cuboid::new(0.04, 0.04, 0.01)));
        let cloth = cx.c([0.28, 0.10, 0.45]);
        let glow = cx.glow([0.8, 0.7, 1.0], 6.0);
        parts.push((cape, cloth, Transform::from_xyz(0.0, 1.0, 0.42).with_rotation(Quat::from_rotation_x(-0.12))));
        for i in 0..6 {
            let (x, y) = (((i * 37) % 5) as f32 * 0.1 - 0.2, 0.6 + ((i * 53) % 7) as f32 * 0.12);
            parts.push((star.clone(), glow.clone(), Transform::from_xyz(x, y, 0.445).with_rotation(Quat::from_rotation_x(-0.12))));
        }
    }
    cx.commands.entity(root).with_children(|p| {
        for (m, mat, t) in parts {
            p.spawn((Mesh3d(m), MeshMaterial3d(mat), t, ArmourPiece));
        }
    });
    if let (Some(_), Some(arm)) = (&gear.shield, arm_l) {
        let disc = cx.m("ar_shield", || lowpoly(Cylinder::new(0.3, 0.05).mesh().resolution(6)).rotated_by(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)));
        let rim = cx.m("ar_srim", || lowpoly(Torus::new(0.27, 0.32).mesh().minor_resolution(4).major_resolution(6)).rotated_by(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)));
        let core = cx.glow([0.35, 0.85, 1.0], 4.0);
        let frame = cx.shiny([0.5, 0.52, 0.58]);
        cx.commands.entity(arm).with_children(|p| {
            p.spawn((Mesh3d(disc), MeshMaterial3d(core), Transform::from_xyz(-0.13, -0.4, 0.0), ArmourPiece));
            p.spawn((Mesh3d(rim), MeshMaterial3d(frame), Transform::from_xyz(-0.14, -0.4, 0.0), ArmourPiece));
        });
    }
}

/// The yellow (walk) / red (action) click cross.
#[derive(Component)]
pub struct Marker(pub f32);

pub fn marker(cx: &mut Ctx, at: Vec3, red: bool) -> Entity {
    let mat = if red { cx.glow([1.0, 0.15, 0.1], 3.0) } else { cx.glow([1.0, 0.9, 0.1], 3.0) };
    let bar = cx.m("mk_bar", || lowpoly(Cuboid::new(0.14, 0.05, 1.0)));
    cx.commands
        .spawn((Transform::from_translation(at + Vec3::Y * 0.08), Visibility::default(), Marker(0.0)))
        .with_children(|p| {
            for a in [0.785f32, -0.785] {
                piece(p, bar.clone(), mat.clone(), Transform::from_rotation(Quat::from_rotation_y(a)));
            }
        })
        .id()
}

/// Ring on the ground under whatever a left click would act on (one entity, moved every frame).
#[derive(Component)]
pub struct HoverRing {
    /// yellow (things), red (creatures), white (astronauts)
    pub mats: [Handle<StandardMaterial>; 3],
}

pub fn hover_ring(cx: &mut Ctx) -> Entity {
    let mesh = cx.m("hover_ring", || lowpoly(Torus::new(0.72, 1.0).mesh().minor_resolution(6).major_resolution(32)));
    let mats = [cx.glow([1.0, 0.85, 0.15], 2.5), cx.glow([1.0, 0.18, 0.1], 2.5), cx.glow([0.9, 0.95, 1.0], 2.0)];
    cx.commands.spawn((Mesh3d(mesh), MeshMaterial3d(mats[0].clone()), Transform::default(), Visibility::Hidden, HoverRing { mats }, Fx)).id()
}

/// Bullet tracer or plasma bolt flying from `a` to `b`.
#[derive(Component)]
pub struct Shot {
    pub a: Vec3,
    pub b: Vec3,
    pub t: f32,
    pub dur: f32,
}

pub fn shot(cx: &mut Ctx, a: Vec3, b: Vec3, plasma: bool) -> Entity {
    let (mesh, mat, dur) = if plasma {
        (cx.m("s_bolt", || Sphere::new(0.16).mesh().ico(1).unwrap()), cx.glow([0.3, 1.0, 0.8], 10.0), 0.35)
    } else {
        (cx.m("s_tracer", || Cuboid::new(0.04, 0.04, 0.9).into()), cx.glow([1.0, 0.85, 0.3], 10.0), 0.14)
    };
    cx.commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(a).looking_at(b, Vec3::Y), Shot { a, b, t: 0.0, dur }, Fx)).id()
}

/// Short-lived muzzle flash.
#[derive(Component)]
pub struct Flash(pub f32);

pub fn flash(cx: &mut Ctx, at: Vec3) -> Entity {
    let m = cx.m("s_flash", || lowpoly(Sphere::new(0.12).mesh().ico(0).unwrap()));
    let mat = cx.glow([1.0, 0.8, 0.4], 12.0);
    cx.commands.spawn((Mesh3d(m), MeshMaterial3d(mat), Transform::from_translation(at), Flash(0.0), Fx)).id()
}

/// Pose one rig's parts for its animation at `phase`.
pub fn pose(limb: Limb, anim: u8, phase: f32, recoil: f32) -> Quat {
    let s = phase.sin();
    match limb {
        // tripod gait: legs 0, 3, 4 swing together, 1, 2, 5 opposite
        Limb::Crawl(i) => {
            let sign = if matches!(i, 0 | 3 | 4) { 1.0 } else { -1.0 };
            let (amp, lift) = match anim {
                anim::WALK | anim::RUN => (0.45, 0.25),
                anim::ATTACK => (0.15, 0.3 * recoil),
                _ => (0.05, 0.0),
            };
            let ph = phase * 1.6;
            return Quat::from_rotation_y(sign * amp * ph.sin()) * Quat::from_rotation_z(sign * lift * ph.cos().max(0.0));
        }
        Limb::Tentacle(_) => {
            let amp = match anim {
                anim::ATTACK => 0.35 + 0.5 * recoil,
                anim::WALK | anim::RUN => 0.3,
                _ => 0.18,
            };
            return Quat::from_rotation_x(amp * (phase * 0.9).sin()) * Quat::from_rotation_z(amp * 0.6 * (phase * 0.7 + 1.3).cos());
        }
        _ => {}
    }
    match anim {
        anim::WALK | anim::RUN => {
            let amp = if anim == anim::RUN { 0.85 } else { 0.55 };
            match limb {
                Limb::LegL | Limb::Fl | Limb::Br => Quat::from_rotation_x(s * amp),
                Limb::LegR | Limb::Fr | Limb::Bl => Quat::from_rotation_x(-s * amp),
                Limb::ArmL => Quat::from_rotation_x(-s * amp * 0.8),
                Limb::ArmR => Quat::from_rotation_x(s * amp * 0.8),
                Limb::Tail => Quat::from_rotation_x(0.5) * Quat::from_rotation_y(s * 0.4),
                _ => Quat::IDENTITY,
            }
        }
        anim::MINE => match limb {
            // overhead swing, a strike every gather cycle
            Limb::ArmR | Limb::ArmL => Quat::from_rotation_x(1.2 + 1.1 * (phase * 0.5).sin().max(-0.4)),
            _ => Quat::IDENTITY,
        },
        anim::SURVEY => match limb {
            Limb::ArmR => Quat::from_rotation_x(1.1 + 0.15 * s),
            Limb::ArmL => Quat::from_rotation_x(0.6 + 0.2 * (phase * 0.7).sin()),
            Limb::LegL => Quat::from_rotation_x(0.5),
            Limb::LegR => Quat::from_rotation_x(-0.2),
            _ => Quat::IDENTITY,
        },
        anim::CRAFT => match limb {
            Limb::ArmR => Quat::from_rotation_x(0.9 + 0.3 * s),
            Limb::ArmL => Quat::from_rotation_x(0.9 - 0.3 * s),
            _ => Quat::IDENTITY,
        },
        anim::ATTACK => match limb {
            // aim: right arm straight out (guns), demons lunge with both claws
            Limb::ArmR => Quat::from_rotation_x(1.5 + recoil * 0.5),
            Limb::ArmL => Quat::from_rotation_x(0.9 + 0.3 * recoil),
            Limb::Fl | Limb::Fr => Quat::from_rotation_x(-0.6 * recoil),
            Limb::Head => Quat::from_rotation_x(0.4 * recoil),
            Limb::Tail => Quat::from_rotation_x(0.9),
            _ => Quat::IDENTITY,
        },
        _ => match limb {
            Limb::ArmL => Quat::from_rotation_z(-0.06 - 0.02 * (phase * 0.3).sin()),
            Limb::ArmR => Quat::from_rotation_z(0.06 + 0.02 * (phase * 0.3).sin()),
            Limb::Tail => Quat::from_rotation_x(0.5) * Quat::from_rotation_y(0.2 * (phase * 0.5).sin()),
            _ => Quat::IDENTITY,
        },
    }
}
