//! MarsMaker viewer: drive a dune buggy on real HiRISE terrain at 1:1 scale.
//!
//!   marsmaker-viewer [SITE_DIR] [--step N] [--spawn X,Z] [--heading DEG] [--mode real|fun] [--realistic] [--albedo]
//!                    [--autodrive] [--weave A] [--throttle T] [--seconds N] [--shot OUT.png] [--shot-at T]
//!                    [--log-every T] [--high-cam] [--low] [--cam-zoom Z] [--gravity-mult G] [--script jump@3,boost@4-6,mode@8]
//!   marsmaker-viewer --live SITE_ID [--live-index FILE] [--live-mpp M] [--live-dump OUT]
//!
//! In the browser the same flags come from the URL query, e.g. `?site=NAME&autodrive&albedo`
//! (see WEB.md). SITE_DIR is a converted site folder (with meta.json); default is the Russell
//! Crater megadune. `--live SITE_ID` (browser: `?live=SITE_ID`) streams any HiRISE DTM straight
//! from NASA's public bucket instead (see live.rs and WEB.md). Controls: W/S or arrows = throttle/reverse,
//! A/D = steer, Space = brake, R = flip upright, M = REAL / FUN mode, C = toggle chase / high camera,
//! Esc = quit; FUN mode adds Shift = boost, Ctrl/J = jump, W/S/A/D = pitch/roll in the air. See README.md for the flags.

mod buggy;
mod live;
mod terrain;
mod visuals;

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::tasks::{IoTaskPool, Task, futures::check_ready};
use buggy::{Buggy, Controls, Mode, Script, Tuning};
use std::path::PathBuf;
use terrain::{Loaded, LoadOptions, Progress, Site, Source, TerrainRender};

const DEFAULT_SITE_NAME: &str = "PSP_007018_1255_PSP_007229_1255";
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_SITE: &str = r"D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255";

#[derive(Resource, Clone)]
struct Args {
    site: String,
    step: Option<usize>,
    spawn: Option<Vec2>,
    heading: f32,
    mode: Mode,
    gravity_mult: f32,
    script: Option<Script>,
    albedo: bool,
    autodrive: bool,
    weave: f32,
    throttle: f32,
    seconds: Option<f32>,
    shot: Option<PathBuf>,
    shot_at: f32,
    log_every: f32,
    high_cam: bool,
    /// --low / ?low: cheaper rendering for weak machines (see visuals.rs)
    low: bool,
    /// initial mouse-wheel zoom factor (tests / screenshots)
    cam_zoom: f32,
    /// live mode: site id in live_index.json, the index file / URL, finest post spacing
    live: Option<String>,
    live_index: String,
    live_mpp: Option<f32>,
    /// native: stream the live site, write OUT.json + OUT.f32 (heights) and exit
    live_dump: Option<PathBuf>,
}

