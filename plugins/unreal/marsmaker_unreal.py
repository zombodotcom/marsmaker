"""MarsMaker -> Unreal Engine 5.3+ editor script (run INSIDE the Unreal Editor, Python plugin enabled).

UNTESTED IN ENGINE: written against the documented UE 5.x Python API, but never run in Unreal.

Unreal's Python API cannot create a Landscape from a heightmap file (spawning a Landscape actor from
Python gives an empty/placeholder actor), so landscape creation stays a UI step. This script does the
rest, from the unreal_import.json written by prepare_tiles.py:

  1. prints the exact per-tile Landscape import settings,
  2. finds the Landscapes you imported and snaps each one to its exact transform (location, scale,
     zero rotation) and label "MarsMaker_<site>_h_<row>_<col>",
  3. imports the tinted colour + nodata mask textures and gives every tile landscape a material
     instance (colour via LandscapeLayerCoords, opacity mask hides the filled nodata area).

Usage (Output Log, "Cmd" box, or Tools > Execute Python Script with the default JSON path edited below):
    py "C:/path/to/plugins/unreal/marsmaker_unreal.py" "D:/marsmaker_data/unreal_prep/<site>/unreal_import.json"
options:  --dry-run   only report what would change
          --no-material   do not import textures / create materials
          --folder /Game/MarsMaker   content folder for textures and materials

Axes: Unreal X = east, Y = south (= -north), Z = up, 1 uu = 1 cm. Heightmap row 0 = north (min Y).
World Z of heightmap code v: location_z + (v - 32768) * scale_z / 128  (exactly the MarsMaker height).
"""
import json
import os
import re
import sys

import unreal

DEFAULT_JSON = "D:/marsmaker_data/unreal_prep/PSP_007018_1255_PSP_007229_1255/unreal_import.json"
BASE_MATERIAL = "M_MarsMakerTile"


def log(msg):
    unreal.log("MarsMaker: " + msg)


def warn(msg):
    unreal.log_warning("MarsMaker: " + msg)


def parse_args(argv):
    opts = {"json": DEFAULT_JSON, "dry_run": False, "material": True, "folder": "/Game/MarsMaker"}
    it = iter(argv)
    for a in it:
        if a == "--dry-run":
            opts["dry_run"] = True
        elif a == "--no-material":
            opts["material"] = False
        elif a == "--folder":
            opts["folder"] = next(it).rstrip("/")
        elif a.lower().endswith(".json"):
            opts["json"] = a
    return opts


# ------------------------------------------------------------------------------------------- report
def print_settings(d):
    L = d["landscape"]
    sx, sy, sz = d["scale"]
    log("Landscape mode > Manage > New > Import from File, per tile:")
    log(f"  Section Size {L['quads_per_section']}x{L['quads_per_section']} quads, Sections Per Component "
        f"{L['sections_per_component']}, Number of Components {L['components']}x{L['components']} "
        f"(resolution {d['tile_px']}x{d['tile_px']}), Scale X {sx} Y {sy} Z {sz:.10f}")
    for t in d["tiles"]:
        u = t["ui_location_cm"]
        log(f"  h_{t['row']}_{t['col']}: file {t['heightmap']}  Location {u[0]:.1f}, {u[1]:.1f}, {u[2]:.4f}")


# ------------------------------------------------------------------------------------------- matching
def level_landscapes():
    actors = unreal.get_editor_subsystem(unreal.EditorActorSubsystem).get_all_level_actors()
    # unreal.Landscape is the main actor; LandscapeStreamingProxy actors belong to a World Partition landscape
    return [a for a in actors if isinstance(a, unreal.Landscape)]


