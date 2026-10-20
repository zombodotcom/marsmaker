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
const BRAKE: f32 = 3500.0;
const STEER_MAX: f32 = 0.55;
const SUBSTEPS: usize = 4;
/// seconds the chassis centre may hang over nodata before we respawn
const OFF_DATA_GRACE: f32 = 0.4;
/// how often a "known good" pose is recorded, and how many are kept
const SAFE_EVERY: f32 = 0.25;
const SAFE_KEEP: usize = 40;

// --- JUST FOR FUN extras (ignored in REAL mode) ---
/// boost: rocket-style thrust along the chassis nose, tapering to zero at BOOST_VMAX
const BOOST_FORCE: f32 = 5000.0;
const BOOST_VMAX: f32 = 75.0;
const BOOST_SECS: f32 = 4.0; // a full meter lasts this long
const BOOST_RECHARGE_SECS: f32 = 5.0; // empty -> full
const BOOST_RECHARGE_DELAY: f32 = 0.6; // after letting go
/// jump: velocity kick (m/s). Under Mars gravity 6 m/s is ~4.8 m high and ~3.2 s of air.
const JUMP_V: f32 = 6.0;
const JUMP_COOLDOWN: f32 = 0.6;
const JUMP_COYOTE: f32 = 0.15; // may still jump this long after the wheels left the ground
/// air control: target pitch / roll rates (rad/s) and how fast they're reached (1/s)
const AIR_PITCH_RATE: f32 = 3.0;
const AIR_ROLL_RATE: f32 = 3.5;
const AIR_GAIN: f32 = 4.0;
const AIR_CONTROL_AFTER: f32 = 0.12; // s airborne before air control engages (not on every bump)
/// soft landings: self-righting toward the ground normal (angular accel per unit sin(tilt)),
/// strong for LAND_WINDOW s after touching down, gentle otherwise. Not applied beyond
/// LAND_EXTREME (chassis up . ground normal), i.e. a roof / side landing still tips over.
const RIGHT_LANDING: f32 = 20.0;
const RIGHT_DRIVING: f32 = 4.0;
const LAND_WINDOW: f32 = 0.7;
const LAND_EXTREME: f32 = 0.25;
/// auto-flip upright after this long on the roof / side
const AUTO_FLIP_SECS: f32 = 1.5;
/// airborne longer than this counts as a jump in the stunt stats
const STUNT_MIN_AIR: f32 = 0.35;

/// The two driving modes. Toggle in game with M, or `--mode real|fun` (`?mode=` in the browser).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Mars as it is: loose-sand tires, a light sand rail's power (~60 km/h), no assists.
    Real,
    /// Ignores realism where that's more fun: grip, power, boost, jump, air control, auto-flip.
    #[default]
    Fun,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "real" | "realistic" => Some(Mode::Real),
            "fun" | "arcade" => Some(Mode::Fun),
            _ => None,
        }
    }
    pub fn other(self) -> Self {
        match self {
            Mode::Real => Mode::Fun,
            Mode::Fun => Mode::Real,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Real => "REAL MARS",
            Mode::Fun => "JUST FOR FUN",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Mode::Real => "real",
            Mode::Fun => "fun",
        }
    }
}

