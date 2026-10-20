//! Look and feel, kept apart from the physics: Mars sky dome and dust-coloured fog, camera
//! (HDR, tonemapping, bloom, speed FOV, look-ahead, landing shake), terrain shading (a Mars
//! palette for the greyscale ortho plus close-up sand detail), the procedural sand-rail model,
//! dust plumes, tire tracks and the boost flame.
//!
//! `--low` (`?low` in the browser) turns off the expensive parts: HDR/bloom, MSAA (FXAA instead),
//! close-up terrain detail, most particles, and uses smaller shadow maps.
//!
//! Everything here only *reads* the physics state (`Buggy`, `Site`); nothing feeds back into it.

use crate::buggy::{Buggy, Chassis, VIS_COM, VIS_WHEEL_R, WHEELS, Wheel};
use crate::terrain::Site;
use bevy::anti_alias::fxaa::Fxaa;
use bevy::asset::{RenderAssetUsages, embedded_asset};
use bevy::camera::Hdr;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::light::{CascadeShadowConfigBuilder, DirectionalLightShadowMap, NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{DistanceFog, ExtendedMaterial, FogFalloff, MaterialExtension, MaterialPlugin};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat};
use bevy::render::view::{ColorGrading, Msaa};
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;
use std::collections::{HashMap, VecDeque};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

const TERRAIN_SHADER: &str = "embedded://marsmaker_viewer/shaders/terrain.wesl";
const SKY_SHADER: &str = "embedded://marsmaker_viewer/shaders/sky.wesl";
const SKY_RADIUS: f32 = 45_000.0;
const BASE_FOV_DEG: f32 = 50.0;

/// Render quality, from `--low` / `?low`.
#[derive(Resource, Clone, Copy)]
pub struct Quality {
    pub low: bool,
}

pub struct VisualsPlugin {
    pub low: bool,
}

impl Plugin for VisualsPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/terrain.wesl");
        embedded_asset!(app, "shaders/sky.wesl");
        let low = self.low;
        app.add_plugins((MaterialPlugin::<TerrainMaterial>::default(), MaterialPlugin::<SkyMaterial>::default()))
            .insert_resource(Quality { low })
            .insert_resource(ClearColor(Color::srgb(0.80, 0.62, 0.47)))
            .insert_resource(GlobalAmbientLight { color: Color::srgb(1.0, 0.80, 0.62), brightness: 900.0, ..default() })
            .insert_resource(DirectionalLightShadowMap { size: if low { 1024 } else if cfg!(target_arch = "wasm32") { 2048 } else { 4096 } })
            .init_resource::<TerrainSwap>()
            .init_resource::<CamFx>()
            .add_systems(Startup, setup_fx)
            .add_systems(
                PostUpdate,
                (swap_terrain_materials, atmosphere, camera_fx, sky_follow, suspension, whip, flame, dust, tracks)
                    .chain()
                    .before(TransformSystems::Propagate),
            );
    }
}

// ---------------------------------------------------------------------------------------------
// camera, sun, sky, fog

/// Spawn the camera (called from main's setup) with the quality-dependent extras.
pub fn spawn_camera(commands: &mut Commands, low: bool, transform: Transform) {
    let fog = DistanceFog {
        color: Color::srgb(0.80, 0.62, 0.47),
        directional_light_color: Color::srgba(1.0, 0.88, 0.75, 0.22),
        directional_light_exponent: 10.0,
        // exponential-squared: nothing between the camera and the car, a soft aerial
        // perspective from ~1-2 km out, fully the horizon/sky colour by ~11 km
        falloff: FogFalloff::from_visibility_squared(11_000.0),
    };
    let proj = Projection::Perspective(PerspectiveProjection { fov: BASE_FOV_DEG.to_radians(), far: 60_000.0, ..default() });
    // a touch more contrast and saturation than the neutral TonyMcMapface look
    let mut grading = ColorGrading::default();
    grading.global.exposure = 0.12;
    grading.global.post_saturation = 1.0;
    grading.midtones.contrast = 1.10;
    grading.shadows.contrast = 1.05;
    let mut e = commands.spawn((Camera3d::default(), transform, proj, fog, Tonemapping::TonyMcMapface, grading));
    if low {
        e.insert((Msaa::Off, Fxaa::default()));
    } else {
        e.insert((Msaa::Sample4, Hdr, Bloom { intensity: 0.07, ..Bloom::NATURAL }, bevy::light::ShadowFilteringMethod::Gaussian));
    }
}

/// The sun (called from main's setup). Direction is set by main from the image metadata.
pub fn sun_bundle(low: bool) -> impl Bundle {
    (
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: true, color: Color::srgb(1.0, 0.94, 0.86), ..default() },
        Transform::from_xyz(-1.0, 0.75, -0.6).looking_at(Vec3::ZERO, Vec3::Y),
        if low {
            CascadeShadowConfigBuilder { num_cascades: 2, maximum_distance: 250.0, first_cascade_far_bound: 30.0, ..default() }.build()
        } else {
            CascadeShadowConfigBuilder { num_cascades: 4, maximum_distance: 500.0, first_cascade_far_bound: 18.0, ..default() }.build()
        },
    )
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct SkyExt {
    #[uniform(100)]
    params: SkyParams,
}

#[derive(ShaderType, Reflect, Debug, Clone, Default)]
struct SkyParams {
    zenith: Vec4,
    horizon: Vec4,
    aureole: Vec4,
    fog_sun: Vec4,
    sun: Vec4,
}

impl MaterialExtension for SkyExt {
    fn fragment_shader() -> ShaderRef {
        SKY_SHADER.into()
    }
}

type SkyMaterial = ExtendedMaterial<StandardMaterial, SkyExt>;

#[derive(Component)]
struct SkyDome;

fn lin(c: Color) -> Vec3 {
    let l = c.to_linear();
    Vec3::new(l.red, l.green, l.blue)
}

fn mix3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Sky, fog, sun colour and ambient from the sun elevation, whenever the sun moves.
#[allow(clippy::type_complexity)]
fn atmosphere(
    sun: Query<&Transform, (With<DirectionalLight>, Without<Camera3d>)>,
    mut light: Query<&mut DirectionalLight>,
    mut fog: Query<&mut DistanceFog>,
    dome: Query<&MeshMaterial3d<SkyMaterial>, With<SkyDome>>,
    mut sky_mats: ResMut<Assets<SkyMaterial>>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut clear: ResMut<ClearColor>,
    mut done_for: Local<Option<Vec3>>,
) {
    let Ok(t) = sun.single() else { return };
    let to_sun = t.back().as_vec3();
    if done_for.is_some_and(|d| d.distance(to_sun) < 1e-4) {
        return;
    }
    let Ok(dome) = dome.single() else { return };
    let Some(mut mat) = sky_mats.get_mut(&dome.0) else { return };
    *done_for = Some(to_sun);
    let el = to_sun.y.clamp(-1.0, 1.0).asin().to_degrees();
    let day = smooth(4.0, 40.0, el);

    let horizon = mix3(lin(Color::srgb(0.72, 0.52, 0.42)), lin(Color::srgb(0.93, 0.71, 0.51)), day);
    let zenith = mix3(lin(Color::srgb(0.32, 0.24, 0.22)), lin(Color::srgb(0.62, 0.41, 0.26)), day);
    let aureole = lin(Color::srgb(0.55, 0.68, 0.92));
    let fog_dir = Vec3::new(1.0, 0.88, 0.75) * 0.22;
    mat.extension.params = SkyParams {
        zenith: zenith.extend(2.6),
        horizon: horizon.extend(0.12),
        aureole: aureole.extend(mix3(Vec3::splat(5.0), Vec3::splat(9.0), day).x),
        fog_sun: fog_dir.extend(10.0),
        sun: Vec4::new((0.175f32).to_radians().cos(), 60.0, 1.2, 0.30 + 0.55 * (1.0 - day)),
    };
    let h = LinearRgba::rgb(horizon.x, horizon.y, horizon.z);
    for mut f in &mut fog {
        f.color = h.into();
        f.directional_light_color = Color::linear_rgba(1.0, 0.88, 0.75, 0.22);
        f.directional_light_exponent = 10.0;
    }
    clear.0 = h.into();
    for mut l in &mut light {
        // reddened by the dusty air at low sun
        l.color = Color::srgb(1.0, 0.82 + 0.12 * day, 0.66 + 0.20 * day);
    }
    ambient.color = Color::srgb(1.0, 0.76 + 0.04 * day, 0.58 + 0.04 * day);
    ambient.brightness = 650.0 + 350.0 * day;
}