impl Args {
    fn parse_from(argv: impl IntoIterator<Item = String>) -> Self {
        let mut a = Args {
            site: String::new(),
            step: None,
            spawn: None,
            heading: 0.0,
            mode: Mode::Fun,
            gravity_mult: 1.0,
            script: None,
            albedo: false,
            autodrive: false,
            weave: 0.35,
            throttle: 1.0,
            seconds: None,
            shot: None,
            shot_at: 3.0,
            log_every: 1.0,
            high_cam: false,
            low: false,
            cam_zoom: 1.0,
            live: None,
            #[cfg(not(target_arch = "wasm32"))]
            live_index: concat!(env!("CARGO_MANIFEST_DIR"), "/web/live_index.json").into(),
            #[cfg(target_arch = "wasm32")]
            live_index: "live_index.json".into(),
            live_mpp: None,
            live_dump: None,
        };
        let mut it = argv.into_iter();
        let num = |s: Option<String>, flag: &str| -> f32 {
            s.and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("{flag} needs a number"))
        };
        while let Some(s) = it.next() {
            match s.as_str() {
                "--step" => a.step = Some(num(it.next(), "--step") as usize),
                "--spawn" => {
                    let v = it.next().expect("--spawn needs X,Z");
                    let p: Vec<f32> = v.split(',').map(|t| t.trim().parse().expect("--spawn X,Z")).collect();
                    assert!(p.len() == 2, "--spawn needs X,Z");
                    a.spawn = Some(Vec2::new(p[0], p[1]));
                }
                "--heading" => a.heading = num(it.next(), "--heading"),
                "--realistic" => a.mode = Mode::Real,
                "--mode" => {
                    let v = it.next().expect("--mode needs real or fun");
                    a.mode = Mode::parse(&v).unwrap_or_else(|| panic!("--mode {v}: use real or fun"));
                }
                "--gravity-mult" => a.gravity_mult = num(it.next(), "--gravity-mult"),
                "--script" => a.script = Some(Script::parse(&it.next().expect("--script needs a list"))),
                "--albedo" => a.albedo = true,
                "--autodrive" => a.autodrive = true,
                "--high-cam" => a.high_cam = true,
                "--low" => a.low = true,
                "--cam-zoom" => a.cam_zoom = num(it.next(), "--cam-zoom"),
                "--weave" => a.weave = num(it.next(), "--weave"),
                "--throttle" => a.throttle = num(it.next(), "--throttle"),
                "--seconds" => a.seconds = Some(num(it.next(), "--seconds")),
                "--shot" => a.shot = Some(PathBuf::from(it.next().expect("--shot needs a path"))),
                "--shot-at" => a.shot_at = num(it.next(), "--shot-at"),
                "--log-every" => a.log_every = num(it.next(), "--log-every"),
                "--site" => a.site = it.next().expect("--site needs a value"),
                "--live" => a.live = Some(it.next().expect("--live needs a site id")),
                "--live-index" => a.live_index = it.next().expect("--live-index needs a path"),
                "--live-mpp" => a.live_mpp = Some(num(it.next(), "--live-mpp")),
                "--live-dump" => a.live_dump = Some(PathBuf::from(it.next().expect("--live-dump needs a path"))),
                f if f.starts_with("--") => panic!("unknown flag {f}"),
                _ => a.site = s,
            }
        }
        a
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn parse() -> Self {
        let mut a = Self::parse_from(std::env::args().skip(1));
        if a.site.is_empty() {
            a.site = DEFAULT_SITE.into();
        }
        a
    }

    /// Browser: flags come from the URL query (`?site=NAME&autodrive&weave=0.2`); `site` is a
    /// folder name under `sites/` next to the page (or any relative/absolute URL containing '/').
    #[cfg(target_arch = "wasm32")]
    fn parse() -> Self {
        let search = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
        let mut argv = Vec::new();
        for kv in search.trim_start_matches('?').split('&').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let v = js_sys::decode_uri_component(v).ok().and_then(|s| s.as_string()).unwrap_or_else(|| v.to_string());
            argv.push(format!("--{k}"));
            if !v.is_empty() {
                argv.push(v);
            }
        }
        let mut a = Self::parse_from(argv);
        if a.site.is_empty() {
            a.site = DEFAULT_SITE_NAME.into();
        }
        if !a.site.contains('/') && a.live.is_none() {
            a.site = format!("sites/{}", a.site);
        }
        a
    }
}

#[derive(Component)]
struct Hud;

#[derive(Resource)]
struct CamMode {
    dist: f32,
    height: f32,
    /// mouse-wheel zoom multiplier on dist and height
    zoom: f32,
}

/// Seconds since the first frame after loading finished.
#[derive(Resource, Default)]
struct RunClock {
    start: Option<f32>,
    now: f32,
}

#[derive(Resource)]
struct Loading {
    task: Task<Result<Loaded, String>>,
    progress: Progress,
    name: String,
}

#[derive(Resource)]
struct LoadFailed(String);

