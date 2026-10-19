# MarsMaker terrain for Unreal Engine 5 (5.3+)

> **UNTESTED IN ENGINE.** Unreal was not installed on the machine where this was written.
> - `prepare_tiles.py` is pure Python and was run and checked on Russell Crater (outputs in
>   `D:/marsmaker_data/unreal_prep/`).
> - `marsmaker_unreal.py` was only smoke-tested against a stub `unreal` module. That checked the
>   matching, placement and control flow, but none of the real engine API calls.
>
> The Landscape import UI steps, the material graph and the texture settings are unchecked.
> Please report problems.

There are two pieces:

| file | runs in | does |
|---|---|---|
| `prepare_tiles.py` | plain Python 3 + numpy + Pillow | fills nodata, writes UE-ready 16-bit heightmaps, masks, tinted colour textures, and `unreal_import.json` / `IMPORT_SETTINGS.txt` with the exact transforms |
| `marsmaker_unreal.py` | Unreal Editor (Python Editor Script Plugin) | snaps the imported Landscapes to their exact transforms, labels them, and creates per-tile materials (colour + nodata opacity mask) |

Why landscapes are created by hand: Unreal's Python API has no call that creates a Landscape from a
heightmap file. A Landscape actor spawned from Python has no components. Forum reports say you get a
`LandscapePlaceholder`, and `landscape_import_heightmap_from_render_target` needs an existing landscape
plus a float render target, which is a lossy and fragile route. So creation stays in the Landscape
mode UI, which is reliable, and the script does the error-prone part (exact numbers) afterwards.

## Steps

1. **Prepare** (anywhere with Python):
   ```
   python plugins/unreal/prepare_tiles.py D:/marsmaker_data/sites/PSP_007018_1255_PSP_007229_1255 --out D:/marsmaker_data/unreal_prep
   ```
   Options: `--prefer-albedo` (use the de-lit `a_` tiles), `--no-tint`, `--fill min` (flat fill instead of
   smooth), `--r16` (also write `.r16` raw files), `--no-merged`, `--no-tiled`, `--no-stitch`.
2. **Enable plugins** in UE: *Python Editor Script Plugin* (and *Editor Scripting Utilities* on 5.3).
3. **Import each tile** in Landscape mode > Manage > **New** > **Import from File**, using
   `IMPORT_SETTINGS.txt`:
   - Heightmap file `heightmaps/<site>_h_<row>_<col>.png`
   - Section Size **63x63 quads**, Sections Per Component **2x2**, Number of Components **16x16**.
     The overall resolution should then read **2017 x 2017**. Do not let it resample.
   - Scale **X 100, Y 100, Z 115.1903668027** (Russell; the value for each site is in the txt and json)
   - Location: the *UI Location* from the table, which is the tile centre. Rotation 0.
   - Leave the material empty. Click *Import*.

   Approximate values are fine, because the next step fixes the transform exactly. The heightmap codes
   are stored raw, and the actor transform turns them into heights.
4. **Run the script** in the Output Log (Cmd) or via *Tools > Execute Python Script*:
   ```
   py "<repo>/plugins/unreal/marsmaker_unreal.py" "D:/marsmaker_data/unreal_prep/PSP_007018_1255_PSP_007229_1255/unreal_import.json"
   ```
   `--dry-run` only reports changes. `--no-material` skips textures and materials. `--folder /Game/X`
   changes where assets go (default `/Game/MarsMaker/<site>`).

   Each landscape is matched to a tile using the first rule that applies:
   1. its label contains `h_<row>_<col>`;
   2. its bounds centre is within 1/4 tile of the tile centre;
   3. its actor location is within 1/4 tile of the tile centre. This catches the case where the UI used
      the typed centre as the corner.

   Each matched landscape then gets the exact location, scale and zero rotation, and the label
   `MarsMaker_<site>_h_<r>_<c>`. Unmatched landscapes are listed.

**Single landscape alternative.** `merged/<site>_merged.png` (Russell: 4033 x 12097 = 32 x 96 components
of 126 quads) or the `tiled/<site>_x{col}_y{row}.png` set (World Partition tiled import) can be imported
as one landscape with the same Scale. Actor location (north-west corner) and UI location (centre) are in
the json. The script reports such a landscape but does not move it, because of World Partition proxies,
and does not texture it. **Unverified:** whether UE's tiled import expects tiles that share their edge
row (MarsMaker tiles overlap by 1 px). If tiles come out offset by 1 m per tile, use the merged PNG.

## Coordinates and maths

**Axes.** Unreal is left-handed, Z up, X forward, 1 uu = 1 cm. The mapping is **X = east,
Y = south (= -north), Z = up**. Facing +X (east), +Y is on your right, which is south, so the mapping is
not mirrored. The Godot importer uses (+X east, +Y up, +Z south) in metres, so Unreal (x, y, z) equals
Godot (x, z, y) * 100. The heightmap's first row (north) lands at min Y and its first column (west) at
min X. That is the natural image orientation in Unreal, so no flip is needed.