fn sky_follow(cam: Query<&Transform, (With<Camera3d>, Without<SkyDome>)>, mut dome: Query<&mut Transform, With<SkyDome>>) {
    let (Ok(c), Ok(mut d)) = (cam.single(), dome.single_mut()) else { return };
    d.translation = c.translation;
}

/// Smoothed camera state for the look-ahead / FOV / shake.
#[derive(Resource, Default)]
struct CamFx {
    look: Vec3,
    fov: f32,
    trauma: f32,
    t: f32,
    was_air: bool,
    air_vy: f32,
}

fn camera_fx(
    buggy: Option<Res<Buggy>>,
    time: Res<Time>,
    mut fx: ResMut<CamFx>,
    mut cam: Query<(&mut Transform, &mut Projection), With<Camera3d>>,
) {
    let (Some(b), Ok((mut t, mut proj))) = (buggy, cam.single_mut()) else { return };
    let dt = time.delta_secs().min(0.1);
    fx.t += dt;
    let speed = b.vel.length();
    // high camera (or zoomed far out): almost no look-ahead or shake
    let dist = t.translation.distance(b.pos);
    let near = 1.0 - smooth(15.0, 40.0, dist);

    // landing shake: trauma from the vertical speed at touchdown
    let air = b.airborne();
    if air {
        fx.air_vy = fx.air_vy.min(b.vel.y);
    }
    if fx.was_air && !air {
        let hit = (-fx.air_vy - 3.0).max(0.0);
        fx.trauma = (fx.trauma + hit * 0.12).min(1.0);
    }
    if !air {
        fx.air_vy = 0.0;
    }
    fx.was_air = air;
    if b.boosting {
        fx.trauma = fx.trauma.max(0.18);
    }
    fx.trauma = (fx.trauma - dt * 1.4).max(0.0);

    // look-ahead along the horizontal velocity, smoothed
    let hv = Vec3::new(b.vel.x, 0.0, b.vel.z);
    let want = (hv * 0.10).clamp_length_max(3.5) * near;
    let a = 1.0 - (-dt * 3.0).exp();
    fx.look = fx.look.lerp(want, a);
    let target = b.pos + Vec3::Y * 1.0 + fx.look;
    if t.translation.distance(target) > 0.1 {
        t.look_at(target, Vec3::Y);
    }
    // debug: MM_ORBIT=deg puts the camera on a 6 m orbit around the buggy (for screenshots)
    if let Some(deg) = std::env::var("MM_ORBIT").ok().and_then(|v| v.parse::<f32>().ok()) {
        let yaw = b.yaw() + deg.to_radians();
        let c = b.pos + Vec3::new(-yaw.sin(), 0.0, -yaw.cos()) * -6.0 + Vec3::Y * 1.4;
        *t = Transform::from_translation(c).looking_at(b.pos + Vec3::Y * 0.3, Vec3::Y);
    }
    let s = fx.trauma * fx.trauma * near;
    if s > 0.0 {
        let n = |f: f32, o: f32| (fx.t * f + o).sin() * 0.6 + (fx.t * f * 2.3 + o * 1.7).sin() * 0.4;
        t.rotate_local(Quat::from_euler(EulerRot::YXZ, n(23.0, 0.0) * 0.012 * s, n(29.0, 3.0) * 0.016 * s, n(17.0, 7.0) * 0.01 * s));
    }

    // speed FOV: +14 deg towards boost speed
    let want_fov = BASE_FOV_DEG + 14.0 * smooth(8.0, 45.0, speed) * (0.4 + 0.6 * near);
    if fx.fov == 0.0 {
        fx.fov = BASE_FOV_DEG;
    }
    fx.fov += (want_fov - fx.fov) * (1.0 - (-dt * 2.0).exp());
    if let Projection::Perspective(p) = proj.as_mut() {
        p.fov = fx.fov.to_radians();
    }
}

// ---------------------------------------------------------------------------------------------
// terrain material

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct TerrainExt {
    #[uniform(100)]
    params: TerrainParams,
}

#[derive(ShaderType, Reflect, Debug, Clone, Default)]
struct TerrainParams {
    detail: Vec4,
    c0: Vec4,
    c1: Vec4,
    c2: Vec4,
    c3: Vec4,
    grade: Vec4,
}

impl MaterialExtension for TerrainExt {
    fn fragment_shader() -> ShaderRef {
        TERRAIN_SHADER.into()
    }
}

type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExt>;

fn terrain_params(low: bool) -> TerrainParams {
    TerrainParams {
        // ripple strength, detail fade distance (m), grain strength
        detail: if low { Vec4::new(0.0, 30.0, 0.0, 0.0) } else { Vec4::new(0.45, 38.0, 0.10, 0.0) },
        // Mars palette over the ortho brightness (sRGB, w = stop): dark basaltic sand,
        // brown sand, ochre, bright butterscotch dust
        c0: Vec4::new(0.07, 0.062, 0.062, 0.0),
        c1: Vec4::new(0.34, 0.255, 0.205, 0.33),
        c2: Vec4::new(0.66, 0.46, 0.32, 0.62),
        c3: Vec4::new(0.98, 0.78, 0.58, 1.0),
        // brightness gain, saturation, ripple wind direction (rad)
        grade: Vec4::new(1.0, 1.0, 0.6, 0.0),
    }
}

/// StandardMaterial handle of a terrain tile -> its TerrainMaterial.
#[derive(Resource, Default)]
struct TerrainSwap(HashMap<AssetId<StandardMaterial>, Handle<TerrainMaterial>>);

/// Marks entities this module spawns at the top level (so they're never mistaken for terrain).
#[derive(Component)]
pub struct Fx;

/// terrain.rs spawns plain StandardMaterial chunks (top level, no parent); give them the Mars
/// terrain shader instead. Runs on every newly spawned chunk (coarse and the full-res window).
#[allow(clippy::type_complexity)]
fn swap_terrain_materials(
    mut commands: Commands,
    q: Query<(Entity, &MeshMaterial3d<StandardMaterial>), (Added<MeshMaterial3d<StandardMaterial>>, Without<ChildOf>, Without<Fx>)>,
    std_mats: Res<Assets<StandardMaterial>>,
    mut mats: ResMut<Assets<TerrainMaterial>>,
    mut swap: ResMut<TerrainSwap>,
    quality: Res<Quality>,
) {
    for (e, m) in &q {
        let h = match swap.0.get(&m.0.id()) {
            Some(h) => h.clone(),
            None => {
                let Some(base) = std_mats.get(&m.0) else { continue };
                let mut base = base.clone();
                // the tile's tint is in base_color (red channel = the plain grey multiplier,
                // since the old tint had red = 1); the palette replaces the tint
                let r = base.base_color.to_linear().red;
                base.base_color = Color::linear_rgb(r, r, r);
                let h = mats.add(TerrainMaterial { base, extension: TerrainExt { params: terrain_params(quality.low) } });
                swap.0.insert(m.0.id(), h.clone());
                h
            }
        };
        commands.entity(e).remove::<MeshMaterial3d<StandardMaterial>>().insert(MeshMaterial3d(h));
    }
}

// ---------------------------------------------------------------------------------------------
// startup: sky dome, particle pool, track meshes, camera extras