fn main() {
    let args = Args::parse();
    let _ = DEFAULT_SITE_NAME;
    #[cfg(not(target_arch = "wasm32"))]
    if let (Some(out), Some(id)) = (&args.live_dump, &args.live) {
        std::process::exit(match live_dump(id, &args.live_index, args.live_mpp, out) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("live dump failed: {e}");
                1
            }
        });
    }
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "MarsMaker - HiRISE dune buggy".into(),
                resolution: (1280, 720).into(),
                canvas: Some("#bevy".into()),
                fit_canvas_to_parent: true,
                ..default()
            }),
            ..default()
        }))
        .insert_resource(if args.high_cam { CamMode { dist: 60.0, height: 25.0, zoom: args.cam_zoom } } else { CamMode { dist: 9.0, height: 3.2, zoom: args.cam_zoom } })
        .add_plugins(visuals::VisualsPlugin { low: args.low })
        .insert_resource(args)
        .insert_resource(Time::<Fixed>::from_hz(120.0))
        .init_resource::<Controls>()
        .init_resource::<RunClock>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                finish_load,
                clock,
                input,
                fine_window,
                buggy::sync_visuals.run_if(resource_exists::<Buggy>),
                chase_camera,
                hud,
                test_hooks,
            )
                .chain(),
        )
        .add_systems(FixedUpdate, physics)
        .run();
}

/// Verification hook: stream a live site and write its grid (OUT.json + little-endian f32 absolute
/// heights in OUT.f32, NaN = nodata), without starting the app.
#[cfg(not(target_arch = "wasm32"))]
fn live_dump(id: &str, index: &str, mpp: Option<f32>, out: &std::path::Path) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    let progress = Progress::default();
    let index = std::fs::read(index).map_err(|e| format!("{index}: {e}"))?;
    let (g, st) = bevy::tasks::block_on(live::load_grid(index, id, mpp, &progress))?;
    let secs = t0.elapsed().as_secs_f32();
    let bytes: Vec<u8> = g.z.iter().flat_map(|v| ((*v as f64 + g.base_m) as f32).to_le_bytes()).collect();
    std::fs::write(out.with_extension("f32"), bytes).map_err(|e| e.to_string())?;
    let textured = g.tiles.iter().filter(|t| matches!(t.tex, terrain::Tex::Px(..))).count();
    // texture mosaic on the same grid (u8, 0 = nodata)
    let mut tex = vec![0u8; g.w * g.h];
    for t in &g.tiles {
        if let terrain::Tex::Px(px, tw, th) = &t.tex {
            for r in 0..*th {
                for c in 0..*tw {
                    tex[(t.y * g.tile_step + r) * g.w + t.x * g.tile_step + c] = px[r * tw + c] as u8;
                }
            }
        }
    }
    std::fs::write(out.with_extension("tex"), tex).map_err(|e| e.to_string())?;
    let meta = serde_json::json!({
        "id": g.name, "title": g.rationale, "w": g.w, "h": g.h, "mpp": g.mpp, "east0": g.east0, "north0": g.north0,
        "base_m": g.base_m, "lat": g.lat, "lon": g.lon, "sun": g.sun, "tiles": g.tiles.len(), "textured_tiles": textured,
        "dtm_bytes": st.dtm_bytes, "ortho_bytes": st.ortho_bytes, "dtm_level": st.dtm_level, "ortho_level": st.ortho_level,
        "seconds": secs,
    });
    std::fs::write(out.with_extension("json"), serde_json::to_string_pretty(&meta).unwrap()).map_err(|e| e.to_string())?;
    println!("{meta}");
    Ok(())
}

fn setup(mut commands: Commands, args: Res<Args>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let src = match &args.live {
        Some(id) => Source::live(id.clone(), args.live_index.clone()),
        None => Source::new(args.site.clone()),
    };
    let progress = Progress::default();
    let opts = LoadOptions { step: args.step, albedo: args.albedo, live_mpp: args.live_mpp };
    let task = IoTaskPool::get().spawn(terrain::load(src.clone(), opts, progress.clone()));
    commands.insert_resource(Loading { task, progress, name: src.name() });

    buggy::spawn_visuals(&mut commands, &mut meshes, &mut materials);
    // placeholder low sun from the north-west; replaced by the image's sun once loaded
    commands.spawn(visuals::sun_bundle(args.low));
    visuals::spawn_camera(&mut commands, args.low, Transform::from_xyz(0.0, 4.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y));
    commands.spawn((
        Hud,
        Text::new(""),
        TextFont { font_size: bevy::text::FontSize::Px(17.0), ..default() },
        TextColor(Color::srgb(1.0, 0.96, 0.9)),
        Node { position_type: PositionType::Absolute, top: Val::Px(12.0), left: Val::Px(14.0), ..default() },
    ));
}

