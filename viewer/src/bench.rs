//! Headless driving-quality benchmark (`--bench`, native only).
//!
//! Loads a site's heightfield (no window, textures or meshes), then drives the buggy at the
//! game's fixed 120 Hz step with scripted inputs, in both modes, for a few steering patterns, and
//! prints one JSON object per run (mode x pattern) on stdout. `tools/drive_bench.py` runs it over
//! several sites and tabulates the results.
//!
//! Metrics per run:
//! * `all4` - share of physics substeps with all four wheels on the ground
//! * `air_per_km` - unwanted airborne events per km (nothing touching for > 0.1 s; the bench
//!   never jumps), `bigair_per_km` the ones over 0.5 s
//! * `vacc_rms` - RMS of the vertical (chassis-up) specific force minus its 0.5 s moving mean,
//!   m/s^2: what a seat accelerometer would call ride harshness
//! * `jerk_rms` - RMS time derivative of that signal, m/s^3
//! * `pr_rate_rms` - RMS pitch + roll rate, deg/s (chassis wobble)
//! * `rollovers`, `respawns`, `nan` (respawns because the state blew up)
//! * `stuck` - share of time with throttle on and speed < 2 km/h
//! * `avg_kmh`, `km`
//! * steering (pattern `step` only): `yaw_t63` - seconds from the steer step until the yaw rate
//!   reaches 63 % of its settled value, `yaw_rate` settled yaw rate (deg/s) and `yaw_overshoot`
//! * `wander` (pattern `straight`): RMS yaw rate with the wheel centred, deg/s
//! * slide (grounded, > 3 m/s): `slip_deg` mean body sideslip |angle between heading and
//!   velocity|, `vlat` mean lateral slide speed (m/s), `slide10` share of time sliding > 10 deg
//! * `us_per_step` - wall time per 120 Hz step

use crate::buggy::{Buggy, Controls, Mode, Tuning};
use crate::terrain::{LoadOptions, Progress, Source};
use bevy::prelude::*;

const DT: f32 = 1.0 / 120.0;

#[derive(Clone, Copy)]
enum Steer {
    /// autodrive's slow sinusoid: steer = amp * sin(0.3 t)
    Weave(f32),
    /// keyboard slalom: full lock left / right, switching every `half` seconds
    Bang(f32),
    /// straight until t0, then full right lock
    Step(f32),
    Straight,
}

struct Pattern {
    name: &'static str,
    heading: f32,
    throttle: f32,
    steer: Steer,
}

const PATTERNS: [Pattern; 17] = [
    Pattern { name: "weave_n", heading: 0.0, throttle: 1.0, steer: Steer::Weave(0.35) },
    Pattern { name: "weave_e", heading: 90.0, throttle: 1.0, steer: Steer::Weave(0.35) },
    Pattern { name: "weave_s", heading: 180.0, throttle: 1.0, steer: Steer::Weave(0.35) },
    Pattern { name: "weave_w", heading: 270.0, throttle: 1.0, steer: Steer::Weave(0.35) },
    Pattern { name: "straight", heading: 225.0, throttle: 1.0, steer: Steer::Straight },
    Pattern { name: "straight2", heading: 30.0, throttle: 1.0, steer: Steer::Straight },
    Pattern { name: "slalom", heading: 135.0, throttle: 1.0, steer: Steer::Bang(1.5) },
    Pattern { name: "slalom2", heading: 300.0, throttle: 1.0, steer: Steer::Bang(1.0) },
    Pattern { name: "step", heading: 315.0, throttle: 0.6, steer: Steer::Step(8.0) },
    Pattern { name: "crawl", heading: 45.0, throttle: 0.3, steer: Steer::Weave(0.5) },
    Pattern { name: "crawl2", heading: 200.0, throttle: 0.3, steer: Steer::Weave(0.5) },
    Pattern { name: "half", heading: 160.0, throttle: 0.5, steer: Steer::Weave(0.35) },
    // cruise control at 30 km/h (throttle < 0 means "hold this speed"): ride comparisons at an
    // equal speed, whatever the engine and grip
    Pattern { name: "cruise_a", heading: 20.0, throttle: -30.0, steer: Steer::Weave(0.25) },
    Pattern { name: "cruise_b", heading: 110.0, throttle: -30.0, steer: Steer::Weave(0.25) },
    Pattern { name: "cruise_c", heading: 250.0, throttle: -30.0, steer: Steer::Straight },
    // parked where it spawned (no throttle): creep and jitter at rest
    Pattern { name: "park", heading: 70.0, throttle: 0.0, steer: Steer::Straight },
    // side-slope test on the ramp site: north along a strip at half throttle
    Pattern { name: "ramp", heading: 0.0, throttle: 0.5, steer: Steer::Straight },
];

