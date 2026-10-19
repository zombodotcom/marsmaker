"""MarsMaker importer: load a converted HiRISE DTM site (meta.json + h_/t_ tiles) as real-scale terrain.

Coordinates: 1 Blender unit = 1 m, X = east, Y = north, Z = up, origin = the site's local projection
centre (see meta.json "projection"). Heights are absolute Mars areoid heights unless "Relative
heights" is on, in which case the site's lowest point sits at Z = 0.
"""
import json
import os

import bpy
import numpy as np
from bpy.props import BoolProperty, IntProperty, StringProperty
from bpy_extras.io_utils import ImportHelper

bl_info = {
    "name": "MarsMaker HiRISE terrain importer",
    "author": "MarsMaker contributors",
    "version": (0, 1, 0),
    "blender": (4, 2, 0),
    "location": "File > Import > MarsMaker site (meta.json)",
    "description": "Import HiRISE DTM sites converted by MarsMaker as 1:1 textured terrain",
    "category": "Import-Export",
}


def read_image(path, channels=1):
    """Load a PNG through Blender (16-bit stays lossless as float) -> array [rows, cols], row 0 = north."""
    img = bpy.data.images.load(path, check_existing=False)
    img.colorspace_settings.name = "Non-Color"
    w, h = img.size
    buf = np.empty(w * h * 4, np.float32)
    img.pixels.foreach_get(buf)
    bpy.data.images.remove(img)
    return buf.reshape(h, w, 4)[::-1, :, 0]          # Blender stores bottom row first


def build_tile_mesh(name, raw, meta, tile, step, relative):
    """raw: uint16-scaled heights (0 = nodata). Returns a mesh with holes where there is no data."""
    q = raw[::step, ::step]
    rows, cols = q.shape
    valid = q > 0
    hmin, rng = meta["height_min_m"], meta["height_range_m"]
    z = hmin + (q - 1.0) * rng / 65534.0
    if relative:
        z -= hmin
    mpp = meta["metres_per_pixel"] * step
    east0, north0 = tile["top_left_m"]["east"], tile["top_left_m"]["north"]

    # keep only vertices that belong to at least one fully valid quad
    quad_ok = valid[:-1, :-1] & valid[1:, :-1] & valid[:-1, 1:] & valid[1:, 1:]
    used = np.zeros_like(valid)
    used[:-1, :-1] |= quad_ok; used[1:, :-1] |= quad_ok; used[:-1, 1:] |= quad_ok; used[1:, 1:] |= quad_ok
    index = np.full(q.shape, -1, np.int64)
    index[used] = np.arange(used.sum())
    if not quad_ok.any():
        return None

    rr, cc = np.nonzero(used)
    verts = np.column_stack([east0 + cc * mpp, north0 - rr * mpp, z[rr, cc]]).astype(np.float32)

    qr, qc = np.nonzero(quad_ok)
    # counter-clockwise seen from above (+Z): NW, SW, SE, NE
    quads = np.column_stack([index[qr, qc], index[qr + 1, qc], index[qr + 1, qc + 1], index[qr, qc + 1]])
    n_poly = len(quads)

    mesh = bpy.data.meshes.new(name)
    mesh.vertices.add(len(verts))
    mesh.vertices.foreach_set("co", verts.ravel())
    mesh.loops.add(n_poly * 4)
    mesh.loops.foreach_set("vertex_index", quads.ravel().astype(np.int32))
    mesh.polygons.add(n_poly)
    mesh.polygons.foreach_set("loop_start", np.arange(0, n_poly * 4, 4, dtype=np.int32))
    mesh.polygons.foreach_set("loop_total", np.full(n_poly, 4, np.int32))

    # UVs: texture pixel (row, col) -> (u, v) with v = 0 at the south edge
    full = raw.shape[0] - 1
    loop_rc = np.column_stack([qr, qc, qr + 1, qc, qr + 1, qc + 1, qr, qc + 1]).reshape(-1, 2) * step
    uv = np.column_stack([loop_rc[:, 1] / full, 1.0 - loop_rc[:, 0] / full]).astype(np.float32)
    mesh.uv_layers.new(name="UVMap").data.foreach_set("uv", uv.ravel())

    mesh.update(calc_edges=True)
    mesh.validate(clean_customdata=False)
    mesh.polygons.foreach_set("use_smooth", np.ones(n_poly, bool))
    return mesh


