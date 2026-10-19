//! Dune buggy: a rigid body on four raycast spring/damper wheels with a friction-circle tire model,
//! under Mars gravity. Ground contact is against the full-resolution DTM heightfield, so every 1 m
//! post is felt. Local axes: +X right, +Y up, -Z forward.

use crate::terrain::Site;
use bevy::prelude::*;

pub const MARS_G: f32 = 3.711;
const MASS: f32 = 420.0; // kg, buggy + driver
const HALF: Vec3 = Vec3::new(0.85, 0.35, 1.5); // chassis half extents (m)
/// Centre of mass relative to the chassis box centre (the visual origin). Engine, driver and
/// floor pan sit low: CoM ~0.67 m above ground at rest, which with the 2.0 m track gives a
/// static stability factor (half track / CoM height) of ~1.5, i.e. a ~56 deg tip-over angle,
/// in line with real sand rails (SSF 1.3-1.6). Physics `pos` is the CoM.
const COM: Vec3 = Vec3::new(0.0, -0.45, 0.0);
/// Principal moments of inertia about the CoM (kg m^2): pitch (X), yaw (Y), roll (Z). Chassis box
/// plus four ~20 kg wheel/hub assemblies 1 m out, which matter most for roll.
const INERTIA: Vec3 = Vec3::new(380.0, 450.0, 190.0);
const K_ARB: f32 = 7000.0; // anti-roll bar rate per axle, N per m of left/right compression difference
const ROLL_CENTRE_H: f32 = 0.25; // height above the contact patch where tire forces act (m)
const WHEEL_R: f32 = 0.42;
const REST: f32 = 0.55; // suspension rest length
const TRAVEL: f32 = 0.45; // max compression before the bump stop
const K: f32 = 9000.0; // spring N/m
const C: f32 = 1300.0; // damper N/(m/s)
const ENGINE: f32 = 2600.0; // N at the wheels, all-wheel drive
const VMAX: f32 = 26.0; // m/s where engine force tapers to zero (~94 km/h)
const BRAKE: f32 = 3500.0;
const STEER_MAX: f32 = 0.55;
const SUBSTEPS: usize = 4;
/// seconds the chassis centre may hang over nodata before we respawn
const OFF_DATA_GRACE: f32 = 0.4;
/// how often a "known good" pose is recorded, and how many are kept
const SAFE_EVERY: f32 = 0.25;
const SAFE_KEEP: usize = 40;

/// Tire / ground model.
///
/// * `arcade` (default): grippy and forgiving. A constant friction circle with mu 0.75, stiff
///   cornering and light rolling drag, so you can fling the buggy over dune crests.
/// * `realistic` (`--realistic`): loose dry sand. A friction *ellipse*: longitudinal peak mu 0.48
///   (tire-on-loose-dry-sand traction is typically ~0.4-0.6) and lateral 0.58, since a tire
///   pushed sideways bulldozes a sand berm that resists the slide. Once the demanded force
///   exceeds the ellipse (wheelspin / sliding), grip falls off with the overload, up to 30% less
///   at 2x, like a tire digging a rut instead of rolling. Rolling resistance adds a
///   load-proportional term (Crr 0.12, soft-sand sinkage) and cornering stiffness is lower, so
///   the buggy understeers, climbs noticeably slower, and slides down side slopes beyond ~28 deg
///   (or earlier under heavy throttle, when the drive force uses up the grip).
#[derive(Clone, Copy)]
pub struct Tuning {
    pub realistic: bool,
    mu: f32,     // longitudinal peak friction coefficient
    mu_lat: f32, // lateral peak friction coefficient
    corner: f32,       // lateral tire stiffness N/(m/s), whole car
    roll_visc: f32,    // N/(m/s) per wheel
    roll_crr: f32,     // load-proportional rolling resistance coefficient
    slip_falloff: f32, // fraction of grip lost at full slip
}

