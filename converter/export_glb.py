"""Export a MarsMaker site (or one tile of it) to a binary glTF (.glb).

Reads D:\\marsmaker_data\\sites\\<site>\\meta.json + h_{row}_{col}.png (+ t_/a_ textures) and writes one
mesh per tile, with nodata quads left out, UVs, an embedded texture and per-vertex normals.

Coordinates follow glTF: 1 unit = 1 m, +X = east, +Y = up, +Z = south (so north = -Z). The origin is the
site's local projection centre (meta.json "projection"), or the middle of the tile grid with --center.
Each tile is a node translated to its north-west corner; its vertices are local to that corner.

Heights are absolute Mars areoid heights in metres unless --relative is given (lowest point -> Y = 0).

Examples:
  python converter/export_glb.py PSP_007018_1255_PSP_007229_1255
  python converter/export_glb.py PSP_007018_1255_PSP_007229_1255 --tile 2_1 --step 1
  python converter/export_glb.py D:\\marsmaker_data\\sites\\X --step 8 --center --relative --albedo

Only numpy + Pillow are needed. If trimesh is installed the written file is re-read as a check.
"""
import argparse
import json
import os
import struct
import sys

import numpy as np
from PIL import Image

DATA_ROOT = os.environ.get("MARS_DATA", r"D:\marsmaker_data")
SITES_DIR = os.path.join(DATA_ROOT, "sites")
OUT_DIR = os.path.join(DATA_ROOT, "glb")

# Blender add-on tints the greyscale HiRISE RED ortho toward Mars dust; do the same via baseColorFactor
MARS_TINT = [1.0, 0.62, 0.42, 1.0]
NO_TEXTURE_COLOR = [0.55, 0.36, 0.25, 1.0]

GL_FLOAT, GL_UINT = 5126, 5125
ARRAY_BUFFER, ELEMENT_ARRAY_BUFFER = 34962, 34963


# ----------------------------------------------------------------------------------------------- input
def resolve_site(arg):
    """Accept a site name, a site folder or a meta.json path."""
    cands = [arg, os.path.join(SITES_DIR, arg)]
    for c in cands:
        if os.path.isfile(c) and c.lower().endswith(".json"):
            return os.path.dirname(os.path.abspath(c))
        if os.path.isfile(os.path.join(c, "meta.json")):
            return os.path.abspath(c)
    sys.exit(f"site not found: {arg} (looked for meta.json in {cands})")


def read_png(path):
    a = np.asarray(Image.open(path))
    return a


def sample_indices(n_px, step):
    """Indices 0, step, 2*step, ... plus the last pixel so tile edges always meet their neighbour."""
    idx = np.arange(0, n_px, step)
    if idx[-1] != n_px - 1:
        idx = np.append(idx, n_px - 1)
    return idx


