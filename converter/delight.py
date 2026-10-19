"""delight — approximate albedo tiles: divide the DTM-predicted sun shading out of the HiRISE ortho.

The ortho photo already contains the shading of the sun at the time of the observation; a game
engine lights the terrain again, so slopes facing the HiRISE sun look doubly bright and the others
doubly dark. This tool predicts that shading from the DTM with the observation's real sun azimuth /
elevation (HiRISE RDR label: SUB_SOLAR_AZIMUTH, NORTH_AZIMUTH, INCIDENCE_ANGLE) and divides it out.

Model (per pixel, DN space, i.e. after undoing the texture tone mapping):
    R  = cos(local incidence) / cos(incidence on flat ground)   Lambert, from the smoothed DTM
    I  ~ a + b R               least-squares fit over the site (absorbs haze + non-Lambert behaviour)
    k  = b / (a + b)           fitted shading strength (1 = pure Lambert, 0 = no shading visible)
    S  = 1 - w + w * clip(R, r_min, r_max),   w = blend * k
    A  = I / max(S, s_min)     -> encoded with the site's texture mapping -> a_{row}_{col}.png
This is APPROXIMATE: the DTM resolves ~3 m, the photo ~0.3-1 m, so only shading at DTM scale is
removed; cast shadows, atmospheric scattering and true photometric behaviour are not modelled.
t_* tiles are left untouched; meta.json gets an "albedo" block and a per-tile "albedo" file name.

Usage:
  python converter/delight.py D:\\marsmaker_data\\sites\\PSP_007018_1255_PSP_007229_1255 [--blend 0.8] [--low-priority]
"""
import argparse, json, math, os, sys, time
from datetime import datetime, timezone

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))  # converter/, next to hirise2engine.py
sys.path.insert(0, HERE)
import hirise2engine as h2e  # noqa: E402

TOOL_VERSION = 1
DEFAULTS = {"blend": 0.8, "smooth_px": 3.0, "r_min": 0.25, "r_max": 2.5, "s_min": 0.35, "pad": 16}


def _now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