/// When the background load finishes: spawn terrain + buggy, aim the sun, place the camera.
#[allow(clippy::too_many_arguments)]
fn finish_load(
    mut commands: Commands,
    args: Res<Args>,
    loading: Option<ResMut<Loading>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut light: Query<&mut Transform, (With<DirectionalLight>, Without<Camera3d>)>,
    mut cam: Query<&mut Transform, (With<Camera3d>, Without<DirectionalLight>)>,
) {
    let Some(mut loading) = loading else { return };
    let Some(result) = check_ready(&mut loading.task) else { return };
    commands.remove_resource::<Loading>();
    let loaded = match result {
        Ok(l) => l,
        Err(e) => {
            error!("loading {} failed: {e}", loading.name);
            commands.insert_resource(LoadFailed(e));
            return;
        }
    };
    let Loaded { mut site, step, tile_step_px, tiles } = loaded;
    if let Some(s) = args.spawn {
        match site.height(s.x, s.y) {
            Some(h) => site.spawn = Vec3::new(s.x, h, s.y),
            None => warn!("--spawn {},{} is not over valid data; using the automatic spawn", s.x, s.y),
        }
    }
    info!("spawn at ({:.1}, {:.1}, {:.1}), textures: {}", site.spawn.x, site.spawn.y, site.spawn.z, site.texture_kind);
    let render = terrain::spawn_terrain(&mut commands, tiles, step, tile_step_px, &mut meshes, &mut images, &mut materials);
    commands.insert_resource(render);
    let tuning = Tuning::new(args.mode, args.gravity_mult);
    commands.insert_resource(Buggy::new(&site, site.spawn, -args.heading.to_radians(), tuning));

    // light from the image's sun (so the ortho's baked shading agrees with ours, and the de-lit
    // albedo gets re-lit the way it was photographed); keep it >= 20 deg up so lee sides stay readable
    if let (Some((az, el)), Ok(mut t)) = (site.sun, light.single_mut()) {
        let (az, el) = (az.to_radians(), el.max(20.0).to_radians());
        let to_sun = Vec3::new(az.sin() * el.cos(), el.sin(), -az.cos() * el.cos());
        *t = Transform::from_translation(to_sun).looking_at(Vec3::ZERO, Vec3::Y);
        info!("sun from the image: azimuth {:.0} deg, elevation {:.0} deg (used {:.0})", az.to_degrees(), site.sun.unwrap().1, el.to_degrees());
    }
    if let Ok(mut c) = cam.single_mut() {
        *c = Transform::from_translation(site.spawn + Vec3::new(0.0, 4.0, 10.0)).looking_at(site.spawn, Vec3::Y);
    }
    commands.insert_resource(site);
}

fn clock(time: Res<Time>, site: Option<Res<Site>>, mut clock: ResMut<RunClock>) {
    if site.is_none() {
        return;
    }
    let t = time.elapsed_secs();
    let start = *clock.start.get_or_insert(t);
    clock.now = t - start;
}