**XY.** `scale_xy = 100 * metres_per_pixel` = 100 (1 quad = 1 m). The tile actor location is
`(top_left.east * 100, -top_left.north * 100, location_z)`, which equals `meta.json`
`tiles[].unreal_location_cm`. prepare_tiles checked this for all 12 Russell tiles (difference 0).

**Z.** A Landscape stores codes v in 0..65535. Its world height is

    z_cm = location_z + (v - 32768) * scale_z / 128        (LANDSCAPE_ZSCALE = 1/128)

MarsMaker codes are `h_m = height_min + (v - 1) * range / 65534`. prepare_tiles keeps the same codes and
only fills v = 0. Equating the code step and the value at v = 1 gives the exact settings:

    scale_z    = 128 * 100 * range / 65534                  = 115.19036680268067 for Russell
    location_z = 100 * height_min + 32767 * 100 * range / 65534 = 35214.961433410645 cm

The common rule of thumb `scale_z = range * 100 / 512` (115.18685) is 128/65534 vs 1/512, which is off by
1 code over the range. For Russell that is up to 0.90 cm. The exact form is used everywhere. Both values
match `meta.json` `unreal.scale_z` / `unreal.location_z_cm` exactly.

**Valid sizes.** A landscape side has `components * sections * quads_per_section + 1` vertices, with
quads per section in {7, 15, 31, 63, 127, 255} and sections per component in {1, 2}. 2016 = 2^5 * 3^2 * 7,
so 2017 is valid as 63 quads x 2x2 sections x **16** components (recommended; matches meta.json), or as
63 quads x 1x1 x 32 components. All valid options are in `unreal_import.json` `landscape_valid_configs`.

## What prepare_tiles.py does with the data

- **Nodata fill.** A Landscape has no nodata. v = 0 posts are filled with a smooth pull-push (pyramid)
  interpolation, done once over the whole tile grid so filled values also agree across tile edges.
  Real posts are never changed, except seam posts (next point).
- **Seam stitching.** On some sites a few shared edge posts differ by 1 code between neighbouring tiles
  (Russell has 51 posts of under 1 cm, all on north-south seams). Each tile takes its north and west edge
  from its neighbour, so all seams are bit-identical. `--no-stitch` turns this off.
- **Masks.** `masks/<site>_m_<r>_<c>.png` is 255 where there is real data. The material uses it as an
  opacity mask (Masked blend, clip 0.5), so the filled area is invisible. **Collision still exists over
  the filled area.** For real holes, paint the Landscape Visibility layer, using a material with a
  *Landscape Visibility Mask* node.
- **Colour.** `color/<site>_c_<r>_<c>.png` is the grey ortho x Mars dust (1.0, 0.62, 0.42), or plain grey
  with `--no-tint`. The material maps it with `LandscapeLayerCoords`, mapping scale 2016, so UV 0..1
  spans the tile. The texture edge sits on the edge posts, up to 0.5 texel (0.5 m) off at the borders.
  The script asks UE to stretch the 2017 px textures to a power of two so they get mips.

## Checks run here (Russell Crater, PSP_007018_1255_PSP_007229_1255)

- Every possible code v = 1..65535 was pushed through the UE formula and compared with the MarsMaker
  decode. The maximum error is 1.5e-11 cm, which is float rounding.
- The written PNGs were read back. They contain no zeros, every real post is unchanged except the 51
  stitched seam posts, and the masks equal `source > 0`.
- The tiled set is identical to the per-tile set, the merged grid is identical to each tile's block, and
  every shared tile edge (including filled areas) is identical.
- Scale and location match the meta.json `unreal` block (difference 0).

## Caveats

- No engine run yet. The most likely trouble spots:
  - whether the New Landscape *Location* is treated as the centre (the script handles either case);
  - material compile details (sampler types: colour = Color/sRGB, mask = Linear Color, non-sRGB);
  - the `power_of_two_mode` enum name in your UE version;
  - whether *Masked* landscape materials behave as expected under Nanite landscapes (UE 5.3+ Nanite
    landscape may ignore opacity masks; turn Nanite off for the landscape if the fill shows).
- The site is large (Russell is about 4 x 12 km), which is fine for UE5 Large World Coordinates.
  Heights are absolute Mars areoid heights in cm (57 to 647 m for Russell). There is no "relative
  heights" option yet; subtract `location_z` by hand if you want the terrain near Z = 0.
- Re-running prepare_tiles overwrites its output folder. Re-running the script re-imports the textures
  (replace_existing) and reuses existing material instances.