class Tiles:
    """Reads tiles of one kind and assembles arbitrary windows of the global site grid, so that
    gradients/smoothing at tile edges see the neighbouring tile (seamless output)."""

    def __init__(self, site, meta, kind):
        self.site, self.meta, self.kind = site, meta, kind
        self.step = meta["tile_px"] - 1
        self.tile = meta["tile_px"]
        self.files = {(t["y"], t["x"]): t.get("height" if kind == "h" else "texture") for t in meta["tiles"]}
        self.cache = {}

    def get(self, r, c):
        key = (r, c)
        if key not in self.cache:
            f = self.files.get(key)
            p = os.path.join(self.site, f) if f else None
            if not p or not os.path.isfile(p):
                self.cache[key] = None
            else:
                v = np.asarray(Image.open(p))
                if self.kind == "h":
                    m = self.meta
                    a = (m["height_min_m"] + (v.astype("float64") - 1) * m["height_range_m"] / 65534).astype("float32")
                    a[v == 0] = np.nan
                else:
                    a = h2e.decode_texture(v, self.meta["texture"]).astype("float32")
                self.cache[key] = a
        return self.cache[key]

    def drop_rows_before(self, r):
        for k in [k for k in self.cache if k[0] < r]:
            del self.cache[k]

    def window(self, g0, h0, H, W):
        """rows g0..g0+H, cols h0..h0+W of the global grid (NaN where no tile)."""
        out = np.full((H, W), np.nan, "float32")
        for r in range(max(0, (g0 - 1) // self.step - 1), (g0 + H) // self.step + 1):
            for c in range(max(0, (h0 - 1) // self.step - 1), (h0 + W) // self.step + 1):
                a = self.get(r, c)
                if a is None:
                    continue
                R0, C0 = r * self.step, c * self.step
                r0, r1 = max(g0, R0), min(g0 + H, R0 + self.tile)
                c0, c1 = max(h0, C0), min(h0 + W, C0 + self.tile)
                if r0 >= r1 or c0 >= c1:
                    continue
                src = a[r0 - R0:r1 - R0, c0 - C0:c1 - C0]
                dst = out[r0 - g0:r1 - g0, c0 - h0:c1 - h0]
                np.copyto(dst, src, where=np.isfinite(src))
        return out


def _nan_gauss(a, sigma):
    from scipy import ndimage
    v = np.isfinite(a)
    if sigma <= 0:
        return a
    num = ndimage.gaussian_filter(np.where(v, a, 0).astype("float64"), sigma, mode="nearest")
    den = ndimage.gaussian_filter(v.astype("float64"), sigma, mode="nearest")
    with np.errstate(invalid="ignore", divide="ignore"):
        out = num / den
    out[den < 0.3] = np.nan
    return out


def shading_ratio(hpad, res, az_deg, el_deg, smooth_px):
    """Lambert shading relative to flat ground, on the padded height window."""
    hs = _nan_gauss(hpad, smooth_px)
    gy, gx = np.gradient(hs, res)
    dzde, dzdn = gx, -gy                       # rows run south
    az, el = math.radians(az_deg), math.radians(el_deg)
    lx, ly, lz = math.sin(az) * math.cos(el), math.cos(az) * math.cos(el), math.sin(el)
    cos_i = (-dzde * lx - dzdn * ly + lz) / np.sqrt(1 + dzde ** 2 + dzdn ** 2)
    return cos_i / lz


def get_sun(meta, label_cache):
    if meta.get("sun"):
        return meta["sun"]
    obs = h2e.obs_from_ortho(meta.get("source_ortho"))
    if not obs:
        return None
    url = h2e.rdr_label_url(obs)
    txt = h2e.fetch_text(url, os.path.join(label_cache, os.path.basename(url)) if label_cache else None)
    return h2e.sun_from_rdr_label(h2e.parse_pds_label(txt) if txt else None, url)


def delight_site(site, blend=None, label_cache=None, k_override=None, opts=None, log=print):
    o = dict(DEFAULTS, **(opts or {}))
    if blend is not None:
        o["blend"] = blend
    mpath = os.path.join(site, "meta.json")
    with open(mpath) as f:
        meta = json.load(f)
    if not meta.get("texture"):
        raise RuntimeError("site has no texture")
    if label_cache is None:
        label_cache = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(site))), "labels")
    sun = get_sun(meta, label_cache)
    if not sun:
        raise RuntimeError("no sun geometry (meta 'sun' missing and RDR label not reachable)")
    az, el = sun["azimuth_deg"], sun["elevation_deg"]
    res, step, T, P = meta["metres_per_pixel"], meta["tile_px"] - 1, meta["tile_px"], o["pad"]
    H, Tx = Tiles(site, meta, "h"), Tiles(site, meta, "t")
    tiles = sorted([t for t in meta["tiles"] if t.get("texture")], key=lambda t: (t["y"], t["x"]))
    mp = meta["texture"].get("mapping") or {"dn_lo": meta["texture"]["stretch_dn"][0],
                                            "dn_hi": meta["texture"]["stretch_dn"][1], "gamma": 1.0, "dither_dn": 0.0}

    def tile_arrays(t):
        g0, h0 = t["y"] * step, t["x"] * step
        hp = H.window(g0 - P, h0 - P, T + 2 * P, T + 2 * P)
        I = Tx.get(t["y"], t["x"])
        return hp, I

    # --- pass 1: fit I ~ a + b R (smoothed photo vs DTM shading) and check the sun azimuth
    t0 = time.time()
    acc = np.zeros(6)                          # n, sR, sI, sRR, sRI, sII
    scan_az = np.arange(0, 360, 15.0)
    scan = np.zeros((len(scan_az), 6))
    for t in tiles:
        H.drop_rows_before(t["y"] - 1); Tx.drop_rows_before(t["y"] - 1)
        hp, I = tile_arrays(t)
        if I is None:
            continue
        Is = _nan_gauss(I, o["smooth_px"])[::3, ::3]
        R = shading_ratio(hp, res, az, el, o["smooth_px"])[P:P + T, P:P + T][::3, ::3]
        m = np.isfinite(R) & np.isfinite(Is) & (R > 0.05) & (R < 4)
        if m.sum() < 100:
            continue
        r, i = R[m].astype("float64"), Is[m].astype("float64")
        acc += [r.size, r.sum(), i.sum(), (r * r).sum(), (r * i).sum(), (i * i).sum()]
        hs = hp[::4, ::4]
        Is4 = _nan_gauss(I, o["smooth_px"])[::4, ::4]
        for j, a_ in enumerate(scan_az):
            R4 = shading_ratio(hs, 4 * res, a_, el, o["smooth_px"] / 4)[P // 4:P // 4 + Is4.shape[0],
                                                                        P // 4:P // 4 + Is4.shape[1]]
            mm = np.isfinite(R4) & np.isfinite(Is4[:R4.shape[0], :R4.shape[1]])
            rr, ii = R4[mm].astype("float64"), Is4[:R4.shape[0], :R4.shape[1]][mm].astype("float64")
            scan[j] += [rr.size, rr.sum(), ii.sum(), (rr * rr).sum(), (rr * ii).sum(), (ii * ii).sum()]

    def fit(s):
        n, sr, si, srr, sri, sii = s
        if n < 100:
            return None
        vr, vi, cov = srr / n - (sr / n) ** 2, sii / n - (si / n) ** 2, sri / n - sr * si / n ** 2
        b = cov / vr if vr > 0 else 0.0
        a = si / n - b * sr / n
        return {"a_dn": a, "b_dn": b, "corr": cov / math.sqrt(vr * vi) if vr > 0 and vi > 0 else 0.0, "n": int(n)}

    f = fit(acc)
    if f is None:
        raise RuntimeError("not enough overlapping DTM/texture pixels")
    k_fit = float(np.clip(f["b_dn"] / (f["a_dn"] + f["b_dn"]), 0, 1)) if (f["a_dn"] + f["b_dn"]) > 0 else 0.0
    k = k_fit if k_override is None else float(k_override)
    w = o["blend"] * k
    cors = [(fit(s) or {"corr": 0})["corr"] for s in scan]
    jbest = int(np.argmax(cors))
    az_check = {"best_azimuth_deg": float(scan_az[jbest]), "corr_best": round(cors[jbest], 4),
                "label_azimuth_deg": az, "corr_label_azimuth": round(f["corr"], 4),
                "scan_step_deg": 15, "scan_corr": [round(c, 3) for c in cors]}
    dang = abs((scan_az[jbest] - az + 180) % 360 - 180)
    warn = []
    if dang > 30:
        warn.append(f"photo shading correlates best with sun azimuth {scan_az[jbest]:.0f} deg, label says {az:.0f}")
    if f["corr"] < 0.15:
        warn.append(f"weak photo/DTM shading correlation ({f['corr']:.2f}): texture may be misaligned or "
                    "albedo-dominated; albedo correction is unreliable here")
    log(f"  sun az {az:.1f} el {el:.1f}; fit I = {f['a_dn']:.1f} + {f['b_dn']:.1f} R (corr {f['corr']:.3f}) "
        f"-> k {k_fit:.3f}, w {w:.3f}; azimuth scan best {scan_az[jbest]:.0f} ({cors[jbest]:.3f}) "
        f"[{time.time() - t0:.0f}s]")

    # --- pass 2: write a_ tiles
    H.cache.clear(); Tx.cache.clear()
    after = np.zeros(6)
    written = 0
    for t in tiles:
        H.drop_rows_before(t["y"] - 1); Tx.drop_rows_before(t["y"] - 1)
        hp, I = tile_arrays(t)
        if I is None:
            continue
        R = shading_ratio(hp, res, az, el, o["smooth_px"])[P:P + T, P:P + T]
        S = 1 - w + w * np.clip(np.nan_to_num(R, nan=1.0), o["r_min"], o["r_max"])
        A = I / np.maximum(S, o["s_min"])
        name = f"a_{t['y']}_{t['x']}.png"
        Image.fromarray(h2e.encode_texture(A, mp, t["y"] * step, t["x"] * step)).save(
            os.path.join(site, name), compress_level=6)
        t["albedo"] = name
        written += 1
        As = _nan_gauss(A, o["smooth_px"])[::3, ::3]
        Rs = R[::3, ::3]
        m = np.isfinite(Rs) & np.isfinite(As) & (Rs > 0.05) & (Rs < 4)
        r, i = Rs[m].astype("float64"), As[m].astype("float64")
        after += [r.size, r.sum(), i.sum(), (r * r).sum(), (r * i).sum(), (i * i).sum()]
    fa = fit(after) or {"corr": None}

    meta["albedo"] = {
        "approximate": True,
        "note": "APPROXIMATE de-lit ortho: DTM-scale sun shading divided out; not a photometric albedo. "
                "Use a_ instead of t_ when the engine applies its own lighting. Same encoding/mapping as texture.",
        "tiles": "a_{row}_{col}.png (uint8 grey, 0 = nodata, encoded with texture.mapping)",
        "tool": f"converter/delight.py v{TOOL_VERSION}", "created": _now(),
        "sun": sun,
        "method": "Lambert ratio R = cos(i_local)/cos(i_flat) from the DTM (Gaussian-smoothed sigma "
                  f"{o['smooth_px']} px); I ~ a + b R fitted over the site; k = b/(a+b); "
                  "S = 1 - w + w clip(R, r_min, r_max), w = blend k; A = I / max(S, s_min), in DN space",
        "blend": o["blend"], "k_fit": round(k_fit, 4), "k_used": round(k, 4), "w": round(w, 4),
        "fit": {kk: (round(v, 4) if isinstance(v, float) else v) for kk, v in f.items()},
        "corr_shading_vs_texture": round(f["corr"], 4),
        "corr_shading_vs_albedo": None if fa["corr"] is None else round(fa["corr"], 4),
        "clamp": {"r_min": o["r_min"], "r_max": o["r_max"], "s_min": o["s_min"]},
        "dtm_smoothing_px": o["smooth_px"], "azimuth_check": az_check,
        "not_modelled": ["cast shadows", "sub-DTM-resolution shading", "atmospheric scattering beyond a constant",
                         "non-Lambertian photometry", "texture/DTM misregistration"],
        "warnings": warn, "tiles_written": written,
    }
    tmp = mpath + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(meta, fh, indent=1)
    os.replace(tmp, mpath)
    log(f"  wrote {written} a_ tiles; shading/brightness correlation {f['corr']:.3f} -> "
        f"{fa['corr'] if fa['corr'] is None else round(fa['corr'], 3)}")
    return meta["albedo"]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("sites", nargs="+", help="site folders (or names under --data/sites)")
    ap.add_argument("--data", default=r"D:\marsmaker_data")
    ap.add_argument("--blend", type=float, default=DEFAULTS["blend"], help="0 = no correction, 1 = full fitted strength")
    ap.add_argument("--k", type=float, default=None, help="override the fitted shading strength k (0..1)")
    ap.add_argument("--smooth-px", type=float, default=DEFAULTS["smooth_px"])
    ap.add_argument("--low-priority", action="store_true")
    a = ap.parse_args()
    if a.low_priority:
        try:
            import psutil
            psutil.Process().nice(psutil.BELOW_NORMAL_PRIORITY_CLASS if os.name == "nt" else 10)
        except Exception as e:
            print(f"could not lower priority: {e}")
    for s in a.sites:
        site = s if os.path.isdir(s) else os.path.join(a.data, "sites", s)
        if os.path.splitdrive(os.path.abspath(site))[0].upper() == "C:":
            sys.exit(f"refusing to write bulk data on C: ({site})")
        print(os.path.basename(site.rstrip("\\/")))
        delight_site(site, a.blend, os.path.join(a.data, "labels"), a.k, {"smooth_px": a.smooth_px})


if __name__ == "__main__":
    main()