impl Tuning {
    pub fn arcade() -> Self {
        Self { realistic: false, mu: 0.75, mu_lat: 0.75, corner: 7000.0, roll_visc: 12.0, roll_crr: 0.0, slip_falloff: 0.0 }
    }
    pub fn realistic() -> Self {
        Self { realistic: true, mu: 0.48, mu_lat: 0.58, corner: 4500.0, roll_visc: 12.0, roll_crr: 0.12, slip_falloff: 0.3 }
    }
}

pub const WHEELS: [Vec3; 4] = [
    Vec3::new(-1.0, -0.15, -1.25), // front left (relative to the chassis box centre)
    Vec3::new(1.0, -0.15, -1.25),  // front right
    Vec3::new(-1.0, -0.15, 1.2),   // rear left
    Vec3::new(1.0, -0.15, 1.2),    // rear right
];

#[derive(Resource, Default)]
pub struct Controls {
    pub throttle: f32, // -1..1
    pub steer: f32,    // -1 (left) .. 1 (right)
    pub brake: bool,
    pub reset: bool,
}

#[derive(Resource)]
pub struct Buggy {
    pub pos: Vec3,
    pub vel: Vec3,
    pub rot: Quat,
    pub ang: Vec3, // world-space angular velocity
    pub steer: f32,
    pub wheel_len: [f32; 4], // current suspension length (for visuals)
    pub wheel_spin: [f32; 4],
    pub grounded: usize,
    pub tuning: Tuning,
    /// recent poses known to be on good ground: (ground point, yaw), oldest first
    safe: Vec<(Vec3, f32)>,
    safe_t: f32,
    off_data: f32,
    pub respawns: u32,
    pub last_respawn: &'static str,
    /// terrain side slope across the buggy's heading (deg), over a ~4 m footprint
    pub side_slope: f32,
    /// last 1 s of side-slope samples taken while driving upright on >= 2 wheels
    side_hist: std::collections::VecDeque<f32>,
    /// the largest side slope held continuously for 1 s without rolling (deg)
    pub max_sustained_side: f32,
    /// times the buggy went past 80 deg of roll/pitch (up.y < 0.17)
    pub rollovers: u32,
    rolled: bool,
}

impl Buggy {
    pub fn new(site: &Site, at: Vec3, heading: f32, tuning: Tuning) -> Self {
        let mut b = Self {
            pos: Vec3::ZERO,
            vel: Vec3::ZERO,
            rot: Quat::IDENTITY,
            ang: Vec3::ZERO,
            steer: 0.0,
            wheel_len: [REST; 4],
            wheel_spin: [0.0; 4],
            grounded: 0,
            tuning,
            safe: Vec::new(),
            safe_t: 0.0,
            off_data: 0.0,
            respawns: 0,
            last_respawn: "",
            side_slope: 0.0,
            side_hist: Default::default(),
            max_sustained_side: 0.0,
            rollovers: 0,
            rolled: false,
        };
        b.place(site, at, heading);
        b
    }

    /// Set the buggy down over `ground` facing `yaw`, tilted to the local ground plane (~3x5 m
    /// footprint) with the CoM ~0.15 m above its ride height, so spawning on a slope doesn't drop
    /// it level onto the hill. Keeps tuning and history.
    fn place(&mut self, site: &Site, ground: Vec3, yaw: f32) {
        let n = site.normal_at(ground.x, ground.z, 2.0);
        self.rot = Quat::from_rotation_arc(Vec3::Y, n) * Quat::from_rotation_y(yaw);
        self.pos = ground + n * 0.8;
        self.vel = Vec3::ZERO;
        self.ang = Vec3::ZERO;
        self.steer = 0.0;
        self.wheel_len = [REST; 4];
        self.grounded = 0;
        self.off_data = 0.0;
    }

    pub fn forward(&self) -> Vec3 {
        self.rot * Vec3::NEG_Z
    }

    pub fn yaw(&self) -> f32 {
        let f = self.forward();
        f32::atan2(-f.x, -f.z)
    }

    fn inv_inertia_world(&self) -> Mat3 {
        let r = Mat3::from_quat(self.rot);
        r * Mat3::from_diagonal(INERTIA.recip()) * r.transpose()
    }