# ----------------------------------------------------------------------------------------------- mesh
def build_tile(raw, meta, step, y_offset):
    """raw: uint16 tile (0 = nodata). Returns dict of numpy arrays in tile-local glTF coords, or None."""
    n_rows, n_cols = raw.shape
    ri, ci = sample_indices(n_rows, step), sample_indices(n_cols, step)
    q = raw[np.ix_(ri, ci)]
    valid = q > 0
    quad_ok = valid[:-1, :-1] & valid[1:, :-1] & valid[:-1, 1:] & valid[1:, 1:]
    if not quad_ok.any():
        return None

    hmin, rng = meta["height_min_m"], meta["height_range_m"]
    h = (hmin + (q.astype(np.float64) - 1.0) * rng / 65534.0) - y_offset
    mpp = meta["metres_per_pixel"]
    xs, zs = ci * mpp, ri * mpp                     # east offset, south offset (m) per sample

    used = np.zeros_like(valid)
    used[:-1, :-1] |= quad_ok; used[1:, :-1] |= quad_ok; used[:-1, 1:] |= quad_ok; used[1:, 1:] |= quad_ok
    index = np.full(q.shape, -1, np.int64)
    index[used] = np.arange(int(used.sum()))
    rr, cc = np.nonzero(used)

    pos = np.column_stack([xs[cc], h[rr, cc], zs[rr]]).astype(np.float32)

    # normals from central differences on the sampled grid (nodata neighbours replaced by the centre value)
    hf = np.where(valid, h, np.nan)
    pad = np.pad(hf, 1, mode="edge")
    centre = pad[1:-1, 1:-1]
    def nb(a):
        return np.where(np.isnan(a), centre, a)
    h_w, h_e = nb(pad[1:-1, :-2]), nb(pad[1:-1, 2:])
    h_n, h_s = nb(pad[:-2, 1:-1]), nb(pad[2:, 1:-1])
    xp = np.pad(xs.astype(np.float64), 1, mode="edge"); zp = np.pad(zs.astype(np.float64), 1, mode="edge")
    dx = (xp[2:] - xp[:-2])[None, :]; dz = (zp[2:] - zp[:-2])[:, None]
    dhdx = (h_e - h_w) / dx; dhdz = (h_s - h_n) / dz
    nrm = np.stack([-dhdx, np.ones_like(dhdx), -dhdz], axis=-1)
    nrm /= np.linalg.norm(nrm, axis=-1, keepdims=True)
    nrm = np.nan_to_num(nrm[rr, cc], nan=0.0).astype(np.float32)
    nrm[np.all(nrm == 0, axis=1)] = (0, 1, 0)

    # UVs: glTF has v = 0 at the top (north) row; map each vertex to its pixel centre
    uv = np.column_stack([(ci[cc] + 0.5) / n_cols, (ri[rr] + 0.5) / n_rows]).astype(np.float32)

    qr, qc = np.nonzero(quad_ok)
    nw, sw = index[qr, qc], index[qr + 1, qc]
    se, ne = index[qr + 1, qc + 1], index[qr, qc + 1]
    # counter-clockwise seen from +Y (front faces up): NW-SW-SE, NW-SE-NE
    tris = np.column_stack([nw, sw, se, nw, se, ne]).reshape(-1).astype(np.uint32)
    return {"pos": pos, "nrm": nrm, "uv": uv, "idx": tris,
            "hmin": float(h[valid].min()), "hmax": float(h[valid].max()),
            "quads": int(quad_ok.sum())}


