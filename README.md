# MarsMaker

Turns NASA HiRISE stereo terrain models (DTMs, ~1 m/post) of Mars into game-engine terrain,
and drives a dune buggy on them.

- `tools/batch_convert.py` – resumable converter for all 1,262 HiRISE DTMs (public CC0 bucket `astrogeo-ard`)
  into 2017 px tiles: `h_r_c.png` (16-bit heights), `t_r_c.png` (ortho), optional `a_r_c.png` (approximate de-lit), `meta.json`.
- `viewer/` – Bevy (Rust) dune-buggy viewer, native and WebAssembly. See `viewer/README.md`, `viewer/WEB.md`.
- `plugins/blender`, `plugins/godot` (tested), `plugins/unity`, `plugins/unreal` (untested in-engine).
- `tools/export_glb.py` – .glb export for any engine.

Code: MIT. HiRISE data: NASA/JPL/University of Arizona, public domain (CC0 via USGS Astrogeology).
