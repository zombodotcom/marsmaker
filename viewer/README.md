# MarsMaker viewer

Drive a dune buggy at 1:1 scale on real HiRISE Mars terrain (Bevy 0.20).

```
cargo build --release -j 4
target/release/marsmaker-viewer [SITE_DIR] [flags]
```

`SITE_DIR` is a converted site folder containing `meta.json`. The default is the Russell Crater
megadune, `D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255`.

## Main menu

Started with no site and no live id (and no test flags such as `--shot`, `--seconds`,
`--autodrive`, `--script`), the viewer opens on the main menu; `--menu` forces it. With a site or
`--live ID` on the command line (or `?site=` / `?live=` in the URL, as play.html links use) it goes
straight in, as before. Pages:

- **ABOUT**: the disclaimer. Real NASA HiRISE terrain (CC0, NASA/JPL/University of Arizona);
  heights accurate to decimetres but resolution about 1-4 m; FUN ignores real physics; SAND is
  simulated and changes the terrain; not a scientific tool; MIT code.
- **MAP**: the converted sites on disk (every folder with a `meta.json` in the sites directory,
  default `D:\marsmaker_data\sites`, or `--sites-dir DIR`; `*.tmp` folders are skipped) plus the
  1,262 DTMs live mode streams from NASA (`web/live_index.json`, or `--live-index FILE`). Type to
  search (name, id, place), filter chips (dunes, craters, polar, gullies, layers, channels,
  landing sites, ice) and source chips (on disk / live). The Mars map has one dot per site
  (cyan on disk, orange live); click near a dot to pick it. In the browser the on-disk list is
  `sites.json` next to the page (written by `tools/web_pack.py`; without it the two default web
  sites are tried).
- **MODE**: FUN / REAL / SAND (SAND shows as "coming soon" until the build has that mode), plus the
  terrain smoothing setting (`--smooth`).
- **CONTROLS**: keys and touch.

PLAY loads the site with the usual progress display. In game, **Esc** (or the `||` button at the
top right, for touch) pauses and opens the pause menu: Resume, Change map, Change mode,
Controls, About, Quit. The game clock and physics stop while a menu is open. Changing mode on the
same map is instant; changing map (or smoothing, or to/from a mode that rewrites the terrain)
despawns the old terrain, buggy and every other scene entity spawned for the site, drops the site
resources (heightfield, meshes, textures), then loads the new map. The log prints entity / mesh /
image / material counts and process memory before and after each unload.

Menu keys: Tab / Shift+Tab switch pages, Up / Down (PageUp / PageDown) choose a site, Left /
Right cycle filters, typing searches, Backspace deletes, Enter plays, Esc goes back / resumes.
Mouse wheel or drag scrolls the list; double-click a row to play it.

## Modes

Three driving modes, shown in the HUD. Press **M** to cycle FUN -> REAL -> SAND live (keeps position
and speed), or start in one with `--mode fun|real|sand` (`?mode=fun|real|sand` in the browser).

- **JUST FOR FUN** (default): ignores realism where that's more fun. Grippy tires (mu 1.1 on an
  Earth-like load, so Mars' low gravity doesn't leave them skating), a cornering-first friction
  circle (flooring it never washes the car out sideways), a mild yaw-stability assist, a
  strong engine (~117 km/h on the flat), **Shift** boost (~140 km/h; the meter lasts 3 s and
  recharges in 5 s once you let go), **Ctrl** or **J** to jump (6 m/s kick, ~5 m high and ~3 s of
  air in Mars gravity), air control (W/S pitch, A/D roll while airborne), soft landings (a
  righting push for 0.7 s after touching down, so only a landing past ~75 deg of tilt rolls you
  over), and an auto-flip after 1.5 s on the roof or side. Until you use air control in a jump,
  the tumble a dune crest kicks in is damped, so flying off a crest at speed lands on the wheels. The HUD shows the boost meter,
  airtime, height and flips of the current or last jump, and your best air, best height and
  total flips (a flip only counts if you land on the wheels).
  Air control needs a fresh press: a key you were already holding at takeoff (e.g. W over a crest)
  does nothing in the air until you let go or press another one.
