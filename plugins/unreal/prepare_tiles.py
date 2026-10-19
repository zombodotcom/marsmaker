#!/usr/bin/env python3
"""Prepare a MarsMaker site for Unreal Engine 5 Landscape import (pure Python: numpy + Pillow).

    python prepare_tiles.py <site_dir> [--out D:/marsmaker_data/unreal_prep] [--fill pullpush|min]
                            [--no-merged] [--no-tiled] [--r16] [--prefer-albedo] [--no-tint]

Writes into <out>/<site>/:
  heightmaps/<site>_h_{row}_{col}.png   16-bit grey, SAME encoding as MarsMaker (v 1..65535) but with the
                                        nodata (v = 0) area filled, because a Landscape cannot store nodata
  masks/<site>_m_{row}_{col}.png        8-bit grey, 255 = real DTM data, 0 = filled (used as opacity mask)
  color/<site>_c_{row}_{col}.png        8-bit RGB HiRISE ortho (t_, or a_ with --prefer-albedo) x Mars tint
  tiled/<site>_x{col}_y{row}.png        same heightmaps named for World Partition tiled import
  merged/<site>_merged.png (+ mask)     whole grid as one heightmap (alternative to the tiled set)
  unreal_import.json                    exact Landscape transforms + settings read by marsmaker_unreal.py
  IMPORT_SETTINGS.txt                   the same numbers for typing into the Landscape mode UI

Height maths (UE Landscape): world_z_cm = location_z + (v - 32768) * scale_z / 128.
MarsMaker: height_m = height_min + (v - 1) * range / 65534.  Equating per code step and at v = 1:
    scale_z    = 128 * 100 * range / 65534                (exact; ~= range * 100 / 512)
    location_z = 100 * height_min + (32768 - 1) * 100 * range / 65534
Unreal axes: X = east, Y = SOUTH (= -north), Z = up, centimetres. Heightmap row 0 = north = min Y.
"""
import argparse
import json
import math
import os
import sys
import time

import numpy as np
from PIL import Image

MARS_TINT = (1.0, 0.62, 0.42)
UE_MID = 32768
SECTION_QUADS = (7, 15, 31, 63, 127, 255)


# ----------------------------------------------------------------------------------------------- maths
def ue_scale_z(rng):
    return 128.0 * 100.0 * rng / 65534.0


def ue_location_z(hmin, rng):
    return 100.0 * hmin + (UE_MID - 1) * 100.0 * rng / 65534.0


def marsmaker_cm(v, hmin, rng):
    return 100.0 * (hmin + (np.asarray(v, np.float64) - 1.0) * rng / 65534.0)


def ue_cm(v, loc_z, scale_z):
    return loc_z + (np.asarray(v, np.float64) - UE_MID) * scale_z / 128.0


def landscape_configs(n_verts, max_components=32):
    """All (quads per section, sections per component, components) with (n-1) = comps * quads * sections."""
    out = []
    q_total = n_verts - 1
    for q in SECTION_QUADS:
        for s in (1, 2):
            per = q * s
            if q_total % per == 0:
                comps = q_total // per
                if comps <= max_components:
                    out.append({"quads_per_section": q, "sections_per_component": f"{s}x{s}",
                                "quads_per_component": per, "components": comps})
    return out


# ----------------------------------------------------------------------------------------------- filling
def _bilinear_up(c, shape):
    """Upsample coarse level c (h2, w2) to shape (h, w) where fine i <-> coarse (i + 0.5) / 2 - 0.5."""
    def weights(n_fine, n_coarse):
        x = (np.arange(n_fine) + 0.5) / 2.0 - 0.5
        x = np.clip(x, 0, n_coarse - 1)
        i0 = np.floor(x).astype(int)
        i1 = np.minimum(i0 + 1, n_coarse - 1)
        return i0, i1, x - i0
    r0, r1, fr = weights(shape[0], c.shape[0])
    c0, c1, fc = weights(shape[1], c.shape[1])
    top = c[r0][:, c0] * (1 - fc) + c[r0][:, c1] * fc
    bot = c[r1][:, c0] * (1 - fc) + c[r1][:, c1] * fc
    return top * (1 - fr)[:, None] + bot * fr[:, None]


