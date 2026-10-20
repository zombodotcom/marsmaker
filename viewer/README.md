# MarsMaker viewer

Drive a dune buggy at 1:1 scale on real HiRISE Mars terrain (Bevy 0.20).

```
cargo build --release -j 4
target/release/marsmaker-viewer [SITE_DIR] [flags]
```

`SITE_DIR` is a converted site folder containing `meta.json`. The default is the Russell Crater
megadune, `D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255`.

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
| Esc | quit |

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
| `--realistic` | same as `--mode real`. |
| `--gravity-mult G` | multiply Mars gravity by G (default 1). |
| `--albedo` | use the approximate de-lit `a_` tiles (converter v3) instead of the ortho `t_` tiles, where present. Lighting follows the image's sun (meta `sun`) either way. |
| `--heading DEG` | initial compass heading (0 = north, 90 = east). |
| `--spawn X,Z` | spawn at world X (east, m) / Z (south, m) instead of the automatic dune-field spawn. Positions are printed in the log. |
| `--step N` | decimation step for the far terrain mesh (must divide 144). Default: automatic, keeping the site under about 16M vertices. |
| `--high-cam` | start in the high camera. |
| `--bg-window` | for automated test runs: a small 640x360, unfocused, bottom-most window off the visible desktop with no taskbar entry, so tests don't pop up over the user (rendering and `--shot` still work; fps is throttled there, so measure fps without it). |
| `--smooth M` | terrain low-pass sigma in metres (default 2; `0` = the raw DTM). See *Terrain* below. |
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

Example test run (about 20 s, below-normal priority is advisable on a shared PC):

```
marsmaker-viewer --autodrive --seconds 20 --shot out.png --shot-at 10
marsmaker-viewer --autodrive --seconds 20 --script jump@3,boost@3.5-6.5,upend@12,mode@16
```

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