- **REAL MARS** (`--mode real`, or `--realistic`): Mars as it is. Loose dry sand: a friction
  ellipse of mu 0.48 / 0.58 whose grip falls off with slip, load-proportional sinkage drag, and a
  light sand rail's power (2600 N at the wheels, ~60 km/h on the flat). No boost, jump, air
  control or assists.
- **REAL SAND** (`--mode sand`, `?mode=sand`): REAL MARS on sand that behaves like sand. See
  *REAL SAND* below. The dunes move (time-lapse with **[** / **]**), the wheels leave ruts and
  sink in, and anything you see changed is a simulation: the HUD then says
  "SIMULATED - not NASA data" until you press **K** (back to the real DTM).

All use Mars gravity (3.711 m/s^2). `--gravity-mult G` scales it (e.g. 0.5 for floaty jumps).

## Controls

| Key | Action |
| --- | --- |
| W / Up, S / Down | throttle / reverse (FUN, in the air: pitch nose down / up) |
| A / D, Left / Right | steer (FUN, in the air: roll left / right) |
| Space | brake |
| Shift | boost (FUN) |
| Ctrl or J | jump (FUN; in the browser use J, since Ctrl+W closes the tab) |
| M | cycle JUST FOR FUN -> REAL MARS -> REAL SAND |
| [ / ] | REAL SAND: slower / faster time-lapse (paused, real time ... 1 Mars year per second) |
| , / . | REAL SAND: turn the wind 15 deg left / right |
| K | REAL SAND: put the sand back (the real DTM again) |
| R | flip upright where you are |
| C | toggle chase camera / high camera |
| Mouse wheel | zoom the camera in / out (C resets) |
| Esc | pause menu (Resume, Change map, Change mode, Controls, Quit) |

## Flags

| Flag | Meaning |
| --- | --- |
| `--mode real\|fun\|sand` | start in REAL MARS, JUST FOR FUN or REAL SAND (default fun). See Modes above and `Tuning` in `src/buggy.rs`. |
| `--sand-rate R` | REAL SAND: start the time-lapse at R Mars years per second (default real time). |
| `--sand-wind DEG` | REAL SAND: compass direction the sand moves *toward* (default: estimated from the slip faces). |
| `--sand-flux Q` | REAL SAND: saturated sand flux, m^3 per m per Earth year (default 5). |
| `--sand-region M` | REAL SAND: edge of the simulated square in metres (default 1024). |
| `--sand-shots NAME@T,...` | REAL SAND test hook: at T s save NAME.png (screenshot), NAME_before/after/diff.png (hillshade of the simulated square before / now, and the height change; wind blows left to right) and log a `SAND DUMP` stats line. |
| `--sand-log T` | REAL SAND log period (default 1 s). |
| `--sand-reset T,...` | REAL SAND test hook: press K (back to the real DTM) at T s. `MM_SAND_RELAX=N` (env) caps the avalanche work per event (default 400). |
| `--realistic` | same as `--mode real`. |
| `--gravity-mult G` | multiply Mars gravity by G (default 1). |
| `--albedo` | use the approximate de-lit `a_` tiles (converter v3) instead of the ortho `t_` tiles, where present. Lighting follows the image's sun (meta `sun`) either way. |
| `--heading DEG` | initial compass heading (0 = north, 90 = east). |
| `--spawn X,Z` | spawn at world X (east, m) / Z (south, m) instead of the automatic dune-field spawn. Positions are printed in the log. |
| `--step N` | decimation step for the far terrain mesh (must divide 144). Default: automatic, keeping the site under about 16M vertices. |
| `--high-cam` | start in the high camera. |
| `--low` | cheaper rendering for weak machines / browsers (`?low`): no HDR/bloom, FXAA instead of MSAA, no close-up terrain detail, fewer dust particles, shorter tire tracks, smaller shadow maps. |
| `--cam-zoom Z` | start with this mouse-wheel zoom factor (1 = default; for screenshots). |
| `--bg-window` | for automated test runs: a small 640x360, unfocused, bottom-most window off the visible desktop with no taskbar entry, so tests don't pop up over the user (rendering and `--shot` still work; fps is throttled there, so measure fps without it). |
| `--smooth M` | terrain low-pass sigma in metres (default 2; `0` = the raw DTM). See *Terrain* below. |
| `--menu` | open the main menu even though a site is given (it is then preselected). |
| `--sites-dir DIR` | folder of converted sites listed in the menu (default `D:\marsmaker_data\sites`). |
| `--window WxH` | window size (e.g. `--window 1280x720` with `--bg-window`, for menu screenshots). |
| `--menu-script LIST` | menu test hook (implies `--menu`): comma-separated steps `CMD[:ARG][@DELAY]` run in order, DELAY = seconds to wait after the step (default 0.3). `page:map\|mode\|about\|controls\|pause`, `search:TEXT`, `filter:dunes`, `source:all\|disk\|live`, `site:ID` / `site:live:ID`, `mode:fun\|real\|sand`, `smooth:raw\|1\|2\|4`, `play`, `esc`, `resume`, `loaded` (wait until driving), `drive:on\|off` (autodrive), `shot:FILE.png` (waits until saved), `stats` (entities / assets / memory), `wait:S`, `quit`. |
| `--autodrive` | full throttle with a slow sinusoidal weave. Auto-flips if the buggy sits on its roof for 3 s. |
| `--weave A` | steer amplitude for `--autodrive`, 0..1 (default 0.35; 0 = straight). |
| `--throttle T` | autodrive throttle, 0..1 (default 1). |
| `--seconds N` | exit after N seconds, counted from the first frame after loading. |
| `--shot OUT.png` | save a screenshot at `--shot-at` seconds (default 3), then exit unless `--seconds` is later. |
| `--shot-at T` | when to take the `--shot` screenshot. |
| `--log-every T` | physics log period in seconds (default 1; 0 = off). |
| `--bench` | headless driving-quality benchmark on SITE_DIR (or `synthetic:flat` / `synthetic:noise`): no window, prints one JSON line per mode x driving pattern. Use `tools/drive_bench.py`. `--bench-seconds N` (default 45), `--bench-mode real\|fun`, `--bench-patterns a,b` (e.g. `ramp` with `--spawn` on the `make_ramp_site.py` site for a headless side-slope test). |
| `--script LIST` | scripted input for tests, e.g. `jump@3,boost@4-6,mode@8`. `NAME@T` is a press at T s, `NAME@T0-T1` a hold that overrides the keyboard and `--autodrive`. Presses: `jump`, `mode`, `reset`, `upend` (turn onto the roof, to test auto-flip). Holds: `boost`, `fwd`, `back`, `coast`, `left`, `right`, `straight`, `brake`. |