    /// R key: flip upright where you are (or at the site spawn if not over data).
    pub fn reset(&mut self, site: &Site) {
        let g = site.height(self.pos.x, self.pos.z).map(|h| Vec3::new(self.pos.x, h, self.pos.z)).unwrap_or(site.spawn);
        let yaw = self.yaw();
        self.place(site, g, yaw);
    }

    /// Respawn on the last good ground ~2 s back along the track, turned around so it faces
    /// back into the data (or at the site spawn if there's no history).
    fn respawn(&mut self, site: &Site, why: &'static str) {
        let k = self.safe.len().saturating_sub(8); // 8 entries * SAFE_EVERY = 2 s back
        let target = self.safe.get(k).copied();
        self.safe.truncate(k);
        match target {
            Some((g, yaw)) => self.place(site, g, yaw + std::f32::consts::PI),
            None => self.place(site, site.spawn, 0.0),
        }
        self.respawns += 1;
        self.last_respawn = why;
        info!("respawn #{} ({why}) at ({:.1}, {:.1}, {:.1})", self.respawns, self.pos.x, self.pos.y, self.pos.z);
    }

    pub fn step(&mut self, site: &Site, ctl: &Controls, dt: f32) {
        let h = dt / SUBSTEPS as f32;
        let target_steer = ctl.steer * STEER_MAX / (1.0 + self.vel.length() * 0.06);
        self.steer += (target_steer - self.steer) * (dt * 6.0).min(1.0);
        for _ in 0..SUBSTEPS {
            self.substep(site, ctl, h);
        }
        self.check_bounds(site, dt);
        self.track_slope(site, dt);
    }

    fn track_slope(&mut self, site: &Site, dt: f32) {
        let n = site.normal_at(self.pos.x, self.pos.z, 2.0);
        let r = self.rot * Vec3::X;
        let r = Vec3::new(r.x, 0.0, r.z).normalize_or_zero();
        // height gradient of the terrain along the buggy's horizontal right axis
        let g = -(n.x * r.x + n.z * r.z) / n.y.max(1e-3);
        self.side_slope = g.abs().atan().to_degrees();
        let up_y = (self.rot * Vec3::Y).y;
        if up_y < 0.17 && !self.rolled {
            self.rollovers += 1;
            info!("ROLLOVER #{} at side slope {:.0} deg", self.rollovers, self.side_slope);
        }
        self.rolled = up_y < 0.17;
        let n_win = (1.0 / dt).round() as usize;
        if self.grounded >= 2 && up_y > 0.5 && self.vel.length() > 0.5 {
            self.side_hist.push_back(self.side_slope);
            if self.side_hist.len() > n_win {
                self.side_hist.pop_front();
            }
            if self.side_hist.len() == n_win {
                let held = self.side_hist.iter().copied().fold(f32::INFINITY, f32::min);
                self.max_sustained_side = self.max_sustained_side.max(held);
            }
        } else {
            self.side_hist.clear();
        }
    }

    /// Edge-of-data / sanity handling: never fall forever, never keep a NaN state.
    fn check_bounds(&mut self, site: &Site, dt: f32) {
        let finite = self.pos.is_finite() && self.vel.is_finite() && self.rot.is_finite() && self.ang.is_finite();
        if !finite || self.vel.length() > 150.0 {
            return self.respawn(site, "physics blew up");
        }
        if self.pos.y < site.min_y - 50.0 {
            return self.respawn(site, "fell below the terrain");
        }
        match site.height(self.pos.x, self.pos.z) {
            None => {
                self.off_data += dt;
                if self.off_data > OFF_DATA_GRACE {
                    self.respawn(site, "left the data");
                }
            }
            Some(g) => {
                self.off_data = 0.0;
                if self.pos.y < g - 3.0 {
                    return self.respawn(site, "tunnelled under the ground");
                }
                self.safe_t += dt;
                let up = self.rot * Vec3::Y;
                if self.safe_t >= SAFE_EVERY && self.grounded >= 3 && up.y > 0.7 {
                    self.safe_t = 0.0;
                    if self.safe.len() >= SAFE_KEEP {
                        self.safe.remove(0);
                    }
                    self.safe.push((Vec3::new(self.pos.x, g, self.pos.z), self.yaw()));
                }
            }
        }
    }