fn setup_fx(
    mut commands: Commands,
    quality: Res<Quality>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut sky_mats: ResMut<Assets<SkyMaterial>>,
) {
    let low = quality.low;

    // sky dome: inside-out icosphere, unlit, no fog (the shader does its own horizon haze)
    let mut sphere = Sphere::new(SKY_RADIUS).mesh().ico(5).unwrap();
    if let Some(Indices::U32(idx)) = sphere.indices_mut() {
        for t in idx.chunks_mut(3) {
            t.swap(1, 2);
        }
    } else if let Some(Indices::U16(idx)) = sphere.indices_mut() {
        for t in idx.chunks_mut(3) {
            t.swap(1, 2);
        }
    }
    commands.spawn((
        SkyDome,
        Fx,
        Mesh3d(meshes.add(sphere)),
        MeshMaterial3d(sky_mats.add(SkyMaterial {
            base: StandardMaterial { unlit: true, fog_enabled: false, ..default() },
            extension: SkyExt::default(),
        })),
        Transform::default(),
        NotShadowCaster,
        NotShadowReceiver,
        bevy::camera::visibility::NoFrustumCulling,
    ));

    // dust particle pool
    let soft = images.add(soft_blob_image());
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    let buckets: Vec<Handle<StandardMaterial>> = (0..DUST_BUCKETS)
        .map(|k| {
            let a = (k as f32 + 0.5) / DUST_BUCKETS as f32;
            std_mats.add(StandardMaterial {
                base_color: Color::srgba(0.80, 0.63, 0.48, a * 0.6),
                base_color_texture: Some(soft.clone()),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                cull_mode: None,
                ..default()
            })
        })
        .collect();
    let n = if low { 150 } else { 600 };
    let mut parts = Vec::with_capacity(n);
    for _ in 0..n {
        let e = commands
            .spawn((Fx, Mesh3d(quad.clone()), MeshMaterial3d(buckets[0].clone()), Transform::default(), Visibility::Hidden, NotShadowCaster, NotShadowReceiver))
            .id();
        parts.push(Particle { e, pos: Vec3::ZERO, vel: Vec3::ZERO, age: 0.0, life: 0.0, s0: 0.0, s1: 0.0, spin: 0.0, g: 0.0, bucket: 0, alive: false });
    }
    commands.insert_resource(Dust { parts, buckets, next: 0, carry: [0.0; 4], since_on: [1.0; 4], rng: 0x1234_5678, was_air: false, min_vy: 0.0 });

    // tire tracks: one dynamic ribbon mesh per wheel
    let track_mat = std_mats.add(StandardMaterial {
        // multiply: a track always darkens whatever sand it's on (bright dust or dark dunes)
        base_color: Color::srgba(0.62, 0.52, 0.46, 0.5),
        base_color_texture: Some(images.add(tread_image())),
        alpha_mode: AlphaMode::Multiply,
        unlit: true,
        perceptual_roughness: 1.0,
        depth_bias: 50.0,
        ..default()
    });
    let mut list = Vec::new();
    for _ in 0..4 {
        let h = meshes.add(empty_mesh());
        commands.spawn((Fx, Mesh3d(h.clone()), MeshMaterial3d(track_mat.clone()), Transform::default(), NotShadowCaster, bevy::camera::visibility::NoFrustumCulling));
        list.push(Track { mesh: h, pts: VecDeque::new(), dirty: false, broken: true, off_t: 0.0 });
    }
    commands.insert_resource(Tracks { list, max_pts: if low { 250 } else { 700 }, life: if low { 18.0 } else { 40.0 } });
}

fn empty_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new())
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new())
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new())
        .with_inserted_indices(Indices::U32(Vec::new()))
}

/// Dust puff sprite: a cluster of soft lobes with fine grainy breakup, lit from above (brighter
/// top, shaded underside), so a puff reads as a defined little cloud rather than a smudge.
fn soft_blob_image() -> Image {
    let n = 128usize;
    let hash = |x: i32, y: i32| -> f32 {
        let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263);
        h = (h ^ (h >> 13)).wrapping_mul(1274126177);
        (h ^ (h >> 16)) as f32 / u32::MAX as f32
    };
    let vnoise = |x: f32, y: f32| -> f32 {
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - ix as f32, y - iy as f32);
        let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
        let a = hash(ix, iy) + (hash(ix + 1, iy) - hash(ix, iy)) * ux;
        let b = hash(ix, iy + 1) + (hash(ix + 1, iy + 1) - hash(ix, iy + 1)) * ux;
        a + (b - a) * uy
    };
    // lobes: (centre u, centre v, radius) in -1..1 sprite space (v up)
    let lobes = [(0.0, -0.05, 0.62), (-0.32, -0.18, 0.42), (0.34, -0.15, 0.45), (-0.12, 0.30, 0.40), (0.22, 0.24, 0.36)];
    let mut data = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let v = 1.0 - (y as f32 + 0.5) / n as f32 * 2.0; // image rows go down; v up
            let mut d = 0.0f32;
            for &(cu, cv, r) in &lobes {
                let q = ((u - cu).powi(2) + (v - cv).powi(2)).sqrt() / r;
                d = d.max((1.0 - q * q).max(0.0));
            }
            let grain = 0.55 * vnoise(u * 5.0 + 3.1, v * 5.0) + 0.3 * vnoise(u * 11.0, v * 11.0 + 7.7) + 0.15 * vnoise(u * 23.0 + 1.3, v * 23.0);
            let a = (d * (0.75 + 0.4 * grain) - 0.08).clamp(0.0, 1.0).powf(1.4);
            // fade to zero at the sprite border whatever the lobes do
            let edge = (1.0 - (u * u + v * v).sqrt()).clamp(0.0, 0.25) * 4.0;
            let a = a * edge;
            let lit = (0.78 + 0.22 * (v * 0.5 + 0.5)) * (0.92 + 0.16 * grain);
            let c = (lit.min(1.0) * 255.0) as u8;
            data.extend_from_slice(&[c, c, c, (a * 255.0) as u8]);
        }
    }
    let mut img = Image::new(
        Extent3d { width: n as u32, height: n as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    img
}

/// Tire track texture: tread bars across the track, soft edges (u across, v along).
fn tread_image() -> Image {
    let (w, h) = (32usize, 32usize);
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            let edge = smooth(0.0, 0.18, u) * smooth(0.0, 0.18, 1.0 - u);
            // chevron bars
            let chev = (v + (u - 0.5).abs() * 0.6).fract();
            let bar = if chev < 0.45 { 1.0 } else { 0.88 };
            let a = edge * bar;
            let g = (200.0 + 55.0 * (1.0 - bar)) as u8;
            data.extend_from_slice(&[g, g, g, (a * 255.0) as u8]);
        }
    }
    let mut img = Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_v: bevy::image::ImageAddressMode::Repeat,
        anisotropy_clamp: 8,
        ..ImageSamplerDescriptor::linear()
    });
    img
}

// ---------------------------------------------------------------------------------------------
// dust

const DUST_BUCKETS: usize = 10;

struct Particle {
    e: Entity,
    pos: Vec3,
    vel: Vec3,
    age: f32,
    life: f32,
    s0: f32,
    s1: f32,
    spin: f32,
    /// downward acceleration (sand falls back, fine dust hangs)
    g: f32,
    bucket: usize,
    alive: bool,
}

#[derive(Resource)]
struct Dust {
    parts: Vec<Particle>,
    buckets: Vec<Handle<StandardMaterial>>,
    next: usize,
    /// fractional particles owed per wheel
    carry: [f32; 4],
    since_on: [f32; 4],
    rng: u32,
    was_air: bool,
    min_vy: f32,
}

impl Dust {
    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }
    fn rand_dir(&mut self) -> Vec3 {
        Vec3::new(self.rand() - 0.5, self.rand() - 0.5, self.rand() - 0.5) * 2.0
    }
    fn emit(&mut self, pos: Vec3, vel: Vec3, life: f32, s0: f32, s1: f32, g: f32) {
        let i = self.next;
        self.next = (self.next + 1) % self.parts.len();
        let spin = self.rand() * TAU;
        let p = &mut self.parts[i];
        *p = Particle { e: p.e, pos, vel, age: 0.0, life, s0, s1, spin, g, bucket: usize::MAX, alive: true };
    }
}

/// Wheel contact points (world) and whether each wheel touches the ground.
fn wheel_contacts(b: &Buggy) -> [(Vec3, bool); 4] {
    let up = b.rot * Vec3::Y;
    std::array::from_fn(|i| {
        let hub = b.pos + b.rot * (WHEELS[i] - VIS_COM - Vec3::Y * b.wheel_len[i]);
        (hub - up * VIS_WHEEL_R, b.wheel_len[i] < b.vis_rest() - 1e-3)
    })
}