Each log line has this form:

```
t=  3.0 pos=(x,y,z) speed=..km/h wheels=<in contact> hag=<CoM height above ground, m> up_y=<chassis up.y>
        side=<side slope across the heading, deg> max_side_1s=<largest side slope held 1 s upright>
        rollovers=.. respawns=.. fine_chunks=.. rebuilds=.. build=<background mesh build ms>
        swap=<main-thread swap ms> fps=.. mode=<real|fun> g=<gravity> boost=<meter 0..1>[(on)]
        air=<current airtime, s> jumps=.. flips=.. best_air=.. best_h=.. autoflips=..
```

`JUMP #n`, `LANDED on the wheels|on the roof/side airtime .. max height .. flips ..`,
`AUTO-FLIP #n` and `MODE -> ..` lines are logged as they happen.

At rest on flat ground `hag` is about 0.67 m (the CoM) and `wheels=4`. A `ROLLOVER #n` line is
logged whenever the chassis passes ~80 deg of roll/pitch.

`tools/drive_bench.py --exe PATH --out results.jsonl [--compare baseline.jsonl]` runs `--bench`
on five dune sites plus the two synthetic terrains and prints per-mode tables: share of time on
all four wheels, unwanted airborne events per km, seat vertical-acceleration RMS and jerk,
pitch/roll rate, rollovers per 10 km, respawns, NaN blow-ups, peak spin, stuck time, average
speed, body slip angle / lateral slide speed, steer-step yaw rate and response time, straight-line
yaw wander and the physics cost per step, plus a ride table at an equal 30 km/h cruise (the fair
comparison) and parked creep / jitter. The developer hook `MM_TUNE="k=3000,fun.yaw_assist=0"`
overrides tuning fields (see `Tuning::apply_overrides`) for sweeps without a rebuild.