fn input(
    keys: Res<ButtonInput<KeyCode>>,
    args: Res<Args>,
    clock: Res<RunClock>,
    buggy: Option<Res<Buggy>>,
    mut ctl: ResMut<Controls>,
    mut cam: ResMut<CamMode>,
    mut exit: MessageWriter<AppExit>,
    mut flipped_since: Local<Option<f32>>,
    mut script_t: Local<f32>,
) {
    let k = |a: KeyCode, b: KeyCode| keys.pressed(a) || keys.pressed(b);
    ctl.throttle = k(KeyCode::KeyW, KeyCode::ArrowUp) as i32 as f32 - k(KeyCode::KeyS, KeyCode::ArrowDown) as i32 as f32;
    ctl.steer = k(KeyCode::KeyD, KeyCode::ArrowRight) as i32 as f32 - k(KeyCode::KeyA, KeyCode::ArrowLeft) as i32 as f32;
    ctl.brake = keys.pressed(KeyCode::Space);
    ctl.reset |= keys.just_pressed(KeyCode::KeyR);
    ctl.boost = k(KeyCode::ShiftLeft, KeyCode::ShiftRight);
    ctl.jump |= keys.any_just_pressed([KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::KeyJ]);
    ctl.toggle_mode |= keys.just_pressed(KeyCode::KeyM);
    if args.autodrive {
        // full throttle with a slow, gentle weave; flip upright if stuck on the roof for 3 s
        ctl.throttle = args.throttle;
        ctl.steer = args.weave * (clock.now * 0.3).sin();
        if let Some(b) = buggy {
            if (b.rot * Vec3::Y).y < 0.3 {
                let since = *flipped_since.get_or_insert(clock.now);
                if clock.now - since > 3.0 {
                    ctl.reset = true;
                    *flipped_since = None;
                }
            } else {
                *flipped_since = None;
            }
        }
    }
    if let (Some(s), true) = (&args.script, clock.start.is_some()) {
        s.apply(*script_t, clock.now, &mut ctl);
        *script_t = clock.now;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        (cam.dist, cam.height) = if cam.dist < 20.0 { (60.0, 25.0) } else { (9.0, 3.2) };
        cam.zoom = 1.0;
    }
    if keys.just_pressed(KeyCode::Escape) {
        exit.write(AppExit::Success);
    }
}

fn physics(site: Option<Res<Site>>, mut buggy: Option<ResMut<Buggy>>, mut ctl: ResMut<Controls>, time: Res<Time>) {
    let (Some(site), Some(buggy)) = (site, buggy.as_mut()) else { return };
    if ctl.reset {
        buggy.reset(&site);
        ctl.reset = false;
    }
    if ctl.toggle_mode {
        let m = buggy.tuning.mode.other();
        buggy.set_mode(m);
        ctl.toggle_mode = false;
    }
    if ctl.upend {
        buggy.upend(&site);
        ctl.upend = false;
    }
    buggy.step(&site, &ctl, time.delta_secs());
    ctl.jump = false;
}

fn fine_window(
    mut commands: Commands,
    site: Option<Res<Site>>,
    buggy: Option<Res<Buggy>>,
    render: Option<ResMut<TerrainRender>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut vis: Query<&mut Visibility>,
) {
    let (Some(site), Some(buggy), Some(mut render)) = (site, buggy, render) else { return };
    terrain::update_fine_window(&mut commands, &site, &mut render, &mut meshes, &mut vis, buggy.pos);
}

fn chase_camera(
    buggy: Option<Res<Buggy>>,
    site: Option<Res<Site>>,
    mut cam_mode: ResMut<CamMode>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    mut cam: Query<&mut Transform, With<Camera3d>>,
) {
    // mouse wheel: each notch zooms 15 %, from close up to ~30x the default distance
    let notches = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 100.0,
    };
    if notches != 0.0 {
        cam_mode.zoom = (cam_mode.zoom * 0.85f32.powf(notches)).clamp(0.35, 30.0);
    }
    let (Some(buggy), Some(site), Ok(mut cam)) = (buggy, site, cam.single_mut()) else { return };
    let f = buggy.forward();
    let flat = Vec3::new(f.x, 0.0, f.z).normalize_or(Vec3::NEG_Z);
    let mut want = buggy.pos - flat * cam_mode.dist * cam_mode.zoom + Vec3::Y * cam_mode.height * cam_mode.zoom;
    if let Some(g) = site.height(want.x, want.z) {
        want.y = want.y.max(g + 1.0); // never inside a dune
    }
    let a = 1.0 - (-time.delta_secs() * 5.0).exp();
    cam.translation = cam.translation.lerp(want, a);
    cam.look_at(buggy.pos + Vec3::Y * 1.0, Vec3::Y);
}