/// Tire / ground / engine model per mode.
///
/// * REAL (`--mode real`, `--realistic`): loose dry sand. A friction *ellipse*: longitudinal
///   peak mu 0.48 (tire-on-loose-dry-sand traction is typically ~0.4-0.6) and lateral 0.58,
///   since a tire pushed sideways bulldozes a sand berm that resists the slide. Once the demanded
///   force exceeds the ellipse (wheelspin / sliding), grip falls off with the overload, up to 30%
///   less at 2x, like a tire digging a rut instead of rolling. Rolling resistance adds a
///   load-proportional term (Crr 0.12, soft-sand sinkage) and cornering stiffness is lower, so
///   the buggy understeers, climbs noticeably slower, and slides down side slopes beyond ~28 deg
///   (or earlier under heavy throttle, when the drive force uses up the grip). 2600 N at the
///   wheels against that drag gives a light sand rail's ~60 km/h on the flat. No assists.
/// * FUN (default): a grippy friction circle (mu 1.1), stiff cornering, 4200 N and light drag
///   (~117 km/h on the flat, ~200+ with boost), plus boost, jump, air control, soft landings and
///   auto-flip (see the constants above).
///
/// Gravity is Mars' 3.711 m/s^2 in both, times `--gravity-mult`.
#[derive(Clone, Copy)]
pub struct Tuning {
    pub mode: Mode,
    pub gravity_mult: f32,
    mu: f32,     // longitudinal peak friction coefficient
    mu_lat: f32, // lateral peak friction coefficient
    corner: f32,       // lateral tire stiffness N/(m/s), whole car
    roll_visc: f32,    // N/(m/s) per wheel
    roll_crr: f32,     // load-proportional rolling resistance coefficient
    slip_falloff: f32, // fraction of grip lost at full slip
    engine: f32,       // N at the wheels, all-wheel drive
    vmax: f32,         // m/s where engine force tapers to zero
}

impl Tuning {
    pub fn new(mode: Mode, gravity_mult: f32) -> Self {
        let gravity_mult = if gravity_mult.is_finite() && gravity_mult > 0.0 { gravity_mult } else { 1.0 };
        match mode {
            Mode::Real => Self {
                mode,
                gravity_mult,
                mu: 0.48,
                mu_lat: 0.58,
                corner: 4500.0,
                roll_visc: 12.0,
                roll_crr: 0.12,
                slip_falloff: 0.3,
                engine: 2600.0,
                vmax: 26.0,
            },
            Mode::Fun => Self {
                mode,
                gravity_mult,
                mu: 1.1,
                mu_lat: 1.1,
                corner: 9000.0,
                roll_visc: 6.0,
                roll_crr: 0.0,
                slip_falloff: 0.0,
                engine: 4200.0,
                vmax: 40.0,
            },
        }
    }
    pub fn fun(&self) -> bool {
        self.mode == Mode::Fun
    }
    pub fn gravity(&self) -> f32 {
        MARS_G * self.gravity_mult
    }
}

/// Scripted input for tests: `--script jump@3,boost@4-6,mode@8`. Each item is `NAME@T` (a
/// one-shot press at T s) or `NAME@T0-T1` (held from T0 to T1). Held items override the keyboard
/// and `--autodrive`. Names: `jump`, `mode`, `reset`, `upend` (turn the buggy onto its roof, to
/// test auto-flip) as presses; `boost`, `fwd`, `back`, `coast` (throttle 0), `left`, `right`,
/// `straight`, `brake` as holds.
#[derive(Clone, Default, Debug)]
pub struct Script(Vec<(String, f32, Option<f32>)>);

impl Script {
    pub fn parse(s: &str) -> Self {
        let mut v = Vec::new();
        for item in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (name, when) = item.split_once('@').unwrap_or_else(|| panic!("--script item {item:?} needs NAME@T"));
            let num = |t: &str| t.trim().parse::<f32>().unwrap_or_else(|_| panic!("--script item {item:?}: bad time"));
            let (t0, t1) = match when.split_once('-') {
                Some((a, b)) => (num(a), Some(num(b))),
                None => (num(when), None),
            };
            let name = name.trim().to_ascii_lowercase();
            const NAMES: [&str; 12] = ["jump", "mode", "reset", "upend", "boost", "fwd", "back", "coast", "left", "right", "straight", "brake"];
            assert!(NAMES.contains(&name.as_str()), "--script: unknown item {name:?} (one of {NAMES:?})");
            v.push((name, t0, t1));
        }
        Script(v)
    }

    /// Apply the script for the frame covering (prev, now] seconds.
    pub fn apply(&self, prev: f32, now: f32, ctl: &mut Controls) {
        for (name, t0, t1) in &self.0 {
            let fire = prev < *t0 && now >= *t0;
            let held = match t1 {
                Some(t1) => now >= *t0 && now <= *t1,
                None => false,
            };
            match name.as_str() {
                "jump" if fire => ctl.jump = true,
                "mode" if fire => ctl.toggle_mode = true,
                "reset" if fire => ctl.reset = true,
                "upend" if fire => ctl.upend = true,
                "boost" if held || fire => ctl.boost = true,
                "fwd" if held => ctl.throttle = 1.0,
                "back" if held => ctl.throttle = -1.0,
                "coast" if held => ctl.throttle = 0.0,
                "left" if held => ctl.steer = -1.0,
                "right" if held => ctl.steer = 1.0,
                "straight" if held => ctl.steer = 0.0,
                "brake" if held => ctl.brake = true,
                _ => {}
            }
        }
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
    /// FUN mode: held boost
    pub boost: bool,
    /// latched presses, cleared by the physics step
    pub jump: bool,
    pub toggle_mode: bool,
    /// test hook: turn the buggy onto its roof
    pub upend: bool,
}