`tools/make_ramp_site.py OUT_DIR [--bumps 0.4]` writes a synthetic site with 20-50 deg side-slope
strips for physics tests (it prints the `--spawn`/`--heading` for each strip).
`tools/web_pack.py` makes downsampled copies of sites for the browser build (see WEB.md).

Menu round trip (main menu, local site, pause, switch to another local site and a live one, switch
mode, with screenshots and unload counts in the log):

```
marsmaker-viewer --bg-window --window 1280x720 --menu-script "shot:m_about.png,page:map,filter:dunes,shot:m_map.png,site:PSP_007018_1255_PSP_007229_1255,mode:real,play,loaded@6,shot:g1.png,esc,shot:pause.png,page:map,site:PSP_003800_1325_PSP_004077_1325,play,loaded@6,shot:g2.png,esc,page:map,site:live:ESP_011417_1755_ESP_011562_1755,play,loaded@6,shot:g3.png,esc,page:mode,mode:fun,play@2,shot:g4.png,quit"
```

Example test run (about 20 s, below-normal priority is advisable on a shared PC):

```
marsmaker-viewer --autodrive --seconds 20 --shot out.png --shot-at 10
marsmaker-viewer --autodrive --seconds 20 --script jump@3,boost@3.5-6.5,upend@12,mode@16
```

## REAL SAND (src/sand.rs, src/sand_sim.rs)

REAL MARS tires on sand that behaves like sand. Everything it changes is a simulation, so as soon
as anything differs from the DTM the HUD says **SIMULATED - not NASA data**; **K** (or switching
map) puts the real DTM back.

- **Moving dunes.** A Werner (1995, *Geology* 23, 1107) slab automaton runs on a 1024 m square
  (512 x 512 cells of 2 m, `--sand-region`) around the buggy, on top of the (smoothed) DTM:
  random sand cells not in a 15 deg wind shadow lose a 0.2 m slab, which hops 3 cells (6 m)
  downwind and lands with probability 0.6 on sand, 0.4 on bare floor, 1 in a shadow zone;
  after each move the neighbourhood avalanches to a 30 deg angle of repose (Mars ~32-34 deg,
  30 is stabler). The grid is rotated into the wind, so hops and shadow scans run along rows.
  Only sand above an estimated floor can move: a morphological opening of the DTM (200 m
  window, removes dunes but keeps plains and slopes), blurred, +0.4 m. Sand blown out of the
  downwind edge re-enters upwind in a random row; erosion and deposition fade out over the
  outer 48 m, so the edge stays the DTM. The square re-centres when you drive near its edge
  (changes are kept in a sparse per-post layer that `Site::at` adds to the DTM, so physics,
  meshes and later squares all see them).
- **Wind.** By default the slip faces decide: every 2 m post sloping 24-42 deg votes with its
  downslope direction (lee faces face downwind). Barchans (ESP_071577_2560): toward 45 deg,
  coherence 0.83 from 6,061 posts; Proctor: toward 20 deg, coherence 0.37 (complex dunes).
  **,** / **.** turn it 15 deg, `--sand-wind DEG` sets it.
- **Clock.** Calibrated to a saturated sand flux of 5 m^3 per m per Earth year (`--sand-flux`),
  i.e. 4.7 slab picks per cell per Mars year; a dune of height H then moves ~flux/H. Observed:
  ~0.1-1 m per Earth year and fluxes of a few to ~10-20 m^3/m/yr (Bridges et al. 2012, *Nature*
  485, 339; Chojnacki et al. 2019, *Geology* 47, 427). **[** / **]** step the time-lapse:
  paused, real time, 1 min/s, 1 hour/s, 1 sol/s, 10 and 100 sols/s, 1 Mars year/s
  (`--sand-rate R` = R Mars years/s). The HUD shows the requested and achieved rate and the
  simulated time. The sim stops while the pause menu is open.
- **Budget.** Native: the automaton runs on a background task (up to ~60% of a frame, 6-40 ms)
  and publishes its changes to the post layer (<= 4 ms); the main thread only rebuilds changed
  terrain chunks (4 background tasks at a time, nearest first) and overlays, <= ~2.7 ms a frame.
  Browser (no threads, no compute shaders): the same code inline, 2 ms sim + 1 ms publish +
  2.5 ms meshes per frame.