# ----------------------------------------------------------------------------------------------- glTF
class GlbWriter:
    def __init__(self):
        self.bin = bytearray()
        self.gltf = {"asset": {"version": "2.0", "generator": "MarsMaker export_glb.py"},
                     "scene": 0, "scenes": [{"nodes": []}], "nodes": [], "meshes": [],
                     "buffers": [], "bufferViews": [], "accessors": [],
                     "materials": [], "textures": [], "images": [], "samplers": []}

    def _view(self, data, target=None):
        while len(self.bin) % 4:
            self.bin.append(0)
        off = len(self.bin)
        self.bin += data
        v = {"buffer": 0, "byteOffset": off, "byteLength": len(data)}
        if target:
            v["target"] = target
        self.gltf["bufferViews"].append(v)
        return len(self.gltf["bufferViews"]) - 1

    def accessor(self, arr, kind, target, minmax=False):
        arr = np.ascontiguousarray(arr)
        view = self._view(arr.tobytes(), target)
        acc = {"bufferView": view, "componentType": GL_UINT if arr.dtype == np.uint32 else GL_FLOAT,
               "count": int(arr.shape[0]), "type": kind}
        if minmax:
            acc["min"] = [float(x) for x in np.atleast_1d(arr.min(axis=0))]
            acc["max"] = [float(x) for x in np.atleast_1d(arr.max(axis=0))]
        self.gltf["accessors"].append(acc)
        return len(self.gltf["accessors"]) - 1

    def texture(self, png_bytes, name):
        if not self.gltf["samplers"]:
            # linear / linear-mipmap-linear, clamp to edge
            self.gltf["samplers"].append({"magFilter": 9729, "minFilter": 9987, "wrapS": 33071, "wrapT": 33071})
        view = self._view(png_bytes)
        self.gltf["images"].append({"bufferView": view, "mimeType": "image/png", "name": name})
        self.gltf["textures"].append({"sampler": 0, "source": len(self.gltf["images"]) - 1})
        return len(self.gltf["textures"]) - 1

    def material(self, name, tex=None, factor=None):
        pbr = {"metallicFactor": 0.0, "roughnessFactor": 0.95, "baseColorFactor": factor or [1, 1, 1, 1]}
        if tex is not None:
            pbr["baseColorTexture"] = {"index": tex}
        self.gltf["materials"].append({"name": name, "pbrMetallicRoughness": pbr, "doubleSided": False})
        return len(self.gltf["materials"]) - 1

    def add_mesh_node(self, name, m, material, translation):
        attrs = {"POSITION": self.accessor(m["pos"], "VEC3", ARRAY_BUFFER, minmax=True),
                 "NORMAL": self.accessor(m["nrm"], "VEC3", ARRAY_BUFFER),
                 "TEXCOORD_0": self.accessor(m["uv"], "VEC2", ARRAY_BUFFER)}
        ind = self.accessor(m["idx"], "SCALAR", ELEMENT_ARRAY_BUFFER, minmax=True)
        prim = {"attributes": attrs, "indices": ind, "mode": 4, "material": material}
        self.gltf["meshes"].append({"name": name, "primitives": [prim]})
        self.gltf["nodes"].append({"name": name, "mesh": len(self.gltf["meshes"]) - 1,
                                   "translation": [float(t) for t in translation]})
        self.gltf["scenes"][0]["nodes"].append(len(self.gltf["nodes"]) - 1)

    def write(self, path, extras=None):
        for k in ("materials", "textures", "images", "samplers"):
            if not self.gltf[k]:
                del self.gltf[k]
        if extras:
            self.gltf["scenes"][0]["extras"] = extras
        while len(self.bin) % 4:
            self.bin.append(0)
        self.gltf["buffers"] = [{"byteLength": len(self.bin)}]
        js = json.dumps(self.gltf, separators=(",", ":")).encode()
        js += b" " * (-len(js) % 4)
        total = 12 + 8 + len(js) + 8 + len(self.bin)
        if total >= 2 ** 32:
            sys.exit(f"output would be {total / 2**30:.1f} GiB - over the 4 GiB GLB limit; use a larger --step")
        with open(path, "wb") as f:
            f.write(struct.pack("<III", 0x46546C67, 2, total))
            f.write(struct.pack("<II", len(js), 0x4E4F534A)); f.write(js)
            f.write(struct.pack("<II", len(self.bin), 0x004E4942)); f.write(self.bin)
        return total


# ----------------------------------------------------------------------------------------------- main
def pick_texture(site_dir, tile, row, col, prefer_albedo):
    """Returns (path, is_albedo) or (None, False). a_ tiles are de-lit albedo, t_ the HiRISE RED ortho (both grey)."""
    a = os.path.join(site_dir, f"a_{row}_{col}.png")
    t = os.path.join(site_dir, tile.get("texture") or f"t_{row}_{col}.png")
    order = [(a, True), (t, False)] if prefer_albedo else [(t, False)]
    for p, alb in order:
        if os.path.isfile(p):
            return p, alb
    return None, False