fn hud(
    buggy: Option<Res<Buggy>>,
    site: Option<Res<Site>>,
    loading: Option<Res<Loading>>,
    failed: Option<Res<LoadFailed>>,
    mut q: Query<&mut Text, With<Hud>>,
) {
    let Ok(mut text) = q.single_mut() else { return };
    if let Some(f) = failed {
        text.0 = format!("Could not load the site:\n{}", f.0);
        return;
    }
    if let Some(l) = loading {
        let p = l.progress.lock().map(|s| s.clone()).unwrap_or_default();
        text.0 = format!("Loading {} ...\n{p}", l.name);
        return;
    }
    let (Some(buggy), Some(site)) = (buggy, site) else { return };
    let slope = site.normal(buggy.pos.x, buggy.pos.z).angle_between(Vec3::Y).to_degrees();
    let respawn = if buggy.respawns > 0 { format!("   respawned: {}", buggy.last_respawn) } else { String::new() };
    text.0 = format!(
        "{}  ({:.2} N, {:.2} E)\n{}\n\n{:5.1} km/h   elevation {:.0} m   slope {:.0} deg   side {:.0} deg   {}{}\n{}\n\n{}\nHiRISE DTM: NASA/JPL/University of Arizona (CC0)",
        site.name,
        site.lat,
        site.lon,
        site.rationale,
        buggy.vel.length() * 3.6,
        buggy.pos.y as f64 + site.base_m,
        slope,
        buggy.side_slope,
        if buggy.airborne() { "AIRBORNE" } else { "" },
        respawn,
        buggy.hud_status(),
        buggy.hud_help(),
    );
}

/// --shot / --seconds / periodic physics log.
#[allow(clippy::too_many_arguments)]
fn test_hooks(
    mut commands: Commands,
    args: Res<Args>,
    clock: Res<RunClock>,
    buggy: Option<Res<Buggy>>,
    site: Option<Res<Site>>,
    render: Option<Res<TerrainRender>>,
    failed: Option<Res<LoadFailed>>,
    time: Res<Time<Real>>,
    mut next_log: Local<f32>,
    mut shot_taken: Local<bool>,
    mut exit: MessageWriter<AppExit>,
) {
    if failed.is_some() && (args.shot.is_some() || args.seconds.is_some()) {
        exit.write(AppExit::error());
        return;
    }
    if site.is_none() {
        return; // still loading
    }
    let t = clock.now;
    if let (Some(b), Some(site)) = (&buggy, &site) {
        if args.log_every > 0.0 && t >= *next_log {
            *next_log = t + args.log_every;
            let g = site.height(b.pos.x, b.pos.z);
            let hag = g.map(|g| format!("{:.2}", b.pos.y - g)).unwrap_or_else(|| "nodata".into());
            let r = render.as_ref();
            info!(
                "t={t:5.1} pos=({:.1},{:.1},{:.1}) speed={:.1}km/h wheels={} hag={hag} up_y={:.3} side={:.0} max_side_1s={:.0} rollovers={} respawns={} fine_chunks={} rebuilds={} build={:.1}ms swap={:.2}ms fps={:.0} {}",
                b.pos.x,
                b.pos.y,
                b.pos.z,
                b.vel.length() * 3.6,
                b.grounded,
                (b.rot * Vec3::Y).y,
                b.side_slope,
                b.max_sustained_side,
                b.rollovers,
                b.respawns,
                r.map_or(0, |r| r.fine_chunks),
                r.map_or(0, |r| r.rebuilds),
                r.map_or(0.0, |r| r.build_ms),
                r.map_or(0.0, |r| r.swap_ms),
                1.0 / time.delta_secs().max(1e-4),
                b.log_extra(),
            );
        }
    }
    if let Some(path) = &args.shot {
        if !*shot_taken && t >= args.shot_at {
            *shot_taken = true;
            let quit = args.seconds.is_none_or(|s| s <= args.shot_at);
            let p = path.clone();
            let mut e = commands.spawn(Screenshot::primary_window());
            e.observe(save_to_disk(path.clone()));
            e.observe(move |_: On<ScreenshotCaptured>, mut exit: MessageWriter<AppExit>| {
                info!("screenshot saved to {}", p.display());
                if quit {
                    exit.write(AppExit::Success);
                }
            });
        }
    }
    // hard deadline (also a safety net if a screenshot never arrives)
    let deadline = match (args.seconds, args.shot.is_some()) {
        (Some(s), false) => Some(s),
        (Some(s), true) => Some(s.max(args.shot_at + 5.0)),
        (None, true) => Some(args.shot_at + 10.0),
        (None, false) => None,
    };
    if deadline.is_some_and(|d| t >= d) {
        exit.write(AppExit::Success);
    }
}
