"""Independent check of the C# core output (Program.cs) using Pillow + numpy.

usage: python verify_core.py <site_dir> <cs_out.json>
Checks: decoded PNG sums / zero counts / sample pixels; resampled values at sample points (2049 bilinear,
1025 tent) recomputed here from the definition; nodata counts; edge columns/rows equal between
neighbouring tiles; mesh vertex/triangle counts for step 4 (same rule as the Godot/Blender importers).
"""
import json
import math
import os
import sys

import numpy as np
from PIL import Image

RPOINTS = [(0, 0), (0, 1), (1, 1), (1024, 1024), (511, 1700), (2047, 2048), (2048, 2048), (333, 999), (1500, 77), (900, 1900)]
POINTS = [(0, 0), (0, 2016), (2016, 0), (2016, 2016), (1008, 1008), (17, 1999), (500, 1500), (1234, 321), (2000, 40), (777, 777)]


def load(path):
    im = Image.open(path)
    a = np.array(im)
    return a.astype(np.int64)


def axis(src_n, n, i):
    scale = (src_n - 1) / (n - 1)
    radius = max(1.0, scale)
    s = (src_n - 1) if i == n - 1 else i * scale
    f = min(int(math.floor(s)), src_n - 1)
    fr = s - f
    b = [f] if fr <= 1e-9 else sorted({f, min(f + 1, src_n - 1)})
    if i == 0 or i == n - 1:
        return b, [(f, 1.0)]
    lo = max(0, math.ceil(s - radius + 1e-9))
    hi = min(src_n - 1, math.floor(s + radius - 1e-9))
    if hi < lo:
        lo = hi = f
    return b, [(k, max(0.0, 1 - abs(k - s) / radius)) for k in range(lo, hi + 1)]


def resample_point(a, n, r, c):
    by, wy = axis(a.shape[0], n, r)
    bx, wx = axis(a.shape[1], n, c)
    if any(a[y, x] == 0 for y in by for x in bx):
        return None
    s = ws = 0.0
    for y, w1 in wy:
        for x, w2 in wx:
            v = a[y, x]
            if v:
                s += w1 * w2 * v
                ws += w1 * w2
    return s / ws


def resample_nan_count(a, n):
    """vectorised validity: nodata if any bilinear-support post is 0."""
    h, w = a.shape
    def supp(src_n):
        i = np.arange(n)
        s = i * (src_n - 1) / (n - 1)
        s[-1] = src_n - 1
        f = np.minimum(np.floor(s).astype(int), src_n - 1)
        f1 = np.where(s - f > 1e-9, np.minimum(f + 1, src_n - 1), f)
        return f, f1
    y0, y1 = supp(h)
    x0, x1 = supp(w)
    z = a == 0
    bad = z[np.ix_(y0, x0)] | z[np.ix_(y0, x1)] | z[np.ix_(y1, x0)] | z[np.ix_(y1, x1)]
    return int(bad.sum()), bad


def mesh_counts(a, step):
    h, w = a.shape
    ri = list(range(0, h, step)); ri += [h - 1] if ri[-1] != h - 1 else []
    ci = list(range(0, w, step)); ci += [w - 1] if ci[-1] != w - 1 else []
    q = a[np.ix_(ri, ci)] > 0
    ok = q[:-1, :-1] & q[1:, :-1] & q[:-1, 1:] & q[1:, 1:]
    used = np.zeros_like(q)
    used[:-1, :-1] |= ok; used[1:, :-1] |= ok; used[:-1, 1:] |= ok; used[1:, 1:] |= ok
    return int(used.sum()), int(ok.sum()) * 2


def main():
    site, out = sys.argv[1], sys.argv[2]
    cs = {t["name"]: t for t in json.load(open(out))["tiles"]}
    fails = 0
    def check(cond, msg):
        nonlocal fails
        if not cond:
            fails += 1
            print("FAIL", msg)
    def key(n):
        p = os.path.splitext(n)[0].split("_")
        return (p[0], int(p[1]), int(p[2]))
    south, east = {}, {}
    for name, t in sorted(cs.items(), key=lambda kv: key(kv[0])):
        a = load(os.path.join(site, name))
        check(a.shape == (t["h"], t["w"]), f"{name} shape")
        check(int(a.sum()) == t["sum"], f"{name} sum {a.sum()} vs {t['sum']}")
        check(int((a == 0).sum()) == t["zeros"], f"{name} zeros")
        for (r, c), v in zip(POINTS, t["px"]):
            check(int(a[r, c]) == v, f"{name} px {(r, c)} {a[r, c]} vs {v}")
        if name.startswith("h_"):
            _, r0, c0 = key(name)
            before = a.copy()
            if (r0 - 1, c0) in south:
                a[0, :] = south[(r0 - 1, c0)]
            if (r0, c0 - 1) in east:
                a[:, 0] = east[(r0, c0 - 1)]
            south[(r0, c0)], east[(r0, c0)] = a[-1, :].copy(), a[:, -1].copy()
            check(int((before != a).sum()) == t["stitched"], f"{name} stitched {(before != a).sum()} vs {t['stitched']}")
            for n in (2049, 1025):
                rr = t[f"r{n}"]
                nan, _ = resample_nan_count(a, n)
                check(nan == rr["nan"], f"{name} r{n} nan {nan} vs {rr['nan']}")
                for (r, c), v in zip(RPOINTS, rr["vals"]):
                    r, c = min(r, n - 1), min(c, n - 1)
                    ref = resample_point(a, n, r, c)
                    if ref is None or v is None:
                        check(ref is None and v is None, f"{name} r{n} {(r, c)} nodata mismatch {ref} vs {v}")
                    else:
                        check(abs(ref - v) <= 0.01, f"{name} r{n} {(r, c)} {ref} vs {v}")
                check(rr["nan_after_fill"] == 0, f"{name} r{n} fill left NaN")
            vref, tref = mesh_counts(a, 4)
            m = t.get("mesh4")
            check(m is not None and m["verts"] == vref and m["tris"] == tref, f"{name} mesh4 {m} vs {vref},{tref}")
            if m:
                hmin, rng = 57.27127456665039, 589.7566795349121
                check(m["miny"] >= hmin - 1e-3 and m["maxy"] <= hmin + rng + 1e-3, f"{name} mesh4 y range")
    # neighbour edges must be identical (shared posts + edge-only filtering)
    for name, t in cs.items():
        if not name.startswith("h_"):
            continue
        _, r, c = os.path.splitext(name)[0].split("_")
        r, c = int(r), int(c)
        east = cs.get(f"h_{r}_{c + 1}.png")
        south = cs.get(f"h_{r + 1}_{c}.png")
        for n in (2049, 1025):
            if east:
                check(t[f"r{n}"]["edge_e"] == east[f"r{n}"]["edge_w"], f"{name} r{n} east edge != neighbour west edge")
            if south:
                check(t[f"r{n}"]["edge_s"] == south[f"r{n}"]["edge_n"], f"{name} r{n} south edge != neighbour north edge")
    print(f"checked {len(cs)} PNGs: {'ALL OK' if fails == 0 else str(fails) + ' FAILURES'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