def export(site_dir, out_path, step=4, tiles_sel=None, center=False, relative=False,
           textures=True, prefer_albedo=False, validate=True):
    with open(os.path.join(site_dir, "meta.json")) as f:
        meta = json.load(f)
    site = os.path.basename(site_dir)
    y_off = meta["height_min_m"] if relative else 0.0
    tsize = meta["tile_size_m"]
    gx, gy = meta["grid_top_left_m"]["east"], meta["grid_top_left_m"]["north"]
    cols, rows = meta["grid_tiles"]
    cx = gx + cols * tsize / 2 if center else 0.0
    cn = gy - rows * tsize / 2 if center else 0.0

    w = GlbWriter()
    stats = {"tiles": 0, "verts": 0, "tris": 0, "hmin": np.inf, "hmax": -np.inf, "empty_tex": 0}
    for t in meta["tiles"]:
        row, col = t["y"], t["x"]
        if tiles_sel and (row, col) not in tiles_sel:
            continue
        raw = read_png(os.path.join(site_dir, t["height"]))
        if raw.dtype != np.uint16:
            sys.exit(f"{t['height']}: expected 16-bit PNG, got {raw.dtype}")
        m = build_tile(raw, meta, step, y_off)
        name = f"{site}_h_{row}_{col}"
        if m is None:
            print(f"  {name}: no valid quads, skipped")
            continue

        mat = None
        if textures:
            tex_path, is_alb = pick_texture(site_dir, t, row, col, prefer_albedo)
            if tex_path:
                img = np.asarray(Image.open(tex_path))
                if img.max() == 0:          # e.g. ortho failed alignment -> converter wrote an empty texture
                    stats["empty_tex"] += 1
                    mat = w.material(name + "_flat", factor=NO_TEXTURE_COLOR)
                else:
                    with open(tex_path, "rb") as f:
                        tex = w.texture(f.read(), os.path.basename(tex_path))
                    # greyscale (t_ and the current a_ tiles) gets the Mars dust tint; a true colour image would not
                    mat = w.material(name, tex, MARS_TINT if img.ndim == 2 else None)
        if mat is None:
            mat = w.material(name + "_flat", factor=NO_TEXTURE_COLOR)

        e0, n0 = t["top_left_m"]["east"], t["top_left_m"]["north"]
        w.add_mesh_node(name, m, mat, (e0 - cx, 0.0, -(n0 - cn)))
        stats["tiles"] += 1
        stats["verts"] += len(m["pos"]); stats["tris"] += len(m["idx"]) // 3
        stats["hmin"] = min(stats["hmin"], m["hmin"]); stats["hmax"] = max(stats["hmax"], m["hmax"])
        print(f"  {name}: {len(m['pos']):,} verts, {len(m['idx']) // 3:,} tris, "
              f"h {m['hmin']:.2f}..{m['hmax']:.2f} m")

    if not stats["tiles"]:
        sys.exit("nothing exported (no matching tiles with data)")
    extras = {"marsmaker_id": meta["id"], "center_lat": meta["center_lat"],
              "center_lon_east": meta["center_lon_east"], "projection": meta["projection"],
              "step": step, "relative_heights": relative, "y_offset_m": y_off,
              "origin_east_north_m": [cx, cn], "axes": "+X east, +Y up, +Z south, metres"}
    size = w.write(out_path, extras)
    print(f"wrote {out_path}: {stats['tiles']} tiles, {stats['verts']:,} verts, {stats['tris']:,} tris, "
          f"{size / 2**20:.1f} MiB, heights {stats['hmin']:.3f}..{stats['hmax']:.3f} m")
    if stats["empty_tex"]:
        print(f"  note: {stats['empty_tex']} tile texture(s) were all-nodata; used a flat Mars colour instead")
    if validate:
        check(out_path, stats, meta, y_off, step)
    return stats