/// `patterns`: comma list of pattern names to run (default: all but the `ramp_*` ones, which
/// are for the `tools/make_ramp_site.py` site with `--spawn`).
pub fn run(
    site_dir: &str,
    opts: LoadOptions,
    seconds: f32,
    gravity_mult: f32,
    only: Option<Mode>,
    spawn: Option<Vec2>,
    patterns: Option<&str>,
) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    let progress = Progress::default();
    let mut site = match site_dir.strip_prefix("synthetic:") {
        Some(kind) => crate::terrain::site_from_grid(synthetic(kind)?, opts, &progress).0,
        None => bevy::tasks::block_on(crate::terrain::load_site(&Source::new(site_dir), opts, &progress))?,
    };
    if let Some(s) = spawn {
        if let Some(h) = site.height(s.x, s.y) {
            site.spawn = Vec3::new(s.x, h, s.y);
        }
    }
    eprintln!("bench: loaded {} in {:.1}s, spawn ({:.1}, {:.1}, {:.1})", site.name, t0.elapsed().as_secs_f32(), site.spawn.x, site.spawn.y, site.spawn.z);
    for mode in [Mode::Fun, Mode::Real] {
        if only.is_some_and(|m| m != mode) {
            continue;
        }
        for p in &PATTERNS {
            let want = match patterns {
                Some(list) => list.split(',').any(|n| n.trim() == p.name),
                None => !p.name.starts_with("ramp"),
            };
            if !want {
                continue;
            }
            let r = run_one(&site, mode, gravity_mult, p, seconds);
            println!("{r}");
        }
    }
    Ok(())
}

