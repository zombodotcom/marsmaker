# MarsMaker viewer

Drive a dune buggy at 1:1 scale on real HiRISE Mars terrain (Bevy 0.20).

```
cargo build --release -j 4
target/release/marsmaker-viewer [SITE_DIR] [flags]
```

`SITE_DIR` is a converted site folder containing `meta.json`. The default is the Russell Crater
megadune, `D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255`.

## Controls

| Key | Action |
| --- | --- |
| W / Up, S / Down | throttle / reverse |
| A / D, Left / Right | steer |
| Space | brake |
| R | flip upright where you are |
| C | toggle chase camera / high camera |
| Esc | quit |

## Flags

| Flag | Meaning |
| --- | --- |
| `--realistic` | loose-sand tire model: mu 0.48, grip falls off with slip, load-proportional rolling drag (see `Tuning` in `src/buggy.rs`). Default is arcade (mu 0.75). |
| `--albedo` | use the approximate de-lit `a_` tiles (converter v3) instead of the ortho `t_` tiles, where present. Lighting follows the image's sun (meta `sun`) either way. |
| `--heading DEG` | initial compass heading (0 = north, 90 = east). |
| `--spawn X,Z` | spawn at world X (east, m) / Z (south, m) instead of the automatic dune-field spawn. Positions are printed in the log. |
| `--step N` | decimation step for the far terrain mesh (must divide 144). Default: automatic, keeping the site under about 16M vertices. |
| `--high-cam` | start in the high camera. |
| `--autodrive` | full throttle with a slow sinusoidal weave. Auto-flips if the buggy sits on its roof for 3 s. |
| `--weave A` | steer amplitude for `--autodrive`, 0..1 (default 0.35; 0 = straight). |
| `--throttle T` | autodrive throttle, 0..1 (default 1). |
| `--seconds N` | exit after N seconds, counted from the first frame after loading. |
| `--shot OUT.png` | save a screenshot at `--shot-at` seconds (default 3), then exit unless `--seconds` is later. |
| `--shot-at T` | when to take the `--shot` screenshot. |
| `--log-every T` | physics log period in seconds (default 1; 0 = off). |

Each log line has this form:

```
t=  3.0 pos=(x,y,z) speed=..km/h wheels=<in contact> hag=<CoM height above ground, m> up_y=<chassis up.y>
        side=<side slope across the heading, deg> max_side_1s=<largest side slope held 1 s upright>
        rollovers=.. respawns=.. fine_chunks=.. rebuilds=.. build=<background mesh build ms>
        swap=<main-thread swap ms> fps=..
```

At rest on flat ground `hag` is about 0.67 m (the CoM) and `wheels=4`. A `ROLLOVER #n` line is
logged whenever the chassis passes ~80 deg of roll/pitch.

`tools/make_ramp_site.py OUT_DIR [--bumps 0.4]` writes a synthetic site with 20-50 deg side-slope
strips for physics tests (it prints the `--spawn`/`--heading` for each strip).
`tools/web_pack.py` makes downsampled copies of sites for the browser build (see WEB.md).

Example test run (about 20 s, below-normal priority is advisable on a shared PC):

```
marsmaker-viewer --autodrive --seconds 20 --shot out.png --shot-at 10
```

## How it works (short)

- **Vehicle**: 420 kg, CoM 0.67 m above ground, 2.0 m track (static stability factor ~1.5,
  tips at ~56 deg), explicit inertia tensor, anti-roll bars, tire forces applied at a 0.25 m roll
  centre, suspension rays intersected with the local ground plane. On the ramp test site it
  holds every strip to 50 deg without rolling. Arcade grip slides from ~37 deg; realistic sand
  (friction ellipse 0.48 / 0.58) slides earlier, especially under throttle.
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