    fn substep(&mut self, site: &Site, ctl: &Controls, h: f32) {
        let up = self.rot * Vec3::Y;
        let mut force = Vec3::NEG_Y * MARS_G * MASS;
        let mut torque = Vec3::ZERO;
        let p0 = self.pos;
        let apply = |f: Vec3, at: Vec3, force: &mut Vec3, torque: &mut Vec3| {
            *force += f;
            *torque += (at - p0).cross(f);
        };
        let tu = self.tuning;
        self.grounded = 0;

        // pass 1: ray-cast every wheel (needed up front for the anti-roll bars)
        struct Hit {
            contact: Vec3,
            compression: f32,
            len: f32,
        }
        let mut hits: [Option<Hit>; 4] = [None, None, None, None];
        for (i, off) in WHEELS.iter().enumerate() {
            let mount = self.pos + self.rot * (*off - COM);
            self.wheel_len[i] = REST;
            let Some(dist) = ray_down(site, mount, up) else { continue };
            let len = dist - WHEEL_R;
            if len >= REST {
                continue;
            }
            self.wheel_len[i] = len.max(REST - TRAVEL);
            hits[i] = Some(Hit { contact: mount - up * dist, compression: REST - len, len });
        }
        // anti-roll bars: the more compressed wheel of an axle pushes up harder, the other less
        let comp = |i: usize| hits[i].as_ref().map_or(0.0, |h| h.compression.min(TRAVEL));
        let arb = [comp(0) - comp(1), comp(2) - comp(3)].map(|d| d * K_ARB);

        for i in 0..4 {
            let Some(hit) = &hits[i] else { continue };
            self.grounded += 1;
            let (contact, len) = (hit.contact, hit.len);
            let n = site.contact_normal(contact.x, contact.z);
            let v = self.vel + self.ang.cross(contact - self.pos);

            // suspension (plus a stiff bump stop once travel is used up) + anti-roll bar share
            let bump = (REST - TRAVEL - len).max(0.0) * K * 8.0;
            let bar = arb[i / 2] * if i % 2 == 0 { 1.0 } else { -1.0 };
            let fs = (K * hit.compression + bump + bar - C * v.dot(up)).max(0.0);
            let load = fs * up.dot(n).max(0.0);
            apply(up * fs, contact, &mut force, &mut torque);

            // tire: longitudinal / lateral in the ground plane
            let steer = if i < 2 { self.steer } else { 0.0 };
            let wheel_fwd = self.rot * (Quat::from_rotation_y(-steer) * Vec3::NEG_Z);
            let fwd = (wheel_fwd - n * wheel_fwd.dot(n)).normalize_or_zero();
            let right = fwd.cross(n);
            let (v_long, v_lat) = (v.dot(fwd), v.dot(right));
            let mut f_long = if ctl.brake {
                -v_long.clamp(-1.0, 1.0) * BRAKE / 4.0
            } else {
                let taper = (1.0 - v_long.abs() / VMAX).max(0.0);
                ctl.throttle * ENGINE / 4.0 * if ctl.throttle * v_long < 0.0 { 1.0 } else { taper }
            };
            // rolling resistance in sand (viscous + load-proportional sinkage drag)
            f_long -= v_long * tu.roll_visc + tu.roll_crr * load * v_long.clamp(-1.0, 1.0);
            if ctl.throttle == 0.0 && !ctl.brake && v_long.abs() < 1.0 {
                f_long -= v_long * 3000.0; // idle hold (engine braking / handbrake) so it settles on slopes
            }
            let f_lat = -v_lat * tu.corner / 4.0;
            // friction ellipse (a circle in arcade mode, where mu == mu_lat)
            let k = ((f_long / (tu.mu * load).max(1e-3)).powi(2) + (f_lat / (tu.mu_lat * load).max(1e-3)).powi(2)).sqrt();
            let mut ft = fwd * f_long + right * f_lat;
            if k > 1.0 {
                // sliding: grip falls off with the overload (slip_falloff = 0 in arcade mode)
                let slip = (k - 1.0).clamp(0.0, 1.0);
                ft *= (1.0 - tu.slip_falloff * slip) / k;
            }
            // Tire forces act through the suspension links, so their roll moment on the sprung
            // mass is about the roll centre, not the contact patch. Applying them at the patch
            // (a ground-level roll centre) exaggerates "tripping"; a double-wishbone buggy's roll
            // centre sits ~0.25 m above ground, so the horizontal tire force is applied there.
            apply(ft, contact + up * ROLL_CENTRE_H, &mut force, &mut torque);
            self.wheel_spin[i] += v_long / WHEEL_R * h;
        }

        // chassis corners: penalty contact so a roll-over slides instead of falling through
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                for sz in [-1.0, 1.0] {
                    let p = self.pos + self.rot * (HALF * Vec3::new(sx, sy, sz) - COM);
                    let Some(gh) = site.height(p.x, p.z) else { continue };
                    let pen = gh - p.y;
                    if pen > 0.0 {
                        let n = site.contact_normal(p.x, p.z);
                        let v = self.vel + self.ang.cross(p - self.pos);
                        let fn_ = (pen * 60000.0 - v.dot(n) * 4000.0).max(0.0);
                        let vt = v - n * v.dot(n);
                        let ff = -vt.normalize_or_zero() * (fn_ * 0.6).min(vt.length() * MASS / h / 8.0);
                        apply(n * fn_ + ff, p, &mut force, &mut torque);
                    }
                }
            }
        }

        self.vel += force / MASS * h;
        self.ang += self.inv_inertia_world() * torque * h;
        self.ang *= 1.0 - 0.3 * h; // a little angular damping
        self.pos += self.vel * h;
        let w = self.ang;
        self.rot = (self.rot + Quat::from_xyzw(w.x, w.y, w.z, 0.0) * self.rot * (0.5 * h)).normalize();
    }
}

