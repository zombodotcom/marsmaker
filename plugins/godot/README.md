# MarsMaker terrain for Godot 4

Loads a MarsMaker site folder (`meta.json` + `h_{row}_{col}.png` heights + `t_`/`a_` textures) as
real-scale terrain: one `MeshInstance3D` (ArrayMesh) per tile plus an optional `StaticBody3D` with a
`HeightMapShape3D` per tile. Tested with Godot 4.6.1 (Forward+, Vulkan). The add-on uses
`@export_tool_button`, so it needs Godot 4.4 or newer.

Coordinates: 1 unit = 1 m, +X = east, +Y = up, +Z = south (north = -Z). The origin is the site's local
projection centre (`meta.json` "projection"). Heights are absolute Mars areoid heights unless
*Relative heights* is on. These are the same axes `tools/export_glb.py` writes, so a GLB export and a
runtime-loaded site line up exactly.

## Install

1. Copy `plugins/godot/addons/marsmaker/` into your project, so you have `res://addons/marsmaker/plugin.cfg`.
2. Project > Project Settings > Plugins: enable **MarsMaker HiRISE terrain**.
3. Add a site with either:
   - Project > Tools > **MarsMaker: add site...**, then pick the site's `meta.json`. This adds a
     `MarsMakerTerrain` node to the open scene.
   - Or Add Node > **MarsMakerTerrain**, then set **Site Dir** to a folder such as
     `D:/marsmaker_data/sites/PSP_007018_1255_PSP_007229_1255`.

The editor builds a coarse preview (`editor_mesh_step`, default 16, without collision). The
generated tiles are not saved into the `.tscn`. They are rebuilt from the site folder whenever the
scene loads. Use the **Rebuild** button in the inspector after you change settings.

## Node settings (`MarsMakerTerrain`)

| property | default | meaning |
|---|---|---|
| `site_dir` | "" | folder containing `meta.json` (absolute path, `res://` or `user://`) |
| `mesh_step` | 4 | keep every Nth DTM post at runtime. 1 = full 1 m detail (~8M triangles per tile, heavy) |
| `editor_mesh_step` | 16 | step for the in-editor preview |
| `collision` | true | one `HeightMapShape3D` per tile (runtime only) |
| `collision_step` | 1 | collision sample spacing in posts, snapped down to a divisor of 2016 |
| `relative_heights` | false | lowest point of the site goes to Y = 0 |
| `center` | false | middle of the tile grid goes to X = Z = 0 |
| `textures` | true | use the HiRISE ortho (`t_`). Greyscale textures are tinted to Mars dust colour |
| `prefer_albedo` | false | use the de-lit greyscale `a_{row}_{col}.png` albedo tiles when they exist (tinted the same way) |

`height_at(x, z)` returns the terrain height under a local X/Z point by raycasting the collision.

## Scripting / runtime use without the node

```gdscript
var opts := MarsMakerSite.default_options()
opts.mesh_step = 8
opts.collision_step = 4
var site := MarsMakerSite.load_site("D:/marsmaker_data/sites/PSP_007018_1255_PSP_007229_1255", opts)
add_child(site)   # children are named h_{row}_{col}, each with "mesh" and "collision" children
```

## How the data is interpreted

- Heights: `uint16`, where 0 = nodata and `v` maps to `height_min_m + (v-1) * height_range_m / 65534`.
  Godot's own PNG loader reduces 16-bit PNGs to 8 bits. The add-on relabels the PNG header as 8-bit
  grey+alpha, which has the same byte layout, so libpng still decodes it in C++ and full precision is kept.
- Tiles are 2017 px with 1 px overlap (step 2016 m). Tile (row, col) is placed at its `top_left_m` from
  `meta.json`. Each mesh keeps the tile's last row and column even when `mesh_step` does not divide 2016,
  so neighbouring tiles always meet.
- Quads with any nodata corner are dropped from the mesh, which leaves holes outside the DTM footprint.
- UV (0,0) is the north-west pixel centre of the tile texture.

## Caveats

- **Collision has no holes.** `HeightMapShape3D` cannot leave samples out, so nodata samples are filled
  with the tile's lowest valid height. Outside the DTM footprint the collision is a flat floor where
  the mesh has no geometry.
- Building runs on the main thread in GDScript. Russell at step 4 with step-4 collision (12 tiles,
  1.46M vertices) takes about 4 s. Step 16 with full-resolution collision takes about 10 s. Use a
  coarse `mesh_step` and higher `collision_step` for large sites, or load tiles on demand with
  `opts.tiles = [Vector2i(row, col), ...]`.
- Exported games: PNGs inside `res://` are imported as textures, and the raw `h_*.png` files are not
  shipped. Keep site folders outside the project (absolute path, or copy them to `user://`). The
  export preset's "non-resource files" filter (`*.json`) might work instead, but it is untested.
- Sites converted before converter version 3 can have all-zero `t_` textures (failed ortho alignment).
  Those tiles are drawn in a flat Mars colour.