def fill_pullpush(h, valid):
    """Smooth hole filling (pull-push / push-pull pyramid). Valid samples are returned unchanged."""
    s = np.where(valid, h, 0.0).astype(np.float32)
    w = valid.astype(np.float32)
    pyr = []
    while True:
        pyr.append((s, w))
        if max(s.shape) <= 1:
            break
        H, W = s.shape
        ph, pw = H % 2, W % 2
        sp = np.pad(s, ((0, ph), (0, pw)))
        wp = np.pad(w, ((0, ph), (0, pw)))
        s = sp.reshape(sp.shape[0] // 2, 2, sp.shape[1] // 2, 2).sum(axis=(1, 3))
        w = wp.reshape(wp.shape[0] // 2, 2, wp.shape[1] // 2, 2).sum(axis=(1, 3))
    # push: coarse -> fine
    s, w = pyr[-1]
    filled = np.where(w > 0, s / np.maximum(w, 1e-12), 0.0)
    for s, w in reversed(pyr[:-1]):
        up = _bilinear_up(filled, s.shape)
        avg = s / np.maximum(w, 1e-12)
        a = np.clip(w, 0.0, 1.0)            # fully trusted where at least one sample's worth of weight
        filled = a * avg + (1 - a) * up
    out = np.where(valid, h, filled)
    return out


def fill_nodata(raw, mode):
    valid = raw > 0
    if valid.all():
        return raw.copy(), valid
    if not valid.any():
        return np.full_like(raw, UE_MID), valid
    if mode == "min":
        out = raw.copy()
        out[~valid] = raw[valid].min()
        return out, valid
    f = fill_pullpush(raw.astype(np.float32), valid)
    out = np.clip(np.rint(f), 1, 65535).astype(np.uint16)
    out[valid] = raw[valid]
    return out, valid


# ----------------------------------------------------------------------------------------------- IO
def read_u16(path):
    im = Image.open(path)
    if im.mode not in ("I;16", "I;16B", "I", "L"):
        raise ValueError(f"{path}: unexpected PNG mode {im.mode}")
    a = np.array(im)
    return a.astype(np.uint16)


def write_u16_png(path, a):
    a = np.ascontiguousarray(a.astype(np.uint16))
    Image.fromarray(a).save(path, optimize=False, compress_level=6)


def write_colour(src_path, dst_path, tint):
    g = np.array(Image.open(src_path).convert("L")).astype(np.float64)
    if not g.any():
        return False
    rgb = np.stack([np.rint(g * t) for t in (tint if tint else (1.0, 1.0, 1.0))], axis=-1)
    Image.fromarray(np.clip(rgb, 0, 255).astype(np.uint8), "RGB").save(dst_path, compress_level=6)
    return True


# ----------------------------------------------------------------------------------------------- main
def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("site_dir")
    ap.add_argument("--out", default=None, help="output root (default: <site_dir>/../../unreal_prep)")
    ap.add_argument("--fill", choices=("pullpush", "min"), default="pullpush")
    ap.add_argument("--no-stitch", action="store_true", help="do not copy shared edge posts from north/west neighbours")
    ap.add_argument("--no-merged", action="store_true")
    ap.add_argument("--no-tiled", action="store_true")
    ap.add_argument("--no-color", action="store_true")
    ap.add_argument("--r16", action="store_true", help="also write little-endian .r16 raw heightmaps")
    ap.add_argument("--prefer-albedo", action="store_true", help="use a_ (approximate de-lit) instead of t_")
    ap.add_argument("--no-tint", action="store_true")
    args = ap.parse_args(argv)

    site_dir = os.path.abspath(args.site_dir)
    site = os.path.basename(site_dir.rstrip("/\\"))
    out_root = args.out or os.path.join(os.path.dirname(os.path.dirname(site_dir)), "unreal_prep")
    out = os.path.join(out_root, site)
    for d in ("heightmaps", "masks", "color", "tiled", "merged"):
        if (d == "tiled" and args.no_tiled) or (d == "merged" and args.no_merged) or (d == "color" and args.no_color):
            continue
        os.makedirs(os.path.join(out, d), exist_ok=True)
    meta = json.load(open(os.path.join(site_dir, "meta.json")))
    hmin, rng = float(meta["height_min_m"]), float(meta["height_range_m"])
    mpp = float(meta.get("metres_per_pixel", 1.0))
    tile_px = int(meta["tile_px"])
    step = tile_px - 1
    cols, rows = meta["grid_tiles"]
    gtl = meta["grid_top_left_m"]
    t_start = time.time()

    scale_xy = 100.0 * mpp
    scale_z = ue_scale_z(rng)
    loc_z = ue_location_z(hmin, rng)
    checks = {}
    ue_meta = meta.get("unreal") or {}
    if ue_meta:
        checks["meta_scale_z_diff"] = abs(ue_meta.get("scale_z", scale_z) - scale_z)
        checks["meta_location_z_diff_cm"] = abs(ue_meta.get("location_z_cm", loc_z) - loc_z)
        checks["meta_scale_xy_diff"] = abs(ue_meta.get("scale_xy", scale_xy) - scale_xy)
    # round trip over every possible code: UE world z vs MarsMaker decode
    v = np.arange(1, 65536, dtype=np.float64)
    err = np.abs(ue_cm(v, loc_z, scale_z) - marsmaker_cm(v, hmin, rng))
    checks["ue_vs_marsmaker_max_abs_err_cm_all_codes"] = float(err.max())
    approx = rng * 100.0 / 512.0
    checks["approx_scale_z_range_x100_div_512"] = approx
    checks["approx_scale_z_worst_error_cm"] = float(np.abs(
        ue_cm(v, ue_location_z(hmin, rng), approx) - marsmaker_cm(v, hmin, rng)).max())

    tiles_out = []
    south_rows, east_cols = {}, {}
    present = {(int(t["y"]), int(t["x"])): t for t in meta["tiles"]}
    max_unchanged_err = 0
    total_stitched = 0

    # pass 1: read + stitch shared edges, assemble the whole grid (0 = nodata, also for missing tiles)
    grid = np.zeros((rows * step + 1, cols * step + 1), np.uint16)
    stitched_n, raws = {}, {}
    for (r, c) in sorted(present):
        t = present[(r, c)]
        raw = read_u16(os.path.join(site_dir, t["height"]))
        if raw.shape != (tile_px, tile_px):
            raise ValueError(f"{t['height']}: shape {raw.shape}, expected {tile_px}")
        src = raw.copy()
        if not args.no_stitch:
            if (r - 1, c) in south_rows:
                raw[0, :] = south_rows[(r - 1, c)]
            if (r, c - 1) in east_cols:
                raw[:, 0] = east_cols[(r, c - 1)]
            south_rows[(r, c)] = raw[-1, :].copy()
            east_cols[(r, c)] = raw[:, -1].copy()
        stitched_n[(r, c)] = int((src != raw).sum())
        total_stitched += stitched_n[(r, c)]
        grid[r * step:r * step + tile_px, c * step:c * step + tile_px] = raw
        raws[(r, c)] = raw
    grid_valid = grid > 0
    # pass 2: fill nodata ONCE over the whole grid, so filled values also agree across tile edges
    # (the filled area has no real data; it is hidden by the mask but still exists as geometry/collision)
    if grid.size <= 200_000_000:
        filled_grid, _ = fill_nodata(grid, args.fill)
        per_tile_fill = False
    else:
        print("grid too large for a single fill pass; filling per tile (filled areas may not match at edges)")
        filled_grid, per_tile_fill = grid.copy(), True
    missing = [(r, c) for r in range(rows) for c in range(cols) if (r, c) not in present]

    # pass 3: cut, write, verify
    for (r, c) in sorted(present):
        t = present[(r, c)]
        raw = raws.pop((r, c))
        valid = raw > 0
        if per_tile_fill:
            filled, _ = fill_nodata(raw, args.fill)
        else:
            filled = filled_grid[r * step:r * step + tile_px, c * step:c * step + tile_px].copy()
        stitched = stitched_n[(r, c)]
        base = f"{site}_h_{r}_{c}"
        hp = os.path.join(out, "heightmaps", base + ".png")
        mp = os.path.join(out, "masks", f"{site}_m_{r}_{c}.png")
        write_u16_png(hp, filled)
        Image.fromarray(np.where(valid, 255, 0).astype(np.uint8), "L").save(mp, compress_level=6)
        if args.r16:
            filled.astype("<u2").tofile(os.path.join(out, "heightmaps", base + ".r16"))
        if not args.no_tiled:
            write_u16_png(os.path.join(out, "tiled", f"{site}_x{c}_y{r}.png"), filled)
        cp = None
        if not args.no_color:
            cands = []
            if args.prefer_albedo:
                cands.append(t.get("albedo") or f"a_{r}_{c}.png")
            cands.append(t.get("texture") or f"t_{r}_{c}.png")
            for name in cands:
                sp = os.path.join(site_dir, name)
                if os.path.isfile(sp):
                    dp = os.path.join(out, "color", f"{site}_c_{r}_{c}.png")
                    if write_colour(sp, dp, None if args.no_tint else MARS_TINT):
                        cp = dp
                    break

        # ---- verification of what was written
        back = read_u16(hp)
        ok_valid = bool(np.array_equal(back[valid], raw[valid]))
        z_back = ue_cm(back[valid].astype(np.float64), loc_z, scale_z)
        z_src = marsmaker_cm(raw[valid], hmin, rng)
        e = float(np.abs(z_back - z_src).max()) if valid.any() else 0.0
        max_unchanged_err = max(max_unchanged_err, e)

        tl = t["top_left_m"]
        loc = [tl["east"] * 100.0 + 0.0, -tl["north"] * 100.0 + 0.0, loc_z]   # + 0.0 avoids "-0.0"
        size_cm = step * scale_xy
        centre = [loc[0] + size_cm / 2.0, loc[1] + size_cm / 2.0, loc_z]
        meta_loc = t.get("unreal_location_cm")
        tiles_out.append({
            "row": r, "col": c,
            "heightmap": os.path.abspath(hp).replace("\\", "/"),
            "mask": os.path.abspath(mp).replace("\\", "/"),
            "color": os.path.abspath(cp).replace("\\", "/") if cp else None,
            "actor_location_cm": loc,
            "ui_location_cm": centre,
            "size_cm": size_cm,
            "valid_fraction": round(float(valid.mean()), 4),
            "filled_px": int((~valid).sum()),
            "stitched_px": stitched,
            "check": {
                "valid_px_unchanged": ok_valid,
                "min_code_written": int(back.min()), "max_code_written": int(back.max()),
                "max_abs_err_cm_ue_vs_source": e,
                "meta_unreal_location_diff_cm": (max(abs(a - b) for a, b in zip(meta_loc, loc)) if meta_loc else None),
            },
        })
        print(f"{base}: valid {valid.mean():.3f}, filled {int((~valid).sum())} px, stitched {stitched}, "
              f"roundtrip err {e:.2e} cm, unchanged {ok_valid}")

    merged_info = None
    if not args.no_merged:
        merged = filled_grid if not per_tile_fill else fill_nodata(grid, "min")[0]
        mp = os.path.join(out, "merged", f"{site}_merged.png")
        write_u16_png(mp, merged)
        Image.fromarray(np.where(grid_valid, 255, 0).astype(np.uint8), "L").save(
            os.path.join(out, "merged", f"{site}_merged_mask.png"), compress_level=6)
        merged_info = {
            "heightmap": os.path.abspath(mp).replace("\\", "/"),
            "size_px": [merged.shape[1], merged.shape[0]],
            "actor_location_cm": [gtl["east"] * 100.0 + 0.0, -gtl["north"] * 100.0 + 0.0, loc_z],
            "ui_location_cm": [gtl["east"] * 100.0 + (merged.shape[1] - 1) * scale_xy / 2.0,
                               -gtl["north"] * 100.0 + (merged.shape[0] - 1) * scale_xy / 2.0, loc_z],
            "configs_x": landscape_configs(merged.shape[1], 1 << 20),
            "configs_y": landscape_configs(merged.shape[0], 1 << 20),
            "missing_tiles_filled": missing,
        }

    configs = landscape_configs(tile_px)
    rec = next((k for k in configs if k["quads_per_section"] == 63 and k["sections_per_component"] == "2x2"), configs[0] if configs else None)
    doc = {
        "format": "marsmaker-unreal-prep/1",
        "site": site,
        "site_dir": site_dir.replace("\\", "/"),
        "id": meta.get("id"),
        "projection": meta.get("projection"),
        "center_lat": meta.get("center_lat"), "center_lon_east": meta.get("center_lon_east"),
        "axes": "X = east, Y = south (-north), Z = up; centimetres; heightmap row 0 = north (min Y), column 0 = west (min X)",
        "height_min_m": hmin, "height_range_m": rng, "height_max_m": meta.get("height_max_m"),
        "scale": [scale_xy, scale_xy, scale_z],
        "location_z_cm": loc_z,
        "formula": {
            "unreal": "world_z_cm = location_z + (v - 32768) * scale_z / 128",
            "marsmaker": "height_m = height_min + (v - 1) * range / 65534",
            "scale_z": "128 * 100 * range / 65534",
            "location_z": "100 * height_min + 32767 * 100 * range / 65534",
        },
        "fill": args.fill,
        "tile_px": tile_px,
        "landscape": rec,
        "landscape_valid_configs": configs,
        "tiles": tiles_out,
        "merged": merged_info,
        "tiled_dir": None if args.no_tiled else os.path.abspath(os.path.join(out, "tiled")).replace("\\", "/"),
        "checks": checks,
        "summary": {
            "max_abs_err_cm_written_vs_source": max_unchanged_err,
            "all_valid_px_unchanged": all(t["check"]["valid_px_unchanged"] for t in tiles_out),
            "stitched_px_total": total_stitched,
            "seconds": round(time.time() - t_start, 1),
        },
    }
    with open(os.path.join(out, "unreal_import.json"), "w") as f:
        json.dump(doc, f, indent=1)
    write_settings_txt(os.path.join(out, "IMPORT_SETTINGS.txt"), doc)
    print(json.dumps({"checks": checks, "summary": doc["summary"], "landscape": rec}, indent=1))
    print("wrote", out)
    bad = (not doc["summary"]["all_valid_px_unchanged"]) or checks["ue_vs_marsmaker_max_abs_err_cm_all_codes"] > 1e-6
    return 1 if bad else 0


def write_settings_txt(path, d):
    L = d["landscape"]
    sx, sy, sz = d["scale"]
    lines = [
        f"MarsMaker -> Unreal Landscape settings for {d['site']}",
        "",
        "Landscape mode > Manage > New > Import from File, for EACH tile:",
        f"  Heightmap File       : the tile's heightmaps/<site>_h_<row>_<col>.png",
        f"  Section Size         : {L['quads_per_section']}x{L['quads_per_section']} Quads",
        f"  Sections Per Component: {L['sections_per_component']}",
        f"  Number of Components : {L['components']} x {L['components']}   (Overall resolution {d['tile_px']} x {d['tile_px']})",
        f"  Scale                : X {sx}  Y {sy}  Z {sz:.10f}",
        "  Location             : 'UI Location' column = tile CENTRE (the New Landscape tool is believed to centre",
        "                         the landscape on this point; marsmaker_unreal.py fixes the transform either way)",
        "  Rotation             : 0, 0, 0",
        "  Afterwards run marsmaker_unreal.py: it snaps every landscape to the exact transform below.",
        "",
        f"{'tile':>8} {'UI Location X':>14} {'UI Location Y':>14} {'Location Z':>14}   | actor location (corner) X, Y",
    ]
    for t in d["tiles"]:
        u, a = t["ui_location_cm"], t["actor_location_cm"]
        lines.append(f"h_{t['row']}_{t['col']:<4} {u[0]:14.1f} {u[1]:14.1f} {u[2]:14.4f}   | {a[0]:.1f}, {a[1]:.1f}")
    if d.get("merged"):
        m = d["merged"]
        lines += ["", "Single landscape alternative (merged heightmap or World Partition tiled import):",
                  f"  file {m['heightmap']} ({m['size_px'][0]} x {m['size_px'][1]})",
                  f"  UI Location {m['ui_location_cm'][0]:.1f}, {m['ui_location_cm'][1]:.1f}, {m['ui_location_cm'][2]:.4f}"
                  f"   actor location (corner) {m['actor_location_cm'][0]:.1f}, {m['actor_location_cm'][1]:.1f}",
                  f"  same Scale as above"]
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    sys.exit(main())