#[allow(clippy::too_many_arguments)]
fn dust(
    buggy: Option<Res<Buggy>>,
    site: Option<Res<Site>>,
    dust: Option<ResMut<Dust>>,
    quality: Res<Quality>,
    time: Res<Time>,
    cam: Query<&Transform, (With<Camera3d>, Without<Fx>)>,
    mut q: Query<(&mut Transform, &mut Visibility, &mut MeshMaterial3d<StandardMaterial>), With<Fx>>,
    mut fx: ResMut<CamFx>,
) {
    let (Some(b), Some(mut d), Ok(cam)) = (buggy, dust, cam.single()) else { return };
    let dt = time.delta_secs().min(0.1);
    let speed = b.vel.length();
    let contacts = wheel_contacts(&b);

    // rolling plume from each wheel on the ground
    let rate_scale = if quality.low { 0.35 } else { 1.0 };
    for (i, (c, on)) in contacts.iter().enumerate() {
        if *on {
            d.since_on[i] = 0.0;
        } else {
            d.since_on[i] += dt;
        }
        // keep puffing for a moment over small hops so the plume doesn't flicker
        if d.since_on[i] > 0.15 || speed < 2.5 {
            continue;
        }
        let rate = ((speed - 2.5) * 2.2).min(45.0) * rate_scale * if b.boosting { 1.5 } else { 1.0 };
        d.carry[i] += rate * dt;
        while d.carry[i] >= 1.0 {
            d.carry[i] -= 1.0;
            let r = d.rand_dir();
            let jitter = d.rand();
            let at = *c + Vec3::Y * 0.15 - b.vel * dt * jitter;
            if i >= 2 && d.rand() < 0.45 {
                // rear paddles throw a rooster tail of sand: backwards and up, falling back
                let fwd = b.forward();
                let throw = (speed * 0.22).min(7.0);
                let v = b.vel * 0.55 - fwd * throw + Vec3::Y * (1.6 + 2.2 * d.rand()) + Vec3::new(r.x, 0.0, r.z) * 0.8;
                let life = 1.0 + 0.9 * d.rand();
                let s1 = 0.6 + 0.5 * d.rand();
                d.emit(at, v, life, 0.2, s1, 2.2);
            } else {
                let v = -b.vel * (0.12 + 0.1 * jitter) + Vec3::Y * (0.5 + 1.1 * d.rand()) + Vec3::new(r.x, 0.0, r.z) * 0.9;
                let life = 1.2 + 1.3 * d.rand();
                let s1 = 1.3 + (speed * 0.04).min(1.0) + 0.9 * d.rand();
                d.emit(at, v, life, 0.3, s1, 0.12);
            }
        }
    }

    // landing burst
    let air = b.airborne();
    if air {
        d.min_vy = d.min_vy.min(b.vel.y);
    } else {
        if d.was_air && d.min_vy < -2.5 {
            let n = ((-d.min_vy - 2.0) * 9.0).min(60.0) * rate_scale.max(0.5);
            let centre = contacts.iter().map(|c| c.0).sum::<Vec3>() / 4.0;
            for _ in 0..n as usize {
                let a = d.rand() * TAU;
                let out = Vec3::new(a.cos(), 0.0, a.sin());
                let v = out * (2.0 + 3.5 * d.rand()) + Vec3::Y * (0.4 + 1.2 * d.rand()) + b.vel * 0.25;
                let life = 1.8 + 1.8 * d.rand();
                let s1 = 2.4 + 1.5 * d.rand();
                d.emit(centre + out * 1.2 + Vec3::Y * 0.2, v, life, 0.5, s1 * 0.6, 0.12);
            }
            fx.trauma = (fx.trauma + 0.1).min(1.0);
        }
        d.min_vy = 0.0;
    }
    d.was_air = air;

    if std::env::var("MM_DEBUG").is_ok() && (time.elapsed_secs() * 1.0).fract() < dt {
        info!("dust alive {} contacts {:?}", d.parts.iter().filter(|p| p.alive).count(), contacts.map(|c| c.1));
    }
    // integrate + draw
    let drag = (-1.6 * dt).exp();
    let n_buckets = d.buckets.len();
    let Dust { parts, buckets, .. } = &mut *d;
    for p in parts.iter_mut() {
        let Ok((mut t, mut vis, mut mat)) = q.get_mut(p.e) else { continue };
        if !p.alive {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        }
        p.age += dt;
        if p.age >= p.life {
            p.alive = false;
            *vis = Visibility::Hidden;
            continue;
        }
        p.vel = p.vel * drag + Vec3::new(0.25, -p.g, 0.0) * dt; // a light breeze; sand falls back
        p.pos += p.vel * dt;
        let x = p.age / p.life;
        let size = p.s0 + (p.s1 - p.s0) * (1.0 - (1.0 - x).powi(2));
        // keep the puff's centre above the ground so the quad doesn't cut a hard line into it
        if let Some(g) = site.as_ref().and_then(|s| s.height(p.pos.x, p.pos.z)) {
            let min_y = g + size * 0.32;
            if p.pos.y < min_y {
                p.pos.y = min_y;
                p.vel.y = p.vel.y.max(0.0);
            }
        }
        // never fill the view: fade out puffs the camera gets close to (it drives through
        // the plume it follows), and drop ones right at the lens
        let cam_d = p.pos.distance(cam.translation) - size * 0.5;
        if cam_d < 1.5 {
            p.alive = false;
            *vis = Visibility::Hidden;
            continue;
        }
        let alpha = smooth(0.0, 0.08, x) * (1.0 - x).powf(1.3) * smooth(1.5, 7.0, cam_d);
        let k = ((alpha * n_buckets as f32) as usize).min(n_buckets - 1);
        if k != p.bucket {
            p.bucket = k;
            mat.0 = buckets[k].clone();
        }
        *t = Transform::from_translation(p.pos).with_rotation(cam.rotation * Quat::from_rotation_z((p.spin.sin()) * 0.25)).with_scale(Vec3::new(if p.spin > PI { -size } else { size }, size, size));
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// tire tracks

struct TrackPt {
    pos: Vec3,
    side: Vec3,
    n: Vec3,
    t: f32,
    v: f32,
    /// first point of a new strip (the wheel was off the ground before it)
    start: bool,
}

struct Track {
    mesh: Handle<Mesh>,
    pts: VecDeque<TrackPt>,
    dirty: bool,
    broken: bool,
    off_t: f32,
}

#[derive(Resource)]
struct Tracks {
    list: Vec<Track>,
    max_pts: usize,
    life: f32,
}

const TRACK_W: f32 = 0.30;
const TRACK_STEP: f32 = 0.35;

fn tracks(buggy: Option<Res<Buggy>>, site: Option<Res<Site>>, tracks: Option<ResMut<Tracks>>, time: Res<Time>, mut meshes: ResMut<Assets<Mesh>>, mut last_respawns: Local<u32>) {
    let (Some(b), Some(site), Some(mut tr)) = (buggy, site, tracks) else { return };
    let now = time.elapsed_secs();
    let dt = time.delta_secs().min(0.1);
    let contacts = wheel_contacts(&b);
    let fwd = b.forward();
    let respawned = b.respawns != *last_respawns;
    *last_respawns = b.respawns;
    let (max_pts, life) = (tr.max_pts, tr.life);
    for (i, track) in tr.list.iter_mut().enumerate() {
        let (c, on) = contacts[i];
        if respawned {
            track.broken = true;
        }
        if on {
            track.off_t = 0.0;
        } else {
            track.off_t += dt;
            // a real jump (not a little skip over a bump) ends the strip
            if track.off_t > 0.3 {
                track.broken = true;
            }
        }
        if on {
            if let Some(h) = site.height(c.x, c.z) {
                let p = Vec3::new(c.x, h, c.z);
                let far_enough = track.pts.back().is_none_or(|l| l.pos.distance(p) >= TRACK_STEP);
                if track.broken || far_enough {
                    if track.pts.back().is_some_and(|l| l.pos.distance(p) > 3.0) {
                        track.broken = true;
                    }
                    let n = site.normal(c.x, c.z);
                    let dir = track.pts.back().filter(|_| !track.broken).map_or(fwd, |l| (p - l.pos).normalize_or(fwd));
                    let side = dir.cross(n).normalize_or(Vec3::X);
                    let v = track.pts.back().filter(|_| !track.broken).map_or(0.0, |l| l.v + l.pos.distance(p) / 0.9);
                    track.pts.push_back(TrackPt { pos: p, side, n, t: now, v, start: track.broken });
                    track.broken = false;
                    track.dirty = true;
                }
            }
        }
        while track.pts.len() > max_pts || track.pts.front().is_some_and(|p| now - p.t > life) {
            track.pts.pop_front();
            track.dirty = true;
        }
        // rebuild a few times a second as alphas fade, every frame when points were added
        if !track.dirty && (now * 4.0).fract() > 0.1 {
            continue;
        }
        track.dirty = false;
        let Some(mut mesh) = meshes.get_mut(&track.mesh) else { continue };
        let n = track.pts.len();
        let mut pos = Vec::with_capacity(n * 2);
        let mut nrm = Vec::with_capacity(n * 2);
        let mut uv = Vec::with_capacity(n * 2);
        let mut col = Vec::with_capacity(n * 2);
        let mut idx = Vec::with_capacity(n * 6);
        for (k, p) in track.pts.iter().enumerate() {
            let age = (now - p.t) / life;
            // fade out with age, and in over the newest 2 points so the end isn't a hard edge
            let fade_new = ((n - 1 - k) as f32 / 2.0).min(1.0) * 0.6 + 0.4;
            let a = (1.0 - age).clamp(0.0, 1.0).powf(0.7) * fade_new;
            let lift = p.n * 0.035;
            pos.push((p.pos + lift - p.side * TRACK_W * 0.5).to_array());
            pos.push((p.pos + lift + p.side * TRACK_W * 0.5).to_array());
            nrm.push(p.n.to_array());
            nrm.push(p.n.to_array());
            uv.push([0.0, p.v]);
            uv.push([1.0, p.v]);
            col.push([1.0, 1.0, 1.0, a]);
            col.push([1.0, 1.0, 1.0, a]);
            if k > 0 && !p.start {
                let (a0, b0, a1, b1) = (2 * k as u32 - 2, 2 * k as u32 - 1, 2 * k as u32, 2 * k as u32 + 1);
                idx.extend_from_slice(&[a0, b0, a1, b0, b1, a1]);
            }
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nrm);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
        if std::env::var("MM_DEBUG").is_ok() && i == 3 {
            info!("track {i}: pts {} idx {}", track.pts.len(), idx.len());
        }
        mesh.insert_indices(Indices::U32(idx));
    }
}

// ---------------------------------------------------------------------------------------------
// the buggy model

/// Boost flame cone: size factor and the exhaust tip it grows from (chassis frame).
#[derive(Component)]
struct Flame(f32, Vec3);

#[derive(Component)]
struct FlameLight;

#[derive(Component)]
struct Whip(Quat);

/// A tube between two points that move with the suspension (upper/lower arms, shocks).
#[derive(Component)]
struct Rod {
    wheel: usize,
    a: Anchor,
    b: Anchor,
    radius: f32,
}

#[derive(Clone, Copy)]
enum Anchor {
    /// fixed point in the chassis frame (chassis box centre origin)
    Body(Vec3),
    /// offset from the wheel hub, in chassis axes (doesn't spin or steer)
    Hub(Vec3),
    /// fraction along the lower arm from its inner mount (for the shock's lower eye)
    Arm(Vec3, Vec3, f32),
}

struct Kit {
    cyl: Handle<Mesh>,
    ball: Handle<Mesh>,
    cube: Handle<Mesh>,
}

fn rod_transform(a: Vec3, b: Vec3, r: f32) -> Transform {
    let d = b - a;
    let len = d.length().max(1e-4);
    Transform::from_translation((a + b) * 0.5).with_rotation(Quat::from_rotation_arc(Vec3::Y, d / len)).with_scale(Vec3::new(r, len, r))
}

type Part = (Handle<Mesh>, Handle<StandardMaterial>, Transform);

/// Build the sand rail. The chassis entity's origin is the physics chassis box centre;
/// local axes +X right, +Y up, -Z forward. Ground is ~1.12 m below the origin at rest.
pub fn spawn_buggy(commands: &mut Commands, meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) {
    let m = |c: Color, rough: f32, metal: f32| StandardMaterial { base_color: c, perceptual_roughness: rough, metallic: metal, ..default() };
    let paint = materials.add(StandardMaterial { clearcoat: 0.6, clearcoat_perceptual_roughness: 0.2, ..m(Color::srgb(0.86, 0.24, 0.03), 0.35, 0.0) });
    let cage = materials.add(m(Color::srgb(0.93, 0.93, 0.90), 0.32, 0.0));
    let steel = materials.add(m(Color::srgb(0.16, 0.16, 0.17), 0.5, 0.7));
    let chrome = materials.add(m(Color::srgb(0.85, 0.85, 0.87), 0.12, 1.0));
    let alu = materials.add(m(Color::srgb(0.62, 0.63, 0.66), 0.3, 1.0));
    let rubber = materials.add(m(Color::srgb(0.035, 0.033, 0.032), 0.88, 0.0));
    let rim_mat = materials.add(m(Color::srgb(0.10, 0.10, 0.11), 0.35, 0.8));
    let seat_mat = materials.add(m(Color::srgb(0.07, 0.07, 0.08), 0.85, 0.0));
    let seat_trim = materials.add(m(Color::srgb(0.85, 0.30, 0.06), 0.6, 0.0));
    let suit = materials.add(m(Color::srgb(0.88, 0.88, 0.85), 0.75, 0.0));
    let visor = materials.add(m(Color::srgb(0.95, 0.70, 0.25), 0.08, 1.0));
    let spring = materials.add(m(Color::srgb(0.95, 0.80, 0.08), 0.35, 0.2));
    let glass = materials.add(StandardMaterial { base_color: Color::WHITE, emissive: LinearRgba::rgb(14.0, 13.0, 11.0), ..default() });
    let tail = materials.add(StandardMaterial { base_color: Color::srgb(0.5, 0.0, 0.0), emissive: LinearRgba::rgb(6.0, 0.15, 0.05), ..default() });
    let flag = materials.add(StandardMaterial { base_color: Color::srgb(1.0, 0.45, 0.05), emissive: LinearRgba::rgb(0.6, 0.15, 0.0), double_sided: true, cull_mode: None, ..default() });
    let flame_outer = materials.add(StandardMaterial { base_color: Color::linear_rgba(2.4, 0.75, 0.12, 0.6), alpha_mode: AlphaMode::Add, unlit: true, ..default() });
    let flame_core = materials.add(StandardMaterial { base_color: Color::linear_rgba(1.2, 1.5, 3.5, 0.7), alpha_mode: AlphaMode::Add, unlit: true, ..default() });

    let kit = Kit {
        cyl: meshes.add(Cylinder::new(1.0, 1.0).mesh().resolution(14)),
        ball: meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap()),
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
    };
    let mut parts: Vec<Part> = Vec::new();
    let tube = |parts: &mut Vec<Part>, a: Vec3, b: Vec3, r: f32, mat: &Handle<StandardMaterial>| {
        parts.push((kit.cyl.clone(), mat.clone(), rod_transform(a, b, r)));
    };
    let boxy = |parts: &mut Vec<Part>, c: Vec3, size: Vec3, rot: Quat, mat: &Handle<StandardMaterial>| {
        parts.push((kit.cube.clone(), mat.clone(), Transform::from_translation(c).with_rotation(rot).with_scale(size)));
    };
    let ball = |parts: &mut Vec<Part>, c: Vec3, r: f32, mat: &Handle<StandardMaterial>| {
        parts.push((kit.ball.clone(), mat.clone(), Transform::from_translation(c).with_scale(Vec3::splat(r))));
    };
    let mir = |v: Vec3| Vec3::new(-v.x, v.y, v.z);

    // --- tube frame (each node list is the right side; mirrored for the left) ---
    let f0 = Vec3::new(0.30, -0.22, -1.72); // nose tip
    let f1 = Vec3::new(0.55, -0.30, -1.05);
    let f2 = Vec3::new(0.64, -0.32, -0.25);
    let f3 = Vec3::new(0.64, -0.32, 0.62); // main hoop foot
    let f4 = Vec3::new(0.50, -0.22, 1.35);
    let f5 = Vec3::new(0.40, -0.02, 1.80); // rear bumper corner
    let u1 = Vec3::new(0.48, 0.12, -1.02); // cowl front
    let u2 = Vec3::new(0.62, 0.22, -0.42); // dash
    let r1 = Vec3::new(0.54, 1.10, -0.08); // roof front corner
    let r2 = Vec3::new(0.58, 1.18, 0.58); // main hoop top
    let hh = Vec3::new(0.66, 0.50, 0.62); // main hoop shoulder
    let r3 = Vec3::new(0.42, 0.50, 1.50); // rear cage top
    let bp = Vec3::new(0.40, -0.10, -1.88); // front bumper
    let rt = 0.032; // tube radius
    let frame: &[(Vec3, Vec3)] = &[(f0, f1), (f1, f2), (f2, f3), (f3, f4), (f4, f5), (f0, u1), (f1, u1), (u1, u2), (u2, f2), (u2, r1), (r1, r2), (f3, hh), (hh, r2), (u2, hh), (r2, r3), (r3, f4), (r3, f5), (f0, bp), (bp, f1)];
    for &(a, b) in frame {
        tube(&mut parts, a, b, rt, &cage);
        tube(&mut parts, mir(a), mir(b), rt, &cage);
    }
    for p in [f0, f1, f2, f3, f4, f5, u1, u2, r1, r2, hh, r3, bp] {
        ball(&mut parts, p, rt * 1.12, &cage);
        ball(&mut parts, mir(p), rt * 1.12, &cage);
    }
    for p in [f0, f1, f2, f3, f5, u1, u2, r1, r2, r3, bp] {
        tube(&mut parts, mir(p), p, rt, &cage); // cross members
    }
    tube(&mut parts, mir(f3), r2, rt * 0.9, &cage); // main hoop diagonal
    tube(&mut parts, mir(hh), hh, rt * 0.9, &cage); // harness bar

    // --- panels ---
    let slope = |a: Vec3, b: Vec3| Quat::from_rotation_x(-((b.y - a.y) / (b.z - a.z)).atan());
    boxy(&mut parts, Vec3::new(0.0, 0.19, -0.72), Vec3::new(1.12, 0.025, 0.66), slope(u1, u2), &paint); // hood
    boxy(&mut parts, Vec3::new(0.0, (f0.y + u1.y) * 0.5 + 0.03, (f0.z + u1.z) * 0.5), Vec3::new(0.78, 0.025, 0.74), slope(f0, u1), &paint); // nose
    for s in [-1.0f32, 1.0] {
        boxy(&mut parts, Vec3::new(0.655 * s, -0.17, 0.18), Vec3::new(0.02, 0.28, 0.84), Quat::IDENTITY, &paint); // side pods
        // side pod number plate
        boxy(&mut parts, Vec3::new(0.668 * s, -0.17, 0.18), Vec3::new(0.005, 0.18, 0.30), Quat::IDENTITY, &cage);
    }
    boxy(&mut parts, Vec3::new(0.0, 1.185, 0.25), Vec3::new(1.12, 0.022, 0.70), slope(r1, r2), &paint); // roof
    boxy(&mut parts, Vec3::new(0.0, -0.33, 0.20), Vec3::new(1.26, 0.02, 2.2), Quat::IDENTITY, &steel); // floor pan
    boxy(&mut parts, Vec3::new(0.0, -0.10, -1.55), Vec3::new(0.50, 0.30, 0.25), Quat::IDENTITY, &steel); // front box / steering rack

    // --- headlights, light bar, tail lights ---
    for s in [-1.0f32, 1.0] {
        let p = Vec3::new(0.30 * s, 0.10, -1.50);
        let face = Quat::from_rotation_x(FRAC_PI_2);
        parts.push((kit.cyl.clone(), steel.clone(), Transform::from_translation(p).with_rotation(face).with_scale(Vec3::new(0.085, 0.10, 0.085))));
        parts.push((kit.cyl.clone(), glass.clone(), Transform::from_translation(p - Vec3::Z * 0.052).with_rotation(face).with_scale(Vec3::new(0.07, 0.01, 0.07))));
        boxy(&mut parts, Vec3::new(0.40 * s, 0.12, 1.83), Vec3::new(0.10, 0.06, 0.03), Quat::IDENTITY, &tail);
    }
    boxy(&mut parts, Vec3::new(0.0, 1.235, -0.10), Vec3::new(0.95, 0.08, 0.08), Quat::IDENTITY, &steel);
    for k in 0..6 {
        let x = -0.38 + k as f32 * 0.152;
        boxy(&mut parts, Vec3::new(x, 1.235, -0.142), Vec3::new(0.11, 0.05, 0.01), Quat::IDENTITY, &glass);
    }

    // --- seats, driver (in a suit), steering ---
    for s in [-1.0f32, 1.0] {
        let x = 0.31 * s;
        boxy(&mut parts, Vec3::new(x, -0.20, 0.22), Vec3::new(0.44, 0.10, 0.46), Quat::IDENTITY, &seat_mat);
        boxy(&mut parts, Vec3::new(x, 0.18, 0.50), Vec3::new(0.44, 0.70, 0.09), Quat::from_rotation_x(0.18), &seat_mat);
        for bs in [-1.0f32, 1.0] {
            boxy(&mut parts, Vec3::new(x + 0.20 * bs, 0.05, 0.36), Vec3::new(0.05, 0.36, 0.30), Quat::from_rotation_x(0.18), &seat_trim);
        }
    }
    let dx = -0.31;
    boxy(&mut parts, Vec3::new(dx, 0.20, 0.36), Vec3::new(0.38, 0.50, 0.24), Quat::from_rotation_x(0.15), &suit); // torso
    ball(&mut parts, Vec3::new(dx, 0.62, 0.30), 0.155, &suit); // helmet
    parts.push((kit.ball.clone(), visor.clone(), Transform::from_xyz(dx, 0.63, 0.205).with_scale(Vec3::new(0.12, 0.085, 0.07))));
    boxy(&mut parts, Vec3::new(dx, 0.20, 0.36 + 0.13), Vec3::new(0.26, 0.32, 0.10), Quat::from_rotation_x(0.15), &steel); // life-support pack
    for s in [-1.0f32, 1.0] {
        // arms to the wheel
        tube(&mut parts, Vec3::new(dx + 0.19 * s, 0.38, 0.32), Vec3::new(dx + 0.13 * s, 0.24, -0.12), 0.05, &suit);
        // legs
        tube(&mut parts, Vec3::new(dx + 0.10 * s, -0.10, 0.20), Vec3::new(dx + 0.11 * s, -0.08, -0.40), 0.065, &suit);
    }
    let wheel_c = Vec3::new(dx, 0.26, -0.16);
    let wheel_rot = Quat::from_rotation_x(FRAC_PI_2 - 0.45);
    parts.push((meshes.add(Torus::new(0.145, 0.185).mesh().minor_resolution(8).major_resolution(24)), seat_mat.clone(), Transform::from_translation(wheel_c).with_rotation(wheel_rot)));
    tube(&mut parts, wheel_c, Vec3::new(dx, 0.12, -0.50), 0.02, &steel);

    // --- engine, exhausts, spare wheel ---
    boxy(&mut parts, Vec3::new(0.0, 0.02, 1.28), Vec3::new(0.70, 0.38, 0.55), Quat::IDENTITY, &steel);
    for s in [-1.0f32, 1.0] {
        boxy(&mut parts, Vec3::new(0.40 * s, 0.05, 1.28), Vec3::new(0.12, 0.26, 0.42), Quat::IDENTITY, &alu); // heads
    }
    parts.push((kit.cyl.clone(), chrome.clone(), Transform::from_xyz(0.0, 0.32, 1.22).with_scale(Vec3::new(0.13, 0.20, 0.13)))); // air filter
    let mut tips = Vec::new();
    for s in [-1.0f32, 1.0] {
        let a = Vec3::new(0.22 * s, 0.02, 1.48);
        let b = Vec3::new(0.24 * s, 0.24, 1.72);
        let c = Vec3::new(0.24 * s, 0.26, 1.92);
        tube(&mut parts, a, b, 0.04, &chrome);
        tube(&mut parts, b, c, 0.045, &chrome);
        ball(&mut parts, b, 0.042, &chrome);
        tips.push(c);
    }
    let spare_tire = meshes.add(tire_mesh(true));
    let spare_rot = Quat::from_rotation_x(-0.5) * Quat::from_rotation_y(FRAC_PI_2);
    parts.push((spare_tire, rubber.clone(), Transform::from_xyz(0.0, 0.88, 1.12).with_rotation(spare_rot).with_scale(Vec3::splat(0.85))));
    parts.push((kit.cyl.clone(), rim_mat.clone(), Transform::from_xyz(0.0, 0.88, 1.12).with_rotation(spare_rot * Quat::from_rotation_z(FRAC_PI_2)).with_scale(Vec3::new(0.21, 0.24, 0.21))));

    let whip_base = Vec3::new(0.56, 1.20, 0.62);
    let flame_cone = meshes.add(Cone { radius: 1.0, height: 1.0 }.mesh().resolution(16));

    commands.spawn((Chassis, Transform::default(), Visibility::default())).with_children(|c| {
        for (mesh, mat, t) in parts {
            c.spawn((Mesh3d(mesh), MeshMaterial3d(mat), t));
        }
        // antenna whip + flag (sways with the chassis acceleration)
        c.spawn((Whip(Quat::from_rotation_x(0.12)), Transform::from_translation(whip_base), Visibility::default())).with_children(|w| {
            w.spawn((Mesh3d(kit.cyl.clone()), MeshMaterial3d(cage.clone()), Transform::from_xyz(0.0, 0.9, 0.0).with_scale(Vec3::new(0.008, 1.8, 0.008))));
            w.spawn((Mesh3d(kit.cube.clone()), MeshMaterial3d(flag.clone()), Transform::from_xyz(0.0, 1.66, 0.17).with_scale(Vec3::new(0.008, 0.22, 0.32)), NotShadowCaster));
        });
        // boost flames: outer cone + hot core, pointing back out of the exhaust tips
        for tip in &tips {
            for (k, mat) in [(1.0, &flame_outer), (0.55, &flame_core)] {
                c.spawn((Flame(k, *tip), Mesh3d(flame_cone.clone()), MeshMaterial3d(mat.clone()),
                    Transform::from_translation(*tip), Visibility::Hidden, NotShadowCaster, NotShadowReceiver));
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        c.spawn((FlameLight, PointLight { color: Color::srgb(1.0, 0.55, 0.2), intensity: 0.0, range: 12.0, shadow_maps_enabled: false, ..default() },
            Transform::from_xyz(0.0, 0.3, 2.3)));
    });

    // --- wheels ---
    let tire_front = meshes.add(tire_mesh(false));
    let tire_rear = meshes.add(tire_mesh(true));
    let ring = meshes.add(Torus::new(0.245, 0.285).mesh().minor_resolution(8).major_resolution(32));
    for i in 0..4 {
        let s = if i % 2 == 0 { -1.0 } else { 1.0 }; // outward
        let tire = if i < 2 { tire_front.clone() } else { tire_rear.clone() };
        let lay = Quat::from_rotation_z(FRAC_PI_2); // Y-axis primitives -> X axle
        commands.spawn((Wheel(i), Transform::default(), Visibility::default())).with_children(|w| {
            w.spawn((Mesh3d(tire), MeshMaterial3d(rubber.clone()), Transform::IDENTITY));
            w.spawn((Mesh3d(kit.cyl.clone()), MeshMaterial3d(rim_mat.clone()), Transform::from_rotation(lay).with_scale(Vec3::new(0.255, 0.30, 0.255))));
            w.spawn((Mesh3d(ring.clone()), MeshMaterial3d(alu.clone()), Transform::from_xyz(0.155 * s, 0.0, 0.0).with_rotation(lay)));
            w.spawn((Mesh3d(kit.cyl.clone()), MeshMaterial3d(alu.clone()), Transform::from_xyz(0.16 * s, 0.0, 0.0).with_rotation(lay).with_scale(Vec3::new(0.075, 0.06, 0.075))));
            for k in 0..6 {
                let a = k as f32 / 6.0 * TAU;
                let r = Quat::from_rotation_x(a);
                w.spawn((Mesh3d(kit.cube.clone()), MeshMaterial3d(alu.clone()),
                    Transform::from_translation(Vec3::new(0.152 * s, 0.0, 0.0) + r * Vec3::Y * 0.15).with_rotation(r).with_scale(Vec3::new(0.025, 0.18, 0.045))));
            }
            for k in 0..12 {
                let a = (k as f32 + 0.5) / 12.0 * TAU;
                w.spawn((Mesh3d(kit.ball.clone()), MeshMaterial3d(chrome.clone()),
                    Transform::from_translation(Vec3::new(0.175 * s, a.cos() * 0.265, a.sin() * 0.265)).with_scale(Vec3::splat(0.011))));
            }
        });

        // suspension: double wishbones (front) / trailing-ish arms (rear), coilover with spring
        let z0 = WHEELS[i].z;
        let (zf, zb) = if i < 2 { (z0 - 0.26, z0 + 0.26) } else { (z0 - 0.55, z0 - 0.15) };
        let body = |x: f32, y: f32, z: f32| Anchor::Body(Vec3::new(x * s, y, z));
        let hub = |x: f32, y: f32| Anchor::Hub(Vec3::new(x * s, y, 0.0));
        let lower_in = Vec3::new(0.56 * s, -0.30, (zf + zb) * 0.5);
        let rods = [
            (body(0.56, -0.30, zf), hub(-0.12, -0.11), 0.026, &steel),
            (body(0.56, -0.30, zb), hub(-0.12, -0.11), 0.026, &steel),
            (body(0.58, 0.02, zf + 0.08), hub(-0.13, 0.12), 0.022, &steel),
            (body(0.58, 0.02, zb - 0.08), hub(-0.13, 0.12), 0.022, &steel),
            (hub(-0.12, -0.11), hub(-0.13, 0.12), 0.03, &steel), // knuckle
        ];
        for (a, b, r, mat) in rods {
            commands.spawn((Fx, Rod { wheel: i, a, b, radius: r }, Mesh3d(kit.cyl.clone()), MeshMaterial3d(mat.clone()), Transform::default()));
        }
        let top = body(0.50, 0.52, z0 + if i < 2 { 0.10 } else { -0.20 });
        let eye = Anchor::Arm(lower_in, Vec3::new(-0.12 * s, -0.11, 0.0), 0.72);
        commands.spawn((Fx, Rod { wheel: i, a: top, b: eye, radius: 0.04 }, Mesh3d(kit.cyl.clone()), MeshMaterial3d(alu.clone()), Transform::default(), RodPart(0.0, 0.55)));
        commands.spawn((Fx, Rod { wheel: i, a: top, b: eye, radius: 0.016 }, Mesh3d(kit.cyl.clone()), MeshMaterial3d(chrome.clone()), Transform::default()));
        commands.spawn((Fx, Rod { wheel: i, a: top, b: eye, radius: 1.0 }, Mesh3d(meshes.add(spring_mesh())), MeshMaterial3d(spring.clone()), Transform::default(), RodPart(0.12, 0.92)));
    }
}

/// Draw only part of a rod: from fraction .0 to .1 of the way from a to b.
#[derive(Component)]
struct RodPart(f32, f32);

fn anchor_pos(a: Anchor, chassis: &Transform, hub: Vec3) -> Vec3 {
    match a {
        Anchor::Body(p) => chassis.transform_point(p),
        Anchor::Hub(o) => hub + chassis.rotation * o,
        Anchor::Arm(inner, hub_off, f) => chassis.transform_point(inner).lerp(hub + chassis.rotation * hub_off, f),
    }
}

#[allow(clippy::type_complexity)]
fn suspension(
    chassis: Query<&Transform, (With<Chassis>, Without<Rod>)>,
    wheels: Query<(&Wheel, &Transform), (Without<Chassis>, Without<Rod>)>,
    mut rods: Query<(&Rod, &mut Transform, Option<&RodPart>), (Without<Chassis>, Without<Wheel>)>,
) {
    let Ok(ct) = chassis.single() else { return };
    let mut hubs = [Vec3::ZERO; 4];
    for (w, t) in &wheels {
        hubs[w.0] = t.translation;
    }
    for (rod, mut t, part) in &mut rods {
        let (mut a, mut b) = (anchor_pos(rod.a, ct, hubs[rod.wheel]), anchor_pos(rod.b, ct, hubs[rod.wheel]));
        if let Some(p) = part {
            (a, b) = (a.lerp(b, p.0), a.lerp(b, p.1));
        }
        *t = rod_transform(a, b, rod.radius);
    }
}

fn whip(buggy: Option<Res<Buggy>>, time: Res<Time>, mut q: Query<(&Whip, &mut Transform)>, mut st: Local<(Vec3, Vec3, Vec3)>) {
    let Some(b) = buggy else { return };
    let dt = time.delta_secs().clamp(1e-4, 0.1);
    // spring-damper "tip offset" driven by the chassis acceleration (in chassis axes)
    let (last_v, off, vel) = &mut *st;
    let acc = (b.vel - *last_v) / dt;
    *last_v = b.vel;
    let local_acc = b.rot.inverse() * acc;
    let force = -local_acc * 0.02 - *off * 60.0 - *vel * 5.0;
    *vel += force * dt;
    *off += *vel * dt;
    *off = off.clamp_length_max(0.35);
    let wobble = (time.elapsed_secs() * 7.0).sin() * 0.01 * b.vel.length().min(30.0) / 30.0;
    for (w, mut t) in &mut q {
        t.rotation = w.0 * Quat::from_rotation_x(off.z + wobble) * Quat::from_rotation_z(-off.x);
    }
}

fn flame(
    buggy: Option<Res<Buggy>>,
    time: Res<Time>,
    mut q: Query<(&Flame, &mut Transform, &mut Visibility)>,
    mut light: Query<&mut PointLight, With<FlameLight>>,
    mut level: Local<f32>,
) {
    let Some(b) = buggy else { return };
    let dt = time.delta_secs().min(0.1);
    let target = if b.boosting { 1.0 } else { 0.0 };
    *level += (target - *level) * (1.0 - (-dt * 14.0).exp());
    let t = time.elapsed_secs();
    for (f, mut tr, mut vis) in &mut q {
        if *level < 0.02 {
            *vis = Visibility::Hidden;
            continue;
        }
        *vis = Visibility::Inherited;
        let flicker = 0.8 + 0.2 * (t * 53.0 + f.0 * 10.0).sin() * (t * 31.0).cos();
        let len = 0.85 * f.0 * *level * flicker;
        let r = 0.075 * f.0.sqrt() * (0.6 + 0.4 * *level);
        // cone apex (+Y) points backwards (+Z); its centre sits half its length behind the tip
        *tr = Transform::from_translation(f.1 + Vec3::Z * (len * 0.5)).with_rotation(Quat::from_rotation_x(FRAC_PI_2)).with_scale(Vec3::new(r, len, r));
    }
    for mut l in &mut light {
        l.intensity = 60_000.0 * *level;
    }
}

/// Tire around the X axle: rounded profile; rear = sand paddles, front = staggered blocks.
fn tire_mesh(paddle: bool) -> Mesh {
    let half_w = 0.18;
    // profile from the inner bead over the tread to the outer bead: (x, r, is_tread)
    let mut prof: Vec<(f32, f32, bool)> = vec![(-0.15, 0.25, false), (-0.172, 0.29, false), (-0.182, 0.33, false), (-0.178, 0.365, false), (-0.165, 0.39, true), (-0.14, 0.402, true)];
    for k in 0..=8 {
        prof.push((-0.12 + 0.24 * k as f32 / 8.0, 0.405, true));
    }
    for &(x, r, t) in prof.clone().iter().rev().take(6) {
        prof.push((-x, r, t));
    }
    let n_ang = 168usize;
    let mut pos = Vec::with_capacity(n_ang * prof.len());
    for a in 0..n_ang {
        let th = a as f32 / n_ang as f32 * TAU;
        for &(x, r, tread) in &prof {
            let mut rr = r;
            if tread {
                if paddle {
                    // 14 transverse paddles across the full tread and over the shoulders
                    let f = (th / TAU * 14.0).fract();
                    rr += if f < 0.16 { 0.032 } else { 0.004 };
                } else {
                    // two staggered rows of blocks with a centre groove
                    let side = if x < 0.0 { 0.0 } else { 0.5 };
                    let f = (th / TAU * 22.0 + side).fract();
                    let centre_groove = x.abs() < 0.018;
                    rr += if !centre_groove && f < 0.62 { 0.015 } else { 0.0 };
                }
            }
            pos.push([x * (half_w / 0.182), rr * th.cos(), rr * th.sin()]);
        }
    }
    let np = prof.len();
    let mut idx = Vec::with_capacity(n_ang * np * 6);
    for a in 0..n_ang {
        let a1 = (a + 1) % n_ang;
        for j in 0..np - 1 {
            let (p00, p10, p01, p11) = ((a * np + j) as u32, (a1 * np + j) as u32, (a * np + j + 1) as u32, (a1 * np + j + 1) as u32);
            // CCW seen from outside: (dtheta x dprofile) points outward
            idx.extend_from_slice(&[p00, p01, p11, p00, p11, p10]);
        }
    }
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_indices(Indices::U32(idx));
    m.compute_smooth_normals();
    m
}

/// Coil spring along Y, unit height centred on the origin, coil radius 0.055 m, wire 0.011 m
/// (the rod transform scales x/z by the rod radius = 1, y by length).
fn spring_mesh() -> Mesh {
    let turns = 7.0;
    let segs = 140usize;
    let sides = 6usize;
    let (rc, rw) = (0.055, 0.011);
    let mut pos = Vec::new();
    for i in 0..=segs {
        let t = i as f32 / segs as f32;
        let a = t * turns * TAU;
        let c = Vec3::new(rc * a.cos(), t - 0.5, rc * a.sin());
        let radial = Vec3::new(a.cos(), 0.0, a.sin());
        for k in 0..sides {
            let b = k as f32 / sides as f32 * TAU;
            let p = c + radial * (rw * b.cos()) + Vec3::Y * (rw * b.sin());
            pos.push(p.to_array());
        }
    }
    let mut idx = Vec::new();
    for i in 0..segs {
        for k in 0..sides {
            let k1 = (k + 1) % sides;
            let (a, b, c, d) = ((i * sides + k) as u32, (i * sides + k1) as u32, ((i + 1) * sides + k) as u32, ((i + 1) * sides + k1) as u32);
            idx.extend_from_slice(&[a, c, d, a, d, b]);
        }
    }
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_indices(Indices::U32(idx));
    m.compute_smooth_normals();
    m
}

/// Chassis and wheel poses from the physics state. Wheels keep spinning (slowing) in the air.
pub fn sync_buggy(
    buggy: Res<Buggy>,
    time: Res<Time>,
    mut chassis: Query<&mut Transform, (With<Chassis>, Without<Wheel>)>,
    mut wheels: Query<(&Wheel, &mut Transform), Without<Chassis>>,
    mut st: Local<([f32; 4], [f32; 4], [f32; 4])>,
) {
    let dt = time.delta_secs().clamp(1e-4, 0.1);
    if let Ok(mut t) = chassis.single_mut() {
        *t = Transform::from_translation(buggy.pos - buggy.rot * VIS_COM).with_rotation(buggy.rot);
    }
    let (last, rate, angle) = &mut *st;
    for i in 0..4 {
        let d = buggy.wheel_spin[i] - last[i];
        last[i] = buggy.wheel_spin[i];
        if d.abs() > 1e-6 && d.abs() < 50.0 {
            rate[i] = d / dt;
            angle[i] += d;
        } else {
            rate[i] *= (-0.6 * dt).exp(); // free-spinning in the air
            angle[i] += rate[i] * dt;
        }
    }
    for (w, mut t) in &mut wheels {
        let i = w.0;
        let off = WHEELS[i] - VIS_COM - Vec3::Y * buggy.wheel_len[i];
        let steer = if i < 2 { buggy.steer } else { 0.0 };
        t.translation = buggy.pos + buggy.rot * off;
        t.rotation = buggy.rot * Quat::from_rotation_y(-steer) * Quat::from_rotation_x(-angle[i]);
    }
}