fn run_one(site: &crate::terrain::Site, mode: Mode, gravity_mult: f32, p: &Pattern, seconds: f32) -> serde_json::Value {
    let tuning = Tuning::new(mode, gravity_mult);
    let g = tuning.gravity();
    let mut b = Buggy::new(site, site.spawn, -p.heading.to_radians(), tuning);
    let mut ctl = Controls::default();
    let n = (seconds / DT) as usize;
    let (mut frames, mut all4, mut dist) = (0.0f64, 0.0f64, 0.0f64);
    let (mut air_run, mut air_events, mut big_air) = (0usize, 0u32, 0u32);
    let (mut vacc2, mut jerk2, mut pr2, mut nacc) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut stuck = 0.0f64;
    let (mut wander2, mut nwander) = (0.0f64, 0.0f64);
    // slide: body sideslip angle (heading vs velocity) and lateral slide speed while driving
    let (mut beta_sum, mut vlat_sum, mut nslide, mut slide_big) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut fu_mean = g;
    let mut fu_prev_hp: Option<f32> = None;
    let mut vel_prev = b.vel;
    let mut respawns0 = b.respawns;
    let mut settle = 1.0f32; // seconds to ignore after a (re)spawn
    let mut nan = 0;
    let mut upside = 0.0f32;
    let mut yaw_trace: Vec<(f32, f32)> = Vec::new();
    let mut max_ang = 0.0f32;
    let verbose = std::env::var("MM_BENCH_VERBOSE").is_ok();
    let mut rolls0 = 0;
    let mut hist: std::collections::VecDeque<String> = Default::default();
    let wall = std::time::Instant::now();
    for i in 0..n {
        let t = i as f32 * DT;
        ctl.throttle = if p.throttle < 0.0 {
            let v_long = b.vel.dot(b.forward()) * 3.6;
            ((-p.throttle - v_long) * 0.15).clamp(-0.3, 1.0)
        } else {
            p.throttle
        };
        ctl.steer = match p.steer {
            Steer::Weave(a) => a * (t * 0.3).sin(),
            Steer::Bang(half) => if t < 3.0 { 0.0 } else if ((t - 3.0) / half) as i64 % 2 == 0 { 1.0 } else { -1.0 },
            Steer::Step(t0) => if t < t0 { 0.0 } else { 1.0 },
            Steer::Straight => 0.0,
        };
        // the same auto-reset as --autodrive: on the roof for 3 s -> flip upright
        if (b.rot * Vec3::Y).y < 0.3 {
            upside += DT;
            if upside > 3.0 {
                b.reset(site);
                upside = 0.0;
            }
        } else {
            upside = 0.0;
        }
        b.step(site, &ctl, DT);
        if b.respawns != respawns0 {
            if b.last_respawn == "physics blew up" {
                nan += 1;
            }
            respawns0 = b.respawns;
            settle = 1.0;
            fu_prev_hp = None;
            vel_prev = b.vel;
            air_run = 0;
            continue;
        }
        let speed = b.vel.length();
        let up = b.rot * Vec3::Y;
        if verbose {
            if i % 12 == 0 || b.ang.length() > 12.0 {
                let wl = b.rot.inverse() * b.ang;
                hist.push_back(format!(
                    "t={t:.1} v={:.1} wheels={} air={:.2} up_y={:.2} side={:.0} pitch_r={:.1} roll_r={:.1} steer={:.2} yaw_r={:.1} hag={:.2}",
                    speed * 3.6, b.grounded, b.air_t, up.y, b.side_slope, wl.x, wl.z, b.steer, wl.y,
                    b.pos.y - site.height(b.pos.x, b.pos.z).unwrap_or(f32::NAN)
                ));
                if hist.len() > 25 {
                    hist.pop_front();
                }
            }
            let spin = b.ang.length() > 25.0 && !hist.is_empty();
            if b.rollovers != rolls0 || spin {
                rolls0 = b.rollovers;
                eprintln!("{} {} {} {}:", if spin { "SPIN" } else { "ROLLOVER" }, site.name, mode.short(), p.name);
                for l in &hist {
                    eprintln!("    {l}");
                }
                hist.clear();
            }
        }
        let acc = (b.vel - vel_prev) / DT;
        vel_prev = b.vel;
        max_ang = max_ang.max(b.ang.length());
        if settle > 0.0 {
            settle -= DT;
            fu_mean = g;
            continue;
        }
        frames += 1.0;
        all4 += b.ground4 as f64;
        dist += Vec3::new(b.vel.x, 0.0, b.vel.z).length() as f64 * DT as f64;
        if b.airborne() {
            air_run += 1;
        } else {
            let secs = air_run as f32 * DT;
            if secs > 0.1 {
                air_events += 1;
            }
            if secs > 0.5 {
                big_air += 1;
            }
            air_run = 0;
        }
        // seat accelerometer: specific force along chassis up, high-passed by a 0.5 s EMA
        let fu = (acc + Vec3::Y * g).dot(up);
        fu_mean += (fu - fu_mean) * (DT / 0.5);
        let hp = fu - fu_mean;
        vacc2 += (hp * hp) as f64;
        if let Some(prev) = fu_prev_hp {
            let j = (hp - prev) / DT;
            jerk2 += (j * j) as f64;
        }
        fu_prev_hp = Some(hp);
        let wl = b.rot.inverse() * b.ang;
        pr2 += (wl.x * wl.x + wl.z * wl.z) as f64;
        nacc += 1.0;
        if ctl.throttle > 0.0 && p.throttle > 0.0 && speed < 2.0 / 3.6 {
            stuck += 1.0;
        }
        let yaw_rate = b.ang.dot(up);
        if speed > 3.0 && b.grounded >= 3 {
            let f = b.forward();
            let r = b.rot * Vec3::X;
            let (vf, vr) = (b.vel.dot(f), b.vel.dot(r));
            let beta = vr.atan2(vf.abs()).abs();
            beta_sum += beta as f64;
            vlat_sum += vr.abs() as f64;
            nslide += 1.0;
            slide_big += (beta > 10f32.to_radians()) as u8 as f64;
        }
        if matches!(p.steer, Steer::Straight) && speed > 1.5 && b.grounded >= 3 {
            wander2 += (yaw_rate * yaw_rate) as f64;
            nwander += 1.0;
        }
        if let Steer::Step(t0) = p.steer {
            if t >= t0 - 0.5 && t <= t0 + 5.0 {
                yaw_trace.push((t - t0, yaw_rate));
            }
        }
    }
    let us = wall.elapsed().as_secs_f64() * 1e6 / n as f64;
    let km = dist / 1000.0;
    let rms = |s: f64, n: f64| if n > 0.0 { (s / n).sqrt() } else { 0.0 };
    let mut v = serde_json::json!({
        "site": site.name, "mode": mode.short(), "pattern": p.name,
        "secs": frames * DT as f64, "km": km,
        "avg_kmh": if frames > 0.0 { dist / (frames * DT as f64) * 3.6 } else { 0.0 },
        "all4": if frames > 0.0 { all4 / frames } else { 0.0 },
        "air_events": air_events, "air_per_km": if km > 0.01 { air_events as f64 / km } else { 0.0 },
        "bigair_per_km": if km > 0.01 { big_air as f64 / km } else { 0.0 },
        "vacc_rms": rms(vacc2, nacc), "jerk_rms": rms(jerk2, nacc),
        "pr_rate_rms": rms(pr2, nacc).to_degrees(),
        "rollovers": b.rollovers, "respawns": b.respawns, "nan": nan, "autoflips": b.autoflips,
        "stuck": if frames > 0.0 { stuck / frames } else { 0.0 },
        "max_ang": max_ang,
        "slip_deg": if nslide > 0.0 { (beta_sum / nslide).to_degrees() } else { 0.0 },
        "vlat": if nslide > 0.0 { vlat_sum / nslide } else { 0.0 },
        "slide10": if nslide > 0.0 { slide_big / nslide } else { 0.0 },
        "slide_secs": nslide * DT as f64,
        "us_per_step": us,
    });
    if nwander > 0.0 {
        v["wander"] = serde_json::json!(rms(wander2, nwander).to_degrees());
    }
    if !yaw_trace.is_empty() {
        // settled yaw rate: mean over 2..5 s after the step
        let settled: Vec<f32> = yaw_trace.iter().filter(|(t, _)| *t >= 2.0).map(|x| x.1).collect();
        let ss = settled.iter().sum::<f32>() / settled.len().max(1) as f32;
        let t63 = yaw_trace.iter().find(|(t, w)| *t >= 0.0 && w / ss.signum() >= 0.63 * ss.abs()).map(|x| x.0);
        let peak = yaw_trace.iter().filter(|(t, _)| *t >= 0.0).map(|x| x.1 * ss.signum()).fold(0.0f32, f32::max);
        v["yaw_rate"] = serde_json::json!(ss.abs().to_degrees());
        v["yaw_t63"] = serde_json::json!(t63);
        v["yaw_overshoot"] = serde_json::json!(if ss.abs() > 1e-3 { peak / ss.abs() - 1.0 } else { 0.0 });
    }
    v
}

