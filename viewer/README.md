# MarsMaker viewer

Drive a dune buggy at 1:1 scale on real HiRISE Mars terrain (Bevy 0.20).

```
cargo build --release -j 4
target/release/marsmaker-viewer [SITE_DIR] [flags]
```

`SITE_DIR` is a converted site folder containing `meta.json`. The default is the Russell Crater
megadune, `D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255`.

## Modes

Two driving modes, shown in the HUD. Press **M** to switch live (keeps position and speed), or
start in one with `--mode real|fun` (`?mode=real|fun` in the browser).

- **JUST FOR FUN** (default): ignores realism where that's more fun. Grippy tires (mu 1.1), a
  strong engine (~117 km/h on the flat), **Shift** boost (~140 km/h; the meter lasts 3 s and
  recharges in 5 s once you let go), **Ctrl** or **J** to jump (6 m/s kick, ~5 m high and ~3 s of
  air in Mars gravity), air control (W/S pitch, A/D roll while airborne), soft landings (a
  righting push for 0.7 s after touching down, so only a landing past ~75 deg of tilt rolls you
  over), and an auto-flip after 1.5 s on the roof or side. The HUD shows the boost meter,
  airtime, height and flips of the current or last jump, and your best air, best height and
  total flips (a flip only counts if you land on the wheels).
  Air control needs a fresh press: a key you were already holding at takeoff (e.g. W over a crest)
  does nothing in the air until you let go or press another one.
- **REAL MARS** (`--mode real`, or `--realistic`): Mars as it is. Loose dry sand: a friction
  ellipse of mu 0.48 / 0.58 whose grip falls off with slip, load-proportional sinkage drag, and a
  light sand rail's power (2600 N at the wheels, ~60 km/h on the flat). No boost, jump, air
  control or assists.

Both use Mars gravity (3.711 m/s^2). `--gravity-mult G` scales it (e.g. 0.5 for floaty jumps).

## Controls

| Key | Action |
| --- | --- |
| W / Up, S / Down | throttle / reverse (FUN, in the air: pitch nose down / up) |
| A / D, Left / Right | steer (FUN, in the air: roll left / right) |
| Space | brake |
| Shift | boost (FUN) |
| Ctrl or J | jump (FUN; in the browser use J, since Ctrl+W closes the tab) |
| M | switch REAL MARS / JUST FOR FUN |
| R | flip upright where you are |
| C | toggle chase camera / high camera |
| Mouse wheel | zoom the camera in / out (C resets) |
| Esc | quit |

## Flags

| Flag | Meaning |
| --- | --- |
| `--mode real\|fun` | start in REAL MARS or JUST FOR FUN (default fun). See Modes above and `Tuning` in `src/buggy.rs`. |
| `--realistic` | same as `--mode real`. |
| `--gravity-mult G` | multiply Mars gravity by G (default 1). |
| `--albedo` | use the approximate de-lit `a_` tiles (converter v3) instead of the ortho `t_` tiles, where present. Lighting follows the image's sun (meta `sun`) either way. |
| `--heading DEG` | initial compass heading (0 = north, 90 = east). |
| `--spawn X,Z` | spawn at world X (east, m) / Z (south, m) instead of the automatic dune-field spawn. Positions are printed in the log. |
| `--step N` | decimation step for the far terrain mesh (must divide 144). Default: automatic, keeping the site under about 16M vertices. |
| `--high-cam` | start in the high camera. |
| `--low` | cheaper rendering for weak machines / browsers (`?low`): no HDR/bloom, FXAA instead of MSAA, no close-up terrain detail, fewer dust particles, shorter tire tracks, smaller shadow maps. |
| `--bg-window` | test runs: open the window unfocused and off-screen. |
| `--cam-zoom Z` | start with this mouse-wheel zoom factor (1 = default; for screenshots). |
| `--autodrive` | full throttle with a slow sinusoidal weave. Auto-flips if the buggy sits on its roof for 3 s. |
| `--weave A` | steer amplitude for `--autodrive`, 0..1 (default 0.35; 0 = straight). |
| `--throttle T` | autodrive throttle, 0..1 (default 1). |
| `--seconds N` | exit after N seconds, counted from the first frame after loading. |
| `--shot OUT.png` | save a screenshot at `--shot-at` seconds (default 3), then exit unless `--seconds` is later. |
| `--shot-at T` | when to take the `--shot` screenshot. |
| `--log-every T` | physics log period in seconds (default 1; 0 = off). |
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

`tools/make_ramp_site.py OUT_DIR [--bumps 0.4]` writes a synthetic site with 20-50 deg side-slope
strips for physics tests (it prints the `--spawn`/`--heading` for each strip).
`tools/web_pack.py` makes downsampled copies of sites for the browser build (see WEB.md).

Example test run (about 20 s, below-normal priority is advisable on a shared PC):

```
marsmaker-viewer --autodrive --seconds 20 --shot out.png --shot-at 10
marsmaker-viewer --autodrive --seconds 20 --script jump@3,boost@3.5-6.5,upend@12,mode@16
```

## Look (src/visuals.rs)

- **Sky**: an analytic Mars sky on a camera-following dome (WebGL2-safe, no compute):
  butterscotch overhead, hazy towards the horizon, a blue-white aureole around the sun that
  grows near sunset, a small sun disc that blooms. Colours follow the sun elevation.
- **Haze**: exponential-squared distance fog in the sky's horizon colour, so nothing between
  camera and buggy is hazed, distant terrain fades into the horizon from ~1-2 km, and the far
  edge melts into the sky.
- **Camera**: HDR + TonyMcMapface + mild bloom, a little contrast grading, speed-based FOV
  (+14 deg towards boost speed), smoothed look-ahead and a short shake on hard landings.
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
  tips at ~56 deg), explicit inertia tensor, anti-roll bars, tire forces applied at a 0.25 m roll
  centre, suspension rays intersected with the local ground plane. On the ramp test site it
  holds every strip to 50 deg without rolling. REAL MARS sand (friction ellipse 0.48 / 0.58)
  slides from ~28 deg, earlier under throttle; FUN grip (mu 1.1) holds much steeper.
- **FUN extras** are all in `src/buggy.rs` (constants at the top): boost is a thrust along the
  nose through the CoM (it works in the air), jump is a velocity kick along the chassis-up /
  world-up average, air control steers the body pitch / roll rate toward a target, and the
  landing assist is an angular acceleration toward the ground normal plus damping of the tumble.
- **Physics** uses the full-resolution DTM heightfield. Heights are interpolated on the same
  two-triangle split the meshes use, and contact normals come from a 3x3 Sobel over samples
  1.5 posts apart.
- **Rendering** cuts each tile into 144-post chunks. Far chunks are decimated. A 5x5-chunk
  (720 m) window around the buggy is drawn at full resolution and rebuilt when the buggy
  crosses a chunk boundary (meshes built on background tasks and swapped in when all are ready, so there is no frame hitch), so what the wheels touch is exactly what's drawn. Full-res chunk
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