def check(path, stats, meta, y_off, step):
    """Re-parse the GLB and compare against what we meant to write and against meta.json."""
    with open(path, "rb") as f:
        data = f.read()
    magic, ver, total = struct.unpack_from("<III", data, 0)
    assert magic == 0x46546C67 and ver == 2 and total == len(data), "bad GLB header"
    jl, _ = struct.unpack_from("<II", data, 12)
    g = json.loads(data[20:20 + jl])
    bl, _ = struct.unpack_from("<II", data, 20 + jl)
    binc = memoryview(data)[28 + jl:28 + jl + bl]
    ymin, ymax, nv = np.inf, -np.inf, 0
    for mesh in g["meshes"]:
        p = mesh["primitives"][0]
        acc = g["accessors"][p["attributes"]["POSITION"]]
        bv = g["bufferViews"][acc["bufferView"]]
        pos = np.frombuffer(binc[bv["byteOffset"]:bv["byteOffset"] + bv["byteLength"]], np.float32).reshape(-1, 3)
        assert np.allclose(pos.min(0), acc["min"]) and np.allclose(pos.max(0), acc["max"]), "accessor bounds"
        ia = g["accessors"][p["indices"]]
        assert ia["max"][0] < acc["count"], "index out of range"
        ymin, ymax, nv = min(ymin, acc["min"][1]), max(ymax, acc["max"][1]), nv + acc["count"]
    lo, hi = meta["height_min_m"] - y_off, meta["height_max_m"] - y_off
    tol = 1e-3 + meta["height_range_m"] / 65534.0
    ok = nv == stats["verts"] and ymin >= lo - tol and ymax <= hi + tol
    msg = (f"check: {len(g['meshes'])} meshes, {nv:,} verts, Y {ymin:.3f}..{ymax:.3f} "
           f"vs meta {lo:.3f}..{hi:.3f}")
    # the GLB must reproduce exactly the heights decoded from the PNGs (float32 rounding only)
    ok = ok and abs(ymin - (stats["hmin"])) < 1e-3 and abs(ymax - stats["hmax"]) < 1e-3
    print(msg + (" OK" if ok else " MISMATCH"))
    try:
        import trimesh
        sc = trimesh.load(path, force="scene")
        b = sc.bounds
        print(f"trimesh: {len(sc.geometry)} geometries, {sum(len(x.vertices) for x in sc.geometry.values()):,} "
              f"verts, bounds min {np.round(b[0], 2).tolist()} max {np.round(b[1], 2).tolist()}")
    except ImportError:
        pass
    if not ok:
        sys.exit(1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("site", help="site name under D:\\marsmaker_data\\sites, a site folder or its meta.json")
    ap.add_argument("--step", type=int, default=4, help="keep every Nth DTM post (1 = full 1 m detail)")
    ap.add_argument("--tile", action="append", metavar="ROW_COL",
                    help="export only this tile, e.g. 2_1 (repeatable)")
    ap.add_argument("--center", action="store_true", help="put the middle of the tile grid at X = Z = 0")
    ap.add_argument("--relative", action="store_true", help="shift heights so the site minimum is Y = 0")
    ap.add_argument("--albedo", action="store_true", help="use the de-lit a_{r}_{c}.png albedo instead of t_ when present")
    ap.add_argument("--no-texture", action="store_true", help="do not embed textures")
    ap.add_argument("--no-validate", action="store_true")
    ap.add_argument("-o", "--out", help=f"output .glb (default {OUT_DIR}\\<site>[_tile]_s<step>.glb)")
    a = ap.parse_args()
    if a.step < 1:
        sys.exit("--step must be >= 1")

    site_dir = resolve_site(a.site)
    site = os.path.basename(site_dir)
    sel = None
    if a.tile:
        sel = set()
        for s in a.tile:
            r, c = s.replace("h_", "").replace(",", "_").split("_")
            sel.add((int(r), int(c)))
    out = a.out
    if not out:
        tag = ("_" + "-".join(f"{r}_{c}" for r, c in sorted(sel))) if sel else ""
        out = os.path.join(OUT_DIR, f"{site}{tag}_s{a.step}.glb")
    out = os.path.abspath(out)
    if out[:2].upper() == "C:" and not os.environ.get("MARS_ALLOW_C"):
        sys.exit("refusing to write to C: (shared system drive) - pick another drive, or set MARS_ALLOW_C=1")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    print(f"exporting {site} step {a.step} -> {out}")
    export(site_dir, out, a.step, sel, a.center, a.relative, not a.no_texture, a.albedo, not a.no_validate)


if __name__ == "__main__":
    main()