/// In-memory test terrains, 2 x 2 km at 1 m posts: `flat` (a plane: steering and grip tests
/// without terrain noise) and `noise` (gentle 20-60 m swells plus DTM-like 1-post noise of
/// 0.12 m RMS and a sprinkling of 0.5-1.5 m single-post spikes, like stereo-matching blunders).
fn synthetic(kind: &str) -> Result<crate::terrain::Grid, String> {
    let n = 2017usize;
    let mut z = vec![0.0f32; n * n];
    let mut seed = 12345u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f32 / (1u64 << 53) as f32
    };
    match kind {
        "flat" => {}
        "noise" => {
            for r in 0..n {
                for c in 0..n {
                    let (x, y) = (c as f32, r as f32);
                    let swell = 1.5 * (x / 9.0).sin() * (y / 13.0).cos() + 0.8 * ((x + y) / 5.0).sin();
                    // sum of 4 uniforms ~ Gaussian, scaled to 0.12 m RMS
                    let g = (rnd() + rnd() + rnd() + rnd() - 2.0) * (0.12 / 0.577);
                    let spike = if rnd() < 0.002 { (0.5 + rnd()) * if rnd() < 0.5 { 1.0 } else { -1.0 } } else { 0.0 };
                    z[r * n + c] = 10.0 + swell + g + spike;
                }
            }
        }
        k => return Err(format!("unknown synthetic terrain {k:?} (flat, noise)")),
    }
    Ok(crate::terrain::Grid {
        name: format!("synthetic_{kind}"),
        rationale: String::new(),
        lat: 0.0,
        lon: 0.0,
        base_m: 0.0,
        mpp: 1.0,
        east0: -1008.0,
        north0: 1008.0,
        w: n,
        h: n,
        z,
        quantum: 0.001,
        sun: None,
        tile_step: n - 1,
        tiles: Vec::new(),
        texture_kind: "none",
    })
}
