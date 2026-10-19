# MarsMaker terrain importer for Unity

> **UNTESTED IN ENGINE.** Unity was not installed on the machine where this package was written. These
> parts were verified outside Unity:
> - The PNG decoder, resampler, hole mask and mesh builder (`Editor/Core`, pure C#) were run with
>   `dotnet` on all 36 PNGs of the Russell Crater site. Their output was compared against Pillow/numpy.
>   Pixel values, sums, nodata counts, resampled values at sample points, mesh vertex/triangle counts
>   and shared tile edges all match.
> - The whole package compiles with 0 errors against Unity 2021.1 reference assemblies (C# 9).
>
> Nothing has been run in the Unity Editor yet: terrain creation, holes, layers, materials and the
> window are all unchecked. Please report problems.

Imports a MarsMaker site folder (`meta.json` + `h_{row}_{col}.png` heights + `t_`/`a_` textures) as
real-scale terrain. You get one **Unity Terrain per tile**, with holes where there is no data,
neighbours connected and a Mars-tinted terrain layer. You can import **one mesh per tile** instead.

Target: Unity 2022.3 LTS and Unity 6, with the Built-in, URP or HDRP render pipeline.

## Install

The package is the folder `plugins/unity/com.marsmaker.terrain/`. Use either method:

- **Package Manager:** Window > Package Manager > **+** > *Add package from disk...*, then select
  `plugins/unity/com.marsmaker.terrain/package.json`.
- Or copy the `com.marsmaker.terrain` folder into your project's `Packages/` folder.

Then open **Tools > MarsMaker > Import Site**, browse to a site folder such as
`D:/marsmaker_data/sites/PSP_007018_1255_PSP_007229_1255` and click **Import**.

Generated assets go to `Assets/MarsMaker/<site>/`: TerrainData, TerrainLayers, tinted colour PNGs,
meshes and materials. They are overwritten when you import again. The scene gets a root object named
after the site, with a `MarsMakerSiteInfo` component that holds the georeference
(`ProjectionToLocal` / `LocalToProjection`).

## Coordinates (and how they map from the other MarsMaker importers)

| | east | up | north | unit |
|---|---|---|---|---|
| MarsMaker (projection) | east | height | north | m |
| Blender importer | +X | +Z | +Y | m |
| Godot importer / glTF (right-handed, Y up) | +X | +Y | **-Z** | m |
| **Unity (left-handed, Y up)** | **+X** | **+Y** | **+Z** | m |

Unity is left-handed, so east/up/north maps to +X/+Y/+Z without mirroring: facing +Z (north), +X (east)
is on your right. Godot is right-handed and needs north = -Z for the same thing. A point at
(east, north, h) is at Unity local `(east - originEast, h - yOffset, north - originNorth)`.

- The origin is the site's local projection centre (`meta.json` "projection"). With **Center** on, it
  is the middle of the tile grid.
- Heights are absolute Mars areoid heights. With **Relative heights** on, the site minimum is at Y = 0.

## How a tile becomes a Unity Terrain

- **Placement.** A Terrain's origin is its south-west corner, so tile (row, col) is placed at
  `(top_left.east, position_y, top_left.north - 2016)`. This is `meta.json` `tiles[].unity_position_m`.
- **Height.** `terrainData.size.y = range * 65535 / 65534` and `position.y = height_min - range / 65534`,
  which are `meta.json` `unity.terrain_size_m[1]` and `unity.position_y_m`. A normalised height of
  `v / 65535` then gives `position.y + v/65535 * size.y = height_min + (v - 1) * range / 65534`. That is
  exactly the MarsMaker decoding. The importer uses the meta.json values and cross-checks them against
  this formula.
- **Resolution.** Unity heightmaps must be 2^n + 1 posts per side. MarsMaker tiles are 2017 posts
  (2016 m), which is not a valid size. The importer resamples each tile over the same 2016 m extent:
  - **2049** (default): about 0.984 m post spacing, bilinear upsampling.
  - **1025**: about 1.97 m spacing, downsampled with a tent filter whose radius equals the sample
    spacing, to reduce aliasing.

  Output post i sits at source position `i * 2016 / (n - 1)`. The first and last rows and columns
  therefore fall exactly on the tile edges, and the edge rows are filtered only along the edge, so
  neighbouring terrains get identical edge heights. This was checked for every seam on Russell.
  Padding 2017 to 2049 was rejected: the terrains would overlap their neighbours by 32 m, which breaks
  neighbour connection and LOD seams. Cropping was rejected because it loses data.
- **Nodata and holes.** A resampled post is nodata if any source post in its bilinear cell is nodata. A
  terrain quad becomes a hole (`TerrainData.SetHoles`, false = hole) if any of its 4 corners is nodata.
  This is the same rule the Blender and Godot importers use to drop quads. Nodata heights under the
  holes are filled by growing the valid area for 8 passes, then with the tile minimum. That keeps
  coarse LODs plausible near edges. `TerrainCollider` respects holes.
- **Seams.** In some converted sites a few shared edge posts differ by 1 code, under 1 cm (Russell has
  51 such posts, all on north-south seams). With **Stitch tile edges** on (the default), each tile copies
  its north and west edge from the already-imported neighbour, so seams are exact.
  `Terrain.SetNeighbors` and `allowAutoConnect` with a shared `groupingID` connect the LODs.
- **Texture.** One TerrainLayer per tile, with `tileSize` equal to the tile size, so the image covers the
  tile exactly once. The grey HiRISE ortho (`t_`), or the approximate de-lit `a_` with **Prefer de-lit
  albedo**, is decoded and multiplied by the Mars dust colour (1.0, 0.62, 0.42). The result is written
  as an RGB PNG asset. The tint is baked in because the TerrainLayer's diffuse remap is not honoured by
  every pipeline. Multiplying the sRGB-encoded values is close to multiplying linear values, because
  gamma is roughly a power law. All-zero textures, which the converter writes when ortho alignment
  failed, fall back to a flat Mars colour.

## Mesh mode

This mode ports the Godot `build_mesh`. Every Nth post is kept (**Mesh step**; the last row and column
are always kept, so tiles meet). Quads with a nodata corner are dropped. Normals use central
differences, which are continuous across tiles. Indices are 32-bit. UV (0,0) is the south-west corner,
and each vertex samples its own pixel centre. Each tile is a GameObject at its north-west corner.
**Mesh collider** is optional. Step 1 is about 8M triangles per tile.

## Why the importer has its own PNG decoder

`Texture2D.LoadImage` reduces 16-bit PNGs to 8 bits, which would destroy the height encoding.
`Editor/Core/MarsMakerPng.cs` is a small PNG decoder. It handles 8/16-bit grey, grey+alpha, RGB and
RGBA, with all 5 filter types, but not interlaced or palette images. It parses chunks, joins the IDAT
data, skips the 2-byte zlib header and inflates with `System.IO.Compression.DeflateStream`. `ZLibStream`
is .NET 6+ and not available in Unity. On Russell it decodes a 2017x2017 16-bit tile in 35-100 ms on
.NET 9. Mono in the Editor will be slower.

To re-run the verification (needs .NET SDK 6+, Python with numpy and Pillow):

```
cd plugins/unity/com.marsmaker.terrain/Tests~/CoreTest
dotnet run -c Release -- D:/marsmaker_data/sites/<site> out.json
python verify_core.py D:/marsmaker_data/sites/<site> out.json      # prints "ALL OK"
```

`Tests~` is ignored by Unity (trailing `~`). Running it creates `bin/` and `obj/` there.

## Caveats and things to check in the Editor

- **Precision.** Unity stores terrain heights with about 15-bit precision (0..32766). The vertical step
  is about `size.y / 32766`, roughly 1.8 cm for Russell. The source steps are about 0.9 cm. The
  mesh mode keeps full float precision.
- **URP.** Terrain holes need *Terrain Holes* enabled on the URP asset. The importer uses the
  pipeline's default terrain material, looked up by reflection. If terrains render pink, assign a
  terrain material on the Terrain component.
- **Built-in pipeline.** The importer sets `Default-Terrain-Standard.mat` as the material template.
- **Re-import** overwrites assets with the same names. Delete the old scene objects yourself.
- **Memory and time.** 2049 resolution uses about 16 MB of floats per tile while building. Import runs
  on the main thread with a cancelable progress bar.
- **Texture alignment.** The terrain layer maps the texture edge-to-edge over the posts, so texels are
  up to half a pixel (0.5 m) off at the tile borders. The mesh mode samples pixel centres like the
  Godot importer.
- **DeflateStream.** It is part of .NET Standard 2.1, the default API level. If a project on the .NET
  Framework API level reports a missing `System.IO.Compression`, switch Api Compatibility Level to
  .NET Standard 2.1.