/// Distance from `mount` down the suspension axis (-`up`) to the ground: intersect the ray with
/// the local ground plane, re-sampled at the hit point a few times (converges in 2-3 steps on
/// smooth ground). None over nodata or if the axis is nearly parallel to the ground.
fn ray_down(site: &Site, mount: Vec3, up: Vec3) -> Option<f32> {
    let mut p = mount;
    let mut dist = 0.0;
    for _ in 0..3 {
        let g = Vec3::new(p.x, site.height(p.x, p.z)?, p.z);
        let n = site.contact_normal(p.x, p.z);
        let denom = up.dot(n);
        if denom < 0.2 {
            return None;
        }
        dist = (mount - g).dot(n) / denom;
        p = mount - up * dist;
    }
    Some(dist)
}

#[derive(Component)]
pub struct Chassis;

#[derive(Component)]
pub struct Wheel(pub usize);

pub fn spawn_visuals(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let paint = materials.add(StandardMaterial { base_color: Color::srgb(0.95, 0.42, 0.08), perceptual_roughness: 0.4, metallic: 0.3, ..default() });
    let steel = materials.add(StandardMaterial { base_color: Color::srgb(0.18, 0.19, 0.2), perceptual_roughness: 0.5, metallic: 0.8, ..default() });
    let seat = materials.add(StandardMaterial { base_color: Color::srgb(0.08, 0.08, 0.09), perceptual_roughness: 0.9, ..default() });
    let tire = materials.add(StandardMaterial { base_color: Color::srgb(0.06, 0.06, 0.06), perceptual_roughness: 0.95, ..default() });
    let rim = materials.add(StandardMaterial { base_color: Color::srgb(0.75, 0.75, 0.78), metallic: 0.9, perceptual_roughness: 0.3, ..default() });

    let tube = |len: f32| Cuboid::new(0.07, len, 0.07);
    commands
        .spawn((Chassis, Transform::default(), Visibility::default()))
        .with_children(|b| {
            // floor pan + nose + engine box
            b.spawn((Mesh3d(meshes.add(Cuboid::new(1.5, 0.18, 3.0))), MeshMaterial3d(paint.clone()), Transform::from_xyz(0.0, -0.1, 0.0)));
            b.spawn((Mesh3d(meshes.add(Cuboid::new(1.3, 0.3, 0.7))), MeshMaterial3d(paint.clone()),
                Transform::from_xyz(0.0, 0.08, -1.25).with_rotation(Quat::from_rotation_x(-0.25))));
            b.spawn((Mesh3d(meshes.add(Cuboid::new(1.1, 0.5, 0.8))), MeshMaterial3d(steel.clone()), Transform::from_xyz(0.0, 0.2, 1.15)));
            // seats
            for x in [-0.35, 0.35] {
                b.spawn((Mesh3d(meshes.add(Cuboid::new(0.5, 0.12, 0.55))), MeshMaterial3d(seat.clone()), Transform::from_xyz(x, 0.05, 0.25)));
                b.spawn((Mesh3d(meshes.add(Cuboid::new(0.5, 0.7, 0.12))), MeshMaterial3d(seat.clone()),
                    Transform::from_xyz(x, 0.4, 0.55).with_rotation(Quat::from_rotation_x(0.15))));
            }
            // roll cage: four posts and a roof frame
            for (x, z) in [(-0.68, -0.35), (0.68, -0.35), (-0.68, 0.75), (0.68, 0.75)] {
                b.spawn((Mesh3d(meshes.add(tube(1.15))), MeshMaterial3d(steel.clone()), Transform::from_xyz(x, 0.55, z)));
            }
            for z in [-0.35, 0.75] {
                b.spawn((Mesh3d(meshes.add(Cuboid::new(1.43, 0.07, 0.07))), MeshMaterial3d(steel.clone()), Transform::from_xyz(0.0, 1.12, z)));
            }
            for x in [-0.68, 0.68] {
                b.spawn((Mesh3d(meshes.add(Cuboid::new(0.07, 0.07, 1.17))), MeshMaterial3d(steel.clone()), Transform::from_xyz(x, 1.12, 0.2)));
            }
            // light bar
            b.spawn((Mesh3d(meshes.add(Cuboid::new(1.0, 0.1, 0.1))), MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::WHITE, emissive: LinearRgba::rgb(8.0, 7.5, 6.0), ..default() })),
                Transform::from_xyz(0.0, 1.2, -0.36)));
        });
    for i in 0..4 {
        commands
            .spawn((Wheel(i), Transform::default(), Visibility::default()))
            .with_children(|w| {
                let lay = Transform::from_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
                w.spawn((Mesh3d(meshes.add(Cylinder::new(WHEEL_R, 0.38))), MeshMaterial3d(tire.clone()), lay));
                w.spawn((Mesh3d(meshes.add(Cylinder::new(WHEEL_R * 0.55, 0.4))), MeshMaterial3d(rim.clone()), lay));
                // tread blocks so you can see the wheels turn
                for k in 0..10 {
                    let a = k as f32 / 10.0 * std::f32::consts::TAU;
                    w.spawn((Mesh3d(meshes.add(Cuboid::new(0.4, 0.06, 0.1))), MeshMaterial3d(tire.clone()),
                        Transform::from_rotation(Quat::from_rotation_x(a)).with_translation(Quat::from_rotation_x(a) * Vec3::Y * WHEEL_R)));
                }
            });
    }
}

pub fn sync_visuals(buggy: Res<Buggy>, mut chassis: Query<&mut Transform, (With<Chassis>, Without<Wheel>)>, mut wheels: Query<(&Wheel, &mut Transform), Without<Chassis>>) {
    if let Ok(mut t) = chassis.single_mut() {
        *t = Transform::from_translation(buggy.pos - buggy.rot * COM).with_rotation(buggy.rot);
    }
    for (w, mut t) in &mut wheels {
        let i = w.0;
        let off = WHEELS[i] - COM - Vec3::Y * buggy.wheel_len[i];
        let steer = if i < 2 { buggy.steer } else { 0.0 };
        t.translation = buggy.pos + buggy.rot * off;
        t.rotation = buggy.rot * Quat::from_rotation_y(-steer) * Quat::from_rotation_x(-buggy.wheel_spin[i]);
    }
}
