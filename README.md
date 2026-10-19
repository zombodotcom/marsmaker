# MarsMaker

Turns NASA HiRISE stereo terrain models (DTMs, about 1 m per post) of Mars into real-scale
game-engine terrain, and lets you drive a dune buggy on it.

## Play in your browser

**https://zombodotcom.github.io/marsmaker/** lets you pick a dune field and drive. Nothing to install. It works
best on a desktop browser with a keyboard; phones get touch buttons.

The page is built from `deploy/` and published to the `gh-pages` branch, which is rewritten
as one orphan commit on every deploy. To update it: build the wasm (`viewer/WEB.md`), edit
`deploy/sites.txt`, then run `python deploy/deploy.py`. The script packs the listed sites with
`viewer/tools/web_pack.py`, makes thumbnails and `sites.json`, assembles
`D:\marsmaker_data\pages_build\` and force-pushes it. Use `--no-push` to build only.
`node deploy/smoke_live.mjs URL out.png` smoke-tests the live page in headless Chrome.

## Quick start

### 1. Drive the buggy

Needs Rust (stable) and a converted site folder (step 2, or one shared by a friend).

```
cd viewer
cargo build --release -j 4
target/release/marsmaker-viewer path/to/sites/PSP_007018_1255_PSP_007229_1255
```

W/S drive, A/D steer, Space brake, R flip upright, C camera, Esc quit. `--realistic` gives
loose-sand grip. All flags and the browser (WebAssembly) build: `viewer/README.md`, `viewer/WEB.md`.

### 2. Convert sites

Needs Python 3 (developed on 3.13) with GDAL-backed rasterio.

```
pip install -r converter/requirements.txt
python converter/batch_convert.py --sites PSP_007018_1255_PSP_007229_1255 --data path/to/marsmaker_data --albedo
python converter/batch_convert.py --filter dune --data path/to/marsmaker_data     # every dune site
```

Sources are the public CC0 USGS bucket `astrogeo-ard` (1,262 HiRISE DTMs, listed in
`converter/catalog.json`). Each site becomes `<data>/sites/<site>/` with 2017 px tiles:
`h_r_c.png` (16-bit heights), `t_r_c.png` (ortho texture), optional `a_r_c.png` (approximate
de-lit albedo) and `meta.json` (location, scale, height range). The run can be stopped at any
point; rerun the same command to resume. Check a site with `python converter/check_seams.py <site dir>`.

### 3. Use it in your engine

| Engine | Where | Status |
| --- | --- | --- |
| Blender | `plugins/blender` (install `marsmaker_importer.zip` as an add-on) | tested |
| Godot 4 | `plugins/godot` | tested |
| Unity | `plugins/unity` | untested in-engine |
| Unreal | `plugins/unreal` | untested in-engine |
| Anything else | `python converter/export_glb.py <site dir>` writes a .glb | |

Each plugin folder has a README.

## Folder map

```
converter/   Python pipeline: HiRISE DTM -> tiles
  batch_convert.py    resumable bulk converter (main entry point)
  hirise2engine.py    core reprojection; also a one-off single-crop CLI (writes <repo>/out/<name>/)
  catalog.json        STAC metadata for all 1,262 DTMs
  fetch_catalog.py, list_bucket.py   rebuild catalog.json from the bucket
  delight.py          approximate de-lighting (a_ tiles), used by batch_convert --albedo
  export_glb.py       site or tile -> .glb, for any engine
  check_seams.py, texture_report.py, align_check.py   QA / diagnostics
  make_comparison.py + comparison_demo.html   browser page comparing HiRISE / CTX / HRSC / MOLA resolution
viewer/      Bevy (Rust) dune-buggy viewer, native + WebAssembly
  src/                the game
  tools/              helpers: web_pack.py (downsample sites for web), make_ramp_site.py (physics test site), web_smoke.mjs
  web/                browser shell page
plugins/     engine importers: blender, godot, unity, unreal
dev/         maintainer dashboards and scratch, not needed to use MarsMaker
  mission.py / monitor.py (+ .html)   local progress dashboards for long conversion runs
  status.json         checklist shown by mission.py
  cap_test.py         window-capture experiment
```

Converted data is large and stays outside the repo (the scripts take `--data DIR`; the default
is `D:\marsmaker_data`). The scripts refuse to write bulk data to C: by default; if C: is your
only drive, set `MARS_ALLOW_C=1`.

Code: MIT. HiRISE data: NASA/JPL/University of Arizona, public domain (CC0 via USGS Astrogeology).