/// Stunt stats of one jump: airtime (s), max height of the chassis above the ground (m), flips.
#[derive(Clone, Copy, Default)]
pub struct Jump {
    pub air: f32,
    pub height: f32,
    pub flips: u32,
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
    /// wheel or chassis touched the ground in the last step
    touching: bool,
    body_contact: bool,
    since_touch: f32,
    // --- FUN mode state (stats are tracked in both modes) ---
    /// boost meter 0..1
    pub boost: f32,
    pub boosting: bool,
    boost_idle: f32,
    jump_cd: f32,
    pub jumps: u32,
    /// current airborne time / max height / integrated pitch and roll (rad) of the current jump
    pub air_t: f32,
    air_h: f32,
    air_pitch: f32,
    air_roll: f32,
    /// throttle / steer key direction at takeoff, and whether each has since been released or
    /// changed (air control needs a fresh press, so holding W over a crest doesn't flip you)
    air_held: [i8; 2],
    air_armed: [bool; 2],
    land_t: f32,
    pub last_jump: Option<Jump>,
    pub best: Jump,
    /// total flips landed
    pub flips: u32,
    upside_t: f32,
    pub autoflips: u32,
    /// on-screen message and how long it stays
    pub msg: String,
    pub msg_t: f32,
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
            touching: false,
            body_contact: false,
            since_touch: 0.0,
            boost: 1.0,
            boosting: false,
            boost_idle: 0.0,
            jump_cd: 0.0,
            jumps: 0,
            air_t: 0.0,
            air_h: 0.0,
            air_pitch: 0.0,
            air_roll: 0.0,
            air_held: [0; 2],
            air_armed: [false; 2],
            land_t: 0.0,
            last_jump: None,
            best: Jump::default(),
            flips: 0,
            upside_t: 0.0,
            autoflips: 0,
            msg: String::new(),
            msg_t: 0.0,
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
        // a teleport isn't a jump
        self.air_t = 0.0;
        self.air_h = 0.0;
        self.air_pitch = 0.0;
        self.air_roll = 0.0;
        self.land_t = 0.0;
        self.upside_t = 0.0;
        self.since_touch = 0.0;
    }

    fn say(&mut self, msg: impl Into<String>, secs: f32) {
        self.msg = msg.into();
        self.msg_t = secs;
    }

    /// M key: switch mode live (keeps position and velocity).
    pub fn set_mode(&mut self, mode: Mode) {
        self.tuning = Tuning::new(mode, self.tuning.gravity_mult);
        self.boosting = false;
        let hint = match mode {
            Mode::Real => "loose sand, real power, no assists",
            Mode::Fun => "Shift boost, Ctrl/J jump, air control",
        };
        self.say(format!("{} MODE  ({hint})", mode.label()), 2.5);
        info!("MODE -> {} at ({:.1}, {:.1}, {:.1}) speed {:.1} km/h", mode.label(), self.pos.x, self.pos.y, self.pos.z, self.vel.length() * 3.6);
    }

    /// Test hook: turn the buggy onto its roof where it is.
    pub fn upend(&mut self, site: &Site) {
        let g = site.height(self.pos.x, self.pos.z).unwrap_or(self.pos.y);
        self.rot = Quat::from_rotation_y(self.yaw()) * Quat::from_rotation_z(std::f32::consts::PI);
        self.pos = Vec3::new(self.pos.x, g + 1.2, self.pos.z);
        self.vel = Vec3::ZERO;
        self.ang = Vec3::ZERO;
        info!("UPEND (test hook) at ({:.1}, {:.1}, {:.1})", self.pos.x, self.pos.y, self.pos.z);
    }

    /// True while neither wheels nor chassis touch the ground.
    pub fn airborne(&self) -> bool {
        !self.touching
    }

    /// Flips in the current jump so far: full turns of pitch plus full turns of roll (30 deg
    /// short of a turn still counts).
    fn air_flips(&self) -> u32 {
        let n = |a: f32| ((a.abs() + 0.5) / std::f32::consts::TAU).floor() as u32;
        n(self.air_pitch) + n(self.air_roll)
    }

    /// Mode tag + status line for the HUD (stunt stats in FUN mode).
    pub fn hud_status(&self) -> String {
        let mut s = format!("[{}]", self.tuning.mode.label());
        if self.tuning.gravity_mult != 1.0 {
            s += &format!(" g x{:.2}", self.tuning.gravity_mult);
        }
        if self.tuning.fun() {
            let n = (self.boost * 10.0).round() as usize;
            s += &format!(
                "\nBOOST [{}{}]{}",
                "#".repeat(n),
                "-".repeat(10 - n.min(10)),
                if self.boosting { " >>>" } else { "" }
            );
            if self.air_t > STUNT_MIN_AIR {
                s += &format!("   AIR {:.1} s   height {:.1} m   flips {}", self.air_t, self.air_h, self.air_flips());
            } else if let Some(j) = self.last_jump {
                s += &format!("   last jump: {:.1} s, {:.1} m, {} flips", j.air, j.height, j.flips);
            }
            s += &format!(
                "\nbest air {:.1} s   best height {:.1} m   total flips {}   jumps {}",
                self.best.air, self.best.height, self.flips, self.jumps
            );
        }
        if self.msg_t > 0.0 {
            s += &format!("\n\n>>> {} <<<", self.msg);
        }
        s
    }

    /// Control help line for the HUD, per mode.
    pub fn hud_help(&self) -> &'static str {
        match self.tuning.mode {
            Mode::Real => "W/S throttle  A/D steer  Space brake  R flip upright  M fun mode  C camera  Esc quit",
            Mode::Fun => {
                "W/S throttle  A/D steer  Space brake  Shift boost  Ctrl/J jump  air: W/S pitch A/D roll  R flip  M real  C cam  Esc"
            }
        }
    }

    /// Extra fields for the periodic physics log.
    pub fn log_extra(&self) -> String {
        format!(
            "mode={} g={:.3} boost={:.2}{} air={:.1}s jumps={} flips={} best_air={:.1}s best_h={:.1}m autoflips={}",
            self.tuning.mode.short(),
            self.tuning.gravity(),
            self.boost,
            if self.boosting { "(on)" } else { "" },
            self.air_t,
            self.jumps,
            self.flips,
            self.best.air,
            self.best.height,
            self.autoflips
        )
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
        self.msg_t = (self.msg_t - dt).max(0.0);
        let fun = self.tuning.fun();

        // boost meter
        self.boosting = fun && ctl.boost && self.boost > 0.0;
        if self.boosting {
            self.boost = (self.boost - dt / BOOST_SECS).max(0.0);
            self.boost_idle = 0.0;
        } else {
            self.boost_idle += dt;
            if self.boost_idle > BOOST_RECHARGE_DELAY {
                self.boost = (self.boost + dt / BOOST_RECHARGE_SECS).min(1.0);
            }
        }
        // jump: needs (recent) ground contact
        self.jump_cd -= dt;
        if fun && ctl.jump && self.jump_cd <= 0.0 && self.since_touch <= JUMP_COYOTE {
            let up = self.rot * Vec3::Y;
            let dir = (up + Vec3::Y).normalize_or(Vec3::Y);
            self.vel += dir * JUMP_V;
            self.jump_cd = JUMP_COOLDOWN;
            self.jumps += 1;
            info!("JUMP #{} at ({:.1}, {:.1}, {:.1}) speed {:.1} km/h", self.jumps, self.pos.x, self.pos.y, self.pos.z, self.vel.length() * 3.6);
        }

        let dir = |v: f32| if v > 0.05 { 1i8 } else if v < -0.05 { -1 } else { 0 };
        let keys = [dir(ctl.throttle), dir(ctl.steer)];
        for i in 0..2 {
            if self.touching {
                self.air_held[i] = keys[i];
                self.air_armed[i] = false;
            } else if keys[i] != self.air_held[i] {
                self.air_armed[i] = true;
            }
        }
        self.touching = false;
        for _ in 0..SUBSTEPS {
            self.substep(site, ctl, h);
            self.touching |= self.grounded > 0 || self.body_contact;
        }
        self.check_bounds(site, dt);
        self.track_slope(site, dt);
        self.track_stunts(site, dt);
        if fun {
            self.auto_flip(site, dt);
        } else {
            self.upside_t = 0.0;
        }
    }

    /// Airtime / height / flips. A jump starts when nothing touches the ground and is counted
    /// on landing if it lasted more than STUNT_MIN_AIR.
    fn track_stunts(&mut self, site: &Site, dt: f32) {
        if !self.touching {
            self.since_touch += dt;
            self.air_t += dt;
            if let Some(g) = site.height(self.pos.x, self.pos.z) {
                // CoM height minus its ride height ~ how far the chassis is off the ground
                self.air_h = self.air_h.max(self.pos.y - g - 0.67);
            }
            let wl = self.rot.inverse() * self.ang;
            self.air_pitch += wl.x * dt;
            self.air_roll += wl.z * dt;
            return;
        }
        self.since_touch = 0.0;
        if self.air_t > STUNT_MIN_AIR {
            let up = self.rot * Vec3::Y;
            let on_wheels = up.dot(site.normal_at(self.pos.x, self.pos.z, 2.0)) > LAND_EXTREME;
            // a flip only counts if you land it
            let flips = if on_wheels { self.air_flips() } else { 0 };
            let j = Jump { air: self.air_t, height: self.air_h.max(0.0), flips };
            self.flips += j.flips;
            self.best.air = self.best.air.max(j.air);
            self.best.height = self.best.height.max(j.height);
            self.best.flips = self.best.flips.max(j.flips);
            self.last_jump = Some(j);
            let up_y = up.y;
            info!(
                "LANDED {} airtime {:.2} s, max height {:.1} m, flips {} (pitch {:.0} deg, roll {:.0} deg), up_y {:.2}, speed {:.1} km/h",
                if on_wheels { "on the wheels" } else { "on the roof/side" },
                j.air,
                j.height,
                j.flips,
                self.air_pitch.to_degrees(),
                self.air_roll.to_degrees(),
                up_y,
                self.vel.length() * 3.6
            );
            if self.tuning.fun() {
                self.land_t = LAND_WINDOW;
                if !on_wheels {
                    self.say("CRASH!", 1.5);
                } else if j.flips > 0 {
                    self.say(format!("{} FLIP{}!", j.flips, if j.flips > 1 { "S" } else { "" }), 2.0);
                } else if j.air > 2.0 {
                    self.say(format!("BIG AIR {:.1} s", j.air), 2.0);
                }
            }
        }
        self.air_t = 0.0;
        self.air_h = 0.0;
        self.air_pitch = 0.0;
        self.air_roll = 0.0;
        self.land_t = (self.land_t - dt).max(0.0);
    }

    /// FUN mode: on the roof or side, nearly stopped, for AUTO_FLIP_SECS -> flip upright.
    fn auto_flip(&mut self, site: &Site, dt: f32) {
        let up_y = (self.rot * Vec3::Y).y;
        if up_y < 0.3 && self.touching && self.vel.length() < 4.0 {
            self.upside_t += dt;
            if self.upside_t > AUTO_FLIP_SECS {
                self.reset(site);
                self.autoflips += 1;
                self.say("AUTO-FLIP", 1.5);
                info!("AUTO-FLIP #{} at ({:.1}, {:.1}, {:.1})", self.autoflips, self.pos.x, self.pos.y, self.pos.z);
            }
        } else {
            self.upside_t = 0.0;
        }
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

    /// FUN mode only. Airborne: W/S pitch and A/D roll steer the spin toward a target rate (no
    /// input keeps the current spin, so flips carry on). Touching: a righting push toward the
    /// ground normal, strong just after a landing (which also soaks up the tumble), gentle while
    /// driving; none past LAND_EXTREME, so a roof landing still ends on the roof (auto-flip).
    fn fun_assists(&mut self, site: &Site, ctl: &Controls, up: Vec3, h: f32) {
        if self.grounded == 0 && !self.body_contact {
            if self.air_t < AIR_CONTROL_AFTER {
                return;
            }
            let mut wl = self.rot.inverse() * self.ang;
            let g = (AIR_GAIN * h).min(1.0);
            if self.air_armed[0] && ctl.throttle != 0.0 {
                wl.x += (-ctl.throttle * AIR_PITCH_RATE - wl.x) * g; // W = nose down
            }
            if self.air_armed[1] && ctl.steer != 0.0 {
                wl.z += (-ctl.steer * AIR_ROLL_RATE - wl.z) * g; // D = roll right
            }
            self.ang = self.rot * wl;
            return;
        }
        let n = site.normal_at(self.pos.x, self.pos.z, 2.0);
        if up.dot(n) < LAND_EXTREME {
            return;
        }
        let landing = self.land_t > 0.0;
        self.ang += up.cross(n) * if landing { RIGHT_LANDING } else { RIGHT_DRIVING } * h;
        if landing {
            let yaw = n * self.ang.dot(n);
            self.ang = yaw + (self.ang - yaw) * (1.0 - 6.0 * h);
        }
    }

    fn substep(&mut self, site: &Site, ctl: &Controls, h: f32) {
        let up = self.rot * Vec3::Y;
        let tu = self.tuning;
        let mut force = Vec3::NEG_Y * tu.gravity() * MASS;
        let mut torque = Vec3::ZERO;
        let p0 = self.pos;
        let apply = |f: Vec3, at: Vec3, force: &mut Vec3, torque: &mut Vec3| {
            *force += f;
            *torque += (at - p0).cross(f);
        };
        if self.boosting {
            // rocket-ish: along the nose, through the CoM, works in the air too
            let fwd = self.forward();
            let taper = (1.0 - self.vel.dot(fwd) / BOOST_VMAX).clamp(0.0, 1.0);
            force += fwd * BOOST_FORCE * taper;
        }
        self.grounded = 0;
        self.body_contact = false;

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
                let taper = (1.0 - v_long.abs() / tu.vmax).max(0.0);
                ctl.throttle * tu.engine / 4.0 * if ctl.throttle * v_long < 0.0 { 1.0 } else { taper }
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
                        self.body_contact = true;
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
        if tu.fun() {
            self.fun_assists(site, ctl, up, h);
        }
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

/// Geometry the visuals (src/visuals.rs) need to draw the buggy where the physics has it.
pub(crate) const VIS_COM: Vec3 = COM;
pub(crate) const VIS_REST: f32 = REST;
pub(crate) const VIS_WHEEL_R: f32 = WHEEL_R;

/// The buggy model lives in src/visuals.rs.
pub fn spawn_visuals(commands: &mut Commands, meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) {
    crate::visuals::spawn_buggy(commands, meshes, materials);
}

/// Chassis / wheel poses from the physics state (see visuals::sync_buggy).
pub use crate::visuals::sync_buggy as sync_visuals;