- **Tire tracks.** A 0.2 m "rut" layer (sparse 8 m patches) on top of the dune surface: each
  wheel presses its footprint (the tire's round profile, so it agrees with the tread envelope)
  to the sinkage depth and pushes 70% of that volume into side berms. Ruts persist and fade in
  proportion to the simulated sand transport (each slab event per cell halves their relief).
  Rut patches are drawn as their own fine meshes (with the terrain material); the full-res
  terrain quads under them are left out, and the fine quads tile the terrain triangles exactly,
  so there are no cracks.
- **Soft sand.** Static sinkage 5 cm at the static wheel load in deep loose sand, scaling with
  load, sand thickness (0 on bare floor) and slope (x1.8 near repose), less at speed (/2 at
  30 km/h), more with wheelspin; a wheel spinning in place digs in at up to 5 cm/s (max 30 cm).
  The drag is from the *fresh* sinkage just ahead of the tire (rolling in an existing rut is
  easy): compaction crr = 0.5 sqrt(z / D) plus bulldozing past 12 cm, applied as a force from
  the sand (outside the tire's friction budget). Grip drops up to 25% in fresh sand and 35% near
  the angle of repose; wheels on near-repose slip faces set off local avalanches (the sand
  under them fails at 27 deg). The HUD shows sinkage per wheel, the extra rolling resistance,
  grip and sand depth, and **BOGGED DOWN** when you're stuck with the throttle down.

Measured (`--sand-rate 1`, buggy parked with the brake, 66 s, this shared PC; `SAND DUMP` lines):

| Site | Simulated | Dunes moved downwind | Cells changed > 5 cm | dh range | Net volume | NaN |
| --- | --- | --- | --- | --- | --- | --- |
| Barchans ESP_071577_2560 | 45 Mars yr | +14.1 m (0.31 m/Mars yr, 0.17 m/Earth yr) | 38% | -10.0 / +14.2 m | -0.1 m^3 | 0 |
| Proctor PSP_003800_1325 | 25.5 Mars yr | +4.3 m (0.17 m/Mars yr) | 58% | -7.3 / +9.4 m | -1.4 m^3 | 0 |

Migration is the shift that best aligns the before / after sand-thickness maps along the wind
(cross-correlation). Barchans: the main barchan's lee face advances in an arc, its stoss side
erodes, and a small dome upwind turns into a barchan with horns. Driving in REAL SAND on the
barchans: 2-8 cm sinkage cruising, 13-16 cm spinning up from rest, crr +0.1-0.3, grip x0.6-0.8;
on a 22-29 deg slip face full throttle slides back down (REAL MARS creeps and slides too);
starting on a 13 deg soft slope it crawls at 2-4 km/h.

Caveats: the floor estimate can't tell sand from narrow bedrock knobs; the slab automaton makes
the dune surfaces rougher than real ones (0.2 m slabs on 2 m cells); 2 m cells under 1 m posts
are interpolated; ripples, sand supply, seasonal winds, ice-cemented polar sand and dust are not
modelled; one constant wind. Blowing-sand streamers are not done.

## Look (src/visuals.rs)

- **Sky**: an analytic Mars sky on a camera-following dome (WebGL2-safe, no compute):
  butterscotch overhead, hazy towards the horizon, a blue-white aureole around the sun that
  grows near sunset, a small sun disc that blooms. Colours follow the sun elevation.
- **Haze**: exponential-squared distance fog in the sky's horizon colour, so nothing between
  camera and buggy is hazed, distant terrain fades into the horizon from ~1-2 km, and the far
  edge melts into the sky.
- **Camera**: HDR + TonyMcMapface + mild bloom, a little contrast grading, speed-based FOV
  (+14 deg towards boost speed), smoothed look-ahead and a short shake on hard landings.
- **Chase camera** (`src/camera.rs`): tilts with the slope (low, looking up a dune face it
  climbs; higher, looking down one it descends), rises over - or past ~58 deg pulls in in front
  of - any terrain between it and the buggy, and keeps clear of the ground around it. Pulling in
  is fast, easing back out slow. `--bench` scores it (`cam_*` fields, `old_cam_*` = the camera
  before this, for comparison; `MM_BENCH_CAM_ZOOM` for zoomed-out views).
- **Terrain**: the greyscale ortho is mapped through a Mars palette (dark basaltic sand ->
  brown sand -> ochre -> bright dust) instead of a single tint; close up a cosmetic wind-ripple
  normal and fine grain break up the texture (fades out by ~40 m).
- **Buggy**: procedural sand rail: tube cage, painted panels, suited driver, engine and
  exhausts, spare wheel, light bar, whip flag that sways, paddle (rear) / block (front) tires
  on beadlock rims, double wishbones and coilovers that follow the suspension, wheels that
  keep spinning in the air.
- **Effects**: pooled dust puffs from each wheel on the ground (and a rooster tail from the
  rear paddles), a burst on hard landings, puffs near the camera faded out; tire tracks that
  darken the sand and fade over 40 s; boost flames (and a light, native only) in FUN mode.

## How it works (short)

- **Vehicle**: 420 kg, CoM 0.67 m above ground, 2.0 m track (static stability factor ~1.5,
  tips at ~56 deg), explicit inertia tensor, anti-roll bars, tire forces applied at a 0.25 m
  (FUN 0.35 m) roll centre, suspension rays intersected with the local ground plane.
- **Suspension** is sized for Mars gravity: a soft progressive spring (2600 N/m + cubic term,
  ~0.15 m static sag, ~0.8 Hz) so the wheels can droop into dips instead of leaving the ground
  (the old 9000 N/m spring sagged only 4 cm under 3.711 m/s^2), ~0.4 m of bump travel, a damped
  bump stop, and dampers at 0.35 of critical in compression / 1.0 in rebound (no pogo after a
  landing). Each tire tests six points along its footprint, so its round tread climbs onto a
  step instead of a point contact taking it as a kick. Steering is speed-sensitive (the lock is
  what the tires can actually turn at that speed) and rate-limited for keyboard input. On the ramp test site it
  holds every strip to 50 deg without rolling. REAL MARS sand (friction ellipse 0.48 / 0.58)
  slides from ~28 deg, earlier under throttle; FUN grip (mu 1.1) holds much steeper.
- **FUN extras** are all in `src/buggy.rs` (constants at the top): boost is a thrust along the
  nose through the CoM (it works in the air), jump is a velocity kick along the chassis-up /
  world-up average, air control steers the body pitch / roll rate toward a target, and the
  landing assist is an angular acceleration toward the ground normal plus damping of the tumble.
- **Terrain** (physics and meshes alike): HiRISE DTMs are posted at ~1 m but resolve ~3-5 m,
  so the top octave is stereo-matching noise, single-post spikes and TIN-facet creases. At load
  the heights are despiked (a post > 0.35 m above / below all 8 neighbours takes their mean) and
  low-passed with a Gaussian, sigma 2 m (`--smooth`), normalised over valid posts so nodata edges
  don't sag. The spawn picker still sees the raw DTM. Heights are interpolated on the same
  two-triangle split the meshes use, and contact normals come from a 3x3 Sobel over samples
  1.5 posts apart.
- **Rendering** cuts each tile into 144-post chunks. Far chunks are decimated. A 5x5-chunk
  (720 m) window around the buggy is drawn at full resolution and rebuilt when the buggy
  crosses a chunk boundary (meshes built on background tasks and swapped in when all are ready, so there is no frame hitch), so what the wheels touch is exactly what's drawn (the smoothed surface). Full-res chunk
  edges that meet decimated chunks are snapped onto the coarse edges, so there are no cracks.
- **Spawn** goes to the 128 m block with the highest 8 m-baseline gradient variance (a dune
  field). Blocks near nodata are skipped, as are steep-noise and TIN-interpolated
  (planar-facet) areas.
- **Edge of data**: if the chassis centre is over nodata for 0.4 s, falls 50 m below the
  site's lowest point, tunnels under the ground, or the state goes non-finite, the buggy
  respawns on good ground from about 2 s earlier, turned around to face back into the data.
- **Textures**: mip levels average only valid texels, nodata edges are dilated by 2 px, and
  any remaining holes are filled with the tile's median grey. A tile whose ortho texture is
  entirely nodata is drawn untextured, tinted to the site's median brightness. The DN stretch
  already applied by the converter is used as-is.
