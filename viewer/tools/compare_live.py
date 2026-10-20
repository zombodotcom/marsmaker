"""Compare a live-streamed grid (marsmaker-viewer --live ID --live-dump OUT) with the converted site.

    python viewer/tools/compare_live.py OUT D:/marsmaker_data/sites/<id>

OUT.json/OUT.f32 hold the live grid (absolute heights, NaN = nodata). The converted site's h_ tiles
are decoded to metres and bilinearly sampled at every live post's world (east, north) position,
then height statistics and a few sample points are printed.
"""
import json, os, sys
import numpy as np
from PIL import Image


def converted_grid(site):
    m = json.load(open(os.path.join(site, "meta.json")))
    W, H = m["grid_px"]
    sp = m["tile_px"] - 1
    z = np.full((H, W), np.nan, np.float32)
    k = m["height_range_m"] / 65534.0
    for t in m["tiles"]:
        a = np.array(Image.open(os.path.join(site, t["height"])))
        r0, c0 = t["y"] * sp, t["x"] * sp
        v = np.where(a > 0, m["height_min_m"] + (a.astype(np.float64) - 1) * k, np.nan).astype(np.float32)
        z[r0:r0 + a.shape[0], c0:c0 + a.shape[1]] = v
    return z, m


def bilinear(z, r, c):
    H, W = z.shape
    ok = (r >= 0) & (c >= 0) & (r < H - 1) & (c < W - 1)
    r0 = np.clip(np.floor(r).astype(int), 0, H - 2)
    c0 = np.clip(np.floor(c).astype(int), 0, W - 2)
    fr, fc = r - r0, c - c0
    v = (z[r0, c0] * (1 - fr) * (1 - fc) + z[r0 + 1, c0] * fr * (1 - fc) + z[r0, c0 + 1] * (1 - fr) * fc
         + z[r0 + 1, c0 + 1] * fr * fc)
    return np.where(ok, v, np.nan)


def main():
    out, site = sys.argv[1], sys.argv[2]
    lm = json.load(open(out + ".json"))
    lz = np.fromfile(out + ".f32", "<f4").reshape(lm["h"], lm["w"])
    cz, cm = converted_grid(site)
    mpp = lm["mpp"]
    ii, jj = np.mgrid[0:lm["h"], 0:lm["w"]]
    east = lm["east0"] + jj * mpp
    north = lm["north0"] - ii * mpp
    cr = (cm["grid_top_left_m"]["north"] - north) / cm["metres_per_pixel"]
    cc = (east - cm["grid_top_left_m"]["east"]) / cm["metres_per_pixel"]
    cs = bilinear(cz, cr, cc)
    both = np.isfinite(lz) & np.isfinite(cs)
    d = (lz - cs)[both]
    print(f"live:      {lm['w']}x{lm['h']} at {mpp:.3f} m, valid {np.isfinite(lz).sum():,} posts, "
          f"min {np.nanmin(lz):.2f} max {np.nanmax(lz):.2f} mean {np.nanmean(lz):.2f} m")
    print(f"converted: {cm['grid_px'][0]}x{cm['grid_px'][1]} at {cm['metres_per_pixel']} m, "
          f"min {np.nanmin(cz):.2f} max {np.nanmax(cz):.2f} mean {np.nanmean(cz):.2f} m")
    print(f"common posts {both.sum():,} ({both.sum() / max(1, np.isfinite(lz).sum()):.1%} of live valid); "
          f"live mean over them {lz[both].mean():.2f}, converted {cs[both].mean():.2f}")
    print(f"live - converted: mean {d.mean():+.3f} m, median {np.median(d):+.3f}, |d| p50 {np.percentile(abs(d), 50):.3f}, "
          f"p95 {np.percentile(abs(d), 95):.3f}, p99 {np.percentile(abs(d), 99):.3f}, max {abs(d).max():.2f} m")
    tp = out + ".tex"
    if os.path.isfile(tp) and cm.get("tiles") and cm["tiles"][0].get("texture"):
        lt = np.fromfile(tp, np.uint8).reshape(lm["h"], lm["w"]).astype(np.float32)
        ct = np.zeros(cz.shape, np.float32)
        sp = cm["tile_px"] - 1
        for t in cm["tiles"]:
            if t.get("texture"):
                a = np.array(Image.open(os.path.join(site, t["texture"])))
                ct[t["y"] * sp:t["y"] * sp + a.shape[0], t["x"] * sp:t["x"] * sp + a.shape[1]] = a
        ct[ct == 0] = np.nan
        lt[lt == 0] = np.nan
        cts = bilinear(ct, cr, cc)
        m = np.isfinite(lt) & np.isfinite(cts)
        a, b = lt[m] - lt[m].mean(), cts[m] - cts[m].mean()
        print(f"texture: {m.sum():,} common texels, correlation live vs converted {float((a * b).mean() / (a.std() * b.std())):.3f}, "
              f"mean grey {lt[m].mean():.1f} vs {cts[m].mean():.1f}")
        # residual misalignment: best integer shift (in live posts) of the correlation
        best = (0, 0, -1.0)
        sub = (slice(200, lm["h"] - 200), slice(200, lm["w"] - 200))
        L, C = lt[sub], cts[sub]
        for dy in range(-3, 4):
            for dx in range(-3, 4):
                Cs = np.roll(np.roll(C, dy, 0), dx, 1)
                mm = np.isfinite(L) & np.isfinite(Cs)
                x, y = L[mm] - L[mm].mean(), Cs[mm] - Cs[mm].mean()
                r = float((x * y).mean() / (x.std() * y.std()))
                if r > best[2]:
                    best = (dx, dy, r)
        print(f"texture best shift (cols, rows) = {best[:2]} posts, correlation {best[2]:.3f}")
    rng = np.random.default_rng(1)
    idx = np.flatnonzero(both.ravel())
    print("sample points (east, north): live / converted")
    for k in rng.choice(idx, 8, replace=False):
        i, j = divmod(k, lm["w"])
        print(f"  ({east[i, j]:8.1f}, {north[i, j]:8.1f}): {lz[i, j]:8.2f} / {cs[i, j]:8.2f}  d={lz[i, j] - cs[i, j]:+.2f}")


if __name__ == "__main__":
    main()