def texture_material(name, path):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    bsdf = nt.nodes.get("Principled BSDF")
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = bpy.data.images.load(path, check_existing=True)
    tex.image.colorspace_settings.name = "sRGB"
    tex.interpolation = "Cubic"
    tex.location = (-600, 300)
    # HiRISE RED orthos are greyscale; tint toward Mars dust so renders look like Mars, not the Moon
    tint = nt.nodes.new("ShaderNodeMix")
    tint.data_type = "RGBA"
    tint.blend_type = "MULTIPLY"
    tint.inputs["Factor"].default_value = 1.0
    tint.inputs[7].default_value = (1.0, 0.62, 0.42, 1.0)
    tint.location = (-300, 300)
    nt.links.new(tex.outputs["Color"], tint.inputs[6])
    nt.links.new(tint.outputs[2], bsdf.inputs["Base Color"])
    bsdf.inputs["Roughness"].default_value = 0.95
    return mat


def import_site(meta_path, step=1, relative=True, textures=True, report=print):
    site_dir = os.path.dirname(meta_path)
    with open(meta_path) as f:
        meta = json.load(f)
    site = os.path.basename(site_dir)
    coll = bpy.data.collections.new(site)
    bpy.context.scene.collection.children.link(coll)
    coll["marsmaker_id"] = meta["id"]
    coll["center_lat"], coll["center_lon_east"] = meta["center_lat"], meta["center_lon_east"]
    coll["projection"] = meta["projection"]

    n_faces = 0
    for t in meta["tiles"]:
        raw = read_image(os.path.join(site_dir, t["height"])) * 65535.0
        raw = np.rint(raw)
        name = f"{site}_{os.path.splitext(t['height'])[0]}"
        mesh = build_tile_mesh(name, raw, meta, t, step, relative)
        if mesh is None:
            continue
        tex_path = os.path.join(site_dir, t.get("texture") or "")
        if textures and t.get("texture") and os.path.isfile(tex_path):
            mesh.materials.append(texture_material(name, tex_path))
        obj = bpy.data.objects.new(name, mesh)
        coll.objects.link(obj)
        n_faces += len(mesh.polygons)
    report(f"MarsMaker: imported {site} ({len(coll.objects)} tiles, {n_faces:,} faces, step {step})")
    return coll


class IMPORT_OT_marsmaker_site(bpy.types.Operator, ImportHelper):
    """Import a MarsMaker HiRISE site (select its meta.json)"""
    bl_idname = "import_scene.marsmaker_site"
    bl_label = "Import MarsMaker site"
    bl_options = {"REGISTER", "UNDO"}

    filename_ext = ".json"
    filter_glob: StringProperty(default="meta.json;*.json", options={"HIDDEN"})
    step: IntProperty(name="Resolution step", default=4, min=1, max=64,
                      description="1 = every DTM post (full 1:1 detail, ~4M faces per tile); 4 = every 4th post")
    relative: BoolProperty(name="Relative heights", default=True,
                           description="Put the site's lowest point at Z = 0 instead of absolute Mars height")
    textures: BoolProperty(name="HiRISE texture", default=True)

    def execute(self, context):
        import_site(self.filepath, self.step, self.relative, self.textures,
                    report=lambda m: self.report({"INFO"}, m))
        return {"FINISHED"}


def menu_func(self, context):
    self.layout.operator(IMPORT_OT_marsmaker_site.bl_idname, text="MarsMaker site (meta.json)")


def register():
    bpy.utils.register_class(IMPORT_OT_marsmaker_site)
    bpy.types.TOPBAR_MT_file_import.append(menu_func)


def unregister():
    bpy.types.TOPBAR_MT_file_import.remove(menu_func)
    bpy.utils.unregister_class(IMPORT_OT_marsmaker_site)