def xy_dist(a, b):
    return ((a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2) ** 0.5


def match_tiles(d, landscapes):
    """Returns {actor: tile}. Priority: label contains h_<r>_<c>; bounds centre near the tile centre;
    actor location near the tile centre (tile centre typed into a field that was used as the corner)."""
    tiles = {(t["row"], t["col"]): t for t in d["tiles"]}
    tol = d["tiles"][0]["size_cm"] * 0.25 if d["tiles"] else 50000.0
    result, used = {}, set()
    pat = re.compile(r"h_(\d+)_(\d+)")
    for a in landscapes:
        m = pat.search(a.get_actor_label())
        if m and (int(m.group(1)), int(m.group(2))) in tiles:
            key = (int(m.group(1)), int(m.group(2)))
            if key not in used:
                result[a] = tiles[key]
                used.add(key)
    for a in landscapes:
        if a in result:
            continue
        origin, _extent = a.get_actor_bounds(False)
        loc = a.get_actor_location()
        best = None
        for hyp in ((origin.x, origin.y), (loc.x, loc.y)):
            cands = [(xy_dist(hyp, t["ui_location_cm"]), k) for k, t in tiles.items() if k not in used]
            cands = [c for c in cands if c[0] <= tol]
            if cands:
                best = min(cands)[1]
                break
        if best is not None:
            result[a] = tiles[best]
            used.add(best)
    return result


def snap(actor, d, t, site, dry):
    loc = unreal.Vector(*t["actor_location_cm"])
    scale = unreal.Vector(*d["scale"])
    label = f"MarsMaker_{site}_h_{t['row']}_{t['col']}"
    old = actor.get_actor_location()
    log(f"{actor.get_actor_label()} -> {label}: location ({old.x:.1f}, {old.y:.1f}, {old.z:.4f}) -> "
        f"({loc.x:.1f}, {loc.y:.1f}, {loc.z:.4f}), scale -> ({scale.x}, {scale.y}, {scale.z:.10f})")
    if dry:
        return
    actor.set_actor_rotation(unreal.Rotator(0.0, 0.0, 0.0), False)
    actor.set_actor_scale3d(scale)
    actor.set_actor_location(loc, False, False)
    actor.set_actor_label(label)


# ------------------------------------------------------------------------------------------- assets
def import_texture(path, folder, name, srgb):
    at = unreal.AssetToolsHelpers.get_asset_tools()
    task = unreal.AssetImportTask()
    task.set_editor_property("filename", path)
    task.set_editor_property("destination_path", folder)
    task.set_editor_property("destination_name", name)
    task.set_editor_property("automated", True)
    task.set_editor_property("replace_existing", True)
    task.set_editor_property("save", False)
    at.import_asset_tasks([task])
    tex = unreal.EditorAssetLibrary.load_asset(folder + "/" + name)
    if tex is None:
        warn(f"texture import failed: {path}")
        return None
    tex.set_editor_property("srgb", srgb)
    tex.set_editor_property("compression_settings", unreal.TextureCompressionSettings.TC_DEFAULT)
    tex.set_editor_property("address_x", unreal.TextureAddress.TA_CLAMP)
    tex.set_editor_property("address_y", unreal.TextureAddress.TA_CLAMP)
    # 2017 px is not a power of two: without this UE keeps a single mip level (no streaming, aliasing).
    # Stretching keeps the 0..1 UV mapping (padding would not, so it is not used as a fallback).
    try:
        tex.set_editor_property("power_of_two_mode", unreal.TexturePowerOfTwoSetting.STRETCH_TO_POWER_OF_TWO)
    except Exception as e:  # noqa: BLE001 - enum value missing in this engine version
        warn(f"could not set power_of_two_mode on {name} (texture stays without mips): {e}")
    unreal.EditorAssetLibrary.save_loaded_asset(tex)
    return tex


def base_material(folder, quads, default_color, default_mask):
    path = folder + "/" + BASE_MATERIAL
    if unreal.EditorAssetLibrary.does_asset_exist(path):
        return unreal.EditorAssetLibrary.load_asset(path)
    at = unreal.AssetToolsHelpers.get_asset_tools()
    mel = unreal.MaterialEditingLibrary
    mat = at.create_asset(BASE_MATERIAL, folder, unreal.Material, unreal.MaterialFactoryNew())
    mat.set_editor_property("blend_mode", unreal.BlendMode.BLEND_MASKED)
    mat.set_editor_property("opacity_mask_clip_value", 0.5)
    # UV = landscape vertex coordinate / mapping_scale -> 0..1 across one tile (V grows southwards = image down)
    coords = mel.create_material_expression(mat, unreal.MaterialExpressionLandscapeLayerCoords, -900, 0)
    coords.set_editor_property("mapping_scale", float(quads))
    col = mel.create_material_expression(mat, unreal.MaterialExpressionTextureSampleParameter2D, -550, -150)
    col.set_editor_property("parameter_name", "Color")
    col.set_editor_property("texture", default_color)
    col.set_editor_property("sampler_type", unreal.MaterialSamplerType.SAMPLERTYPE_COLOR)
    msk = mel.create_material_expression(mat, unreal.MaterialExpressionTextureSampleParameter2D, -550, 200)
    msk.set_editor_property("parameter_name", "Mask")
    msk.set_editor_property("texture", default_mask)
    msk.set_editor_property("sampler_type", unreal.MaterialSamplerType.SAMPLERTYPE_LINEAR_COLOR)
    rough = mel.create_material_expression(mat, unreal.MaterialExpressionConstant, -300, 450)
    rough.set_editor_property("r", 0.95)
    spec = mel.create_material_expression(mat, unreal.MaterialExpressionConstant, -300, 550)
    spec.set_editor_property("r", 0.3)
    mel.connect_material_expressions(coords, "", col, "UVs")
    mel.connect_material_expressions(coords, "", msk, "UVs")
    mel.connect_material_property(col, "RGB", unreal.MaterialProperty.MP_BASE_COLOR)
    mel.connect_material_property(msk, "R", unreal.MaterialProperty.MP_OPACITY_MASK)
    mel.connect_material_property(rough, "", unreal.MaterialProperty.MP_ROUGHNESS)
    mel.connect_material_property(spec, "", unreal.MaterialProperty.MP_SPECULAR)
    mel.recompile_material(mat)
    unreal.EditorAssetLibrary.save_loaded_asset(mat)
    return mat


def tile_material(folder, site, t, parent):
    mel = unreal.MaterialEditingLibrary
    name = f"MI_{site}_h_{t['row']}_{t['col']}"
    tex_c = import_texture(t["color"], folder, f"T_{site}_c_{t['row']}_{t['col']}", True) if t.get("color") else None
    tex_m = import_texture(t["mask"], folder, f"T_{site}_m_{t['row']}_{t['col']}", False)
    path = folder + "/" + name
    if unreal.EditorAssetLibrary.does_asset_exist(path):
        mi = unreal.EditorAssetLibrary.load_asset(path)
    else:
        at = unreal.AssetToolsHelpers.get_asset_tools()
        mi = at.create_asset(name, folder, unreal.MaterialInstanceConstant, unreal.MaterialInstanceConstantFactoryNew())
    mel.set_material_instance_parent(mi, parent)
    if tex_c:
        mel.set_material_instance_texture_parameter_value(mi, "Color", tex_c)
    if tex_m:
        mel.set_material_instance_texture_parameter_value(mi, "Mask", tex_m)
    mel.update_material_instance(mi)
    unreal.EditorAssetLibrary.save_loaded_asset(mi)
    return mi


# ------------------------------------------------------------------------------------------- main
def main(argv):
    o = parse_args(argv)
    if not os.path.isfile(o["json"]):
        unreal.log_error(f"MarsMaker: {o['json']} not found. Run plugins/unreal/prepare_tiles.py first.")
        return
    with open(o["json"]) as f:
        d = json.load(f)
    if d.get("format") != "marsmaker-unreal-prep/1":
        warn("unexpected JSON format " + str(d.get("format")))
    site = d["site"]
    folder = f"{o['folder']}/{site}"
    print_settings(d)

    lands = level_landscapes()
    if not lands:
        warn("no Landscape actors in the open level. Import the heightmaps with the settings above, then run again.")
        return
    tile_size = d["tiles"][0]["size_cm"] if d["tiles"] else 0
    merged = d.get("merged")
    # a single landscape covering the whole grid (merged / World Partition tiled import)
    big = []
    for a in lands:
        _o, ext = a.get_actor_bounds(False)
        if ext.x * 2 > tile_size * 1.5 or ext.y * 2 > tile_size * 1.5:
            big.append(a)
    for a in big:
        m = merged or {}
        exp = m.get("actor_location_cm")
        loc = a.get_actor_location()
        log(f"{a.get_actor_label()} looks like a whole-site landscape (merged or World Partition tiled import). "
            f"Expected actor location {exp}, scale {d['scale']}; actual location ({loc.x:.1f}, {loc.y:.1f}, {loc.z:.4f}). "
            "Not moved automatically (World Partition proxies); set the transform by hand if it differs.")

    matches = match_tiles(d, [a for a in lands if a not in big])
    unmatched = [a.get_actor_label() for a in lands if a not in big and a not in matches]
    if unmatched:
        warn("landscapes not matched to a tile (rename to contain h_<row>_<col> to force): " + ", ".join(unmatched))
    if not matches:
        return

    with unreal.ScopedEditorTransaction("MarsMaker place tiles"):
        for a, t in sorted(matches.items(), key=lambda kv: (kv[1]["row"], kv[1]["col"])):
            snap(a, d, t, site, o["dry_run"])

    if o["material"] and not o["dry_run"]:
        first = next((t for t in d["tiles"] if t.get("color")), d["tiles"][0])
        c0 = import_texture(first["color"], folder, f"T_{site}_c_{first['row']}_{first['col']}", True) if first.get("color") else None
        m0 = import_texture(first["mask"], folder, f"T_{site}_m_{first['row']}_{first['col']}", False)
        parent = base_material(folder, d["tile_px"] - 1, c0, m0)
        with unreal.ScopedSlowTask(len(matches), "MarsMaker materials") as task:
            task.make_dialog(True)
            for a, t in matches.items():
                if task.should_cancel():
                    break
                task.enter_progress_frame(1, f"h_{t['row']}_{t['col']}")
                mi = tile_material(folder, site, t, parent)
                a.set_editor_property("landscape_material", mi)
    done = sorted(f"h_{t['row']}_{t['col']}" for t in matches.values())
    missing = sorted(f"h_{t['row']}_{t['col']}" for t in d["tiles"] if t not in matches.values())
    log(f"placed {len(done)} tile landscapes: {', '.join(done)}")
    if missing:
        log("tiles not in the level yet: " + ", ".join(missing))


if __name__ == "__main__":
    main(sys.argv[1:])
