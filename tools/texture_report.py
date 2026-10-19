"""texture_report — how many grey levels a site's texture really uses, and how banded it is.

For every t_ (or --prefix a_) tile of a site it accumulates:
  distinct_levels    number of the 255 data levels (1..255) that occur at all
  effective_levels   exp(entropy) of the level histogram: levels "in use", weighted by frequency
  fill_ratio         distinct levels / span (max - min + 1); a stretched low-bit image leaves gaps
  mean_gap           mean distance between consecutive occupied levels (1 = no gaps)
  smooth_step        median non-zero difference between horizontal neighbours in smooth areas
                     (|diff| <= 12): the size of the smallest visible step; ~1-2 natural, >=4 banded
  plateau_fraction   share of smooth-area neighbour pairs that are exactly equal (flat bands)
  banding_index      smooth_step * plateau_fraction / 0.5, rough single number: < 1.5 fine,
                     2-4 visible banding in flat areas, > 4 strong posterisation

Usage:
  python tools/texture_report.py D:\\marsmaker_data\\sites\\X [more sites] [--prefix a_] [--json out.json]
"""
import argparse, glob, json, os, re

import numpy as np
from PIL import Image


def report(site, prefix="t_", max_tiles=None):
    files = sorted(glob.glob(os.path.join(site, f"{prefix}*_*.png")),
                   key=lambda p: tuple(int(x) for x in re.findall(r"(\d+)_(\d+)\.png$", p)[0]))
    if max_tiles:
        files = files[:max_tiles]
    hist = np.zeros(256, "int64")
    steps = np.zeros(256, "int64")       # histogram of |horizontal diff| in smooth areas
    n_tiles = 0
    for f in files:
        a = np.asarray(Image.open(f))
        if a.dtype != np.uint8:
            a = a.astype("uint8")
        hist += np.bincount(a.ravel(), minlength=256)
        if a.max() == 0:
            continue
        n_tiles += 1
        d = np.abs(np.diff(a.astype("int16"), axis=1))
        both = (a[:, 1:] > 0) & (a[:, :-1] > 0)
        sm = both & (d <= 12)
        steps += np.bincount(d[sm].ravel(), minlength=256)
    h = hist[1:]
    tot = int(h.sum())
    out = {"site": os.path.basename(os.path.normpath(site)), "prefix": prefix, "tiles": len(files),
           "tiles_with_data": n_tiles, "valid_pixels": tot}
    if tot == 0:
        out["note"] = "no valid texture pixels"
        return out
    occ = np.nonzero(h)[0] + 1
    p = h[h > 0] / tot
    gaps = np.diff(occ)
    nz = steps[1:]
    if nz.sum():
        cum = np.cumsum(nz)
        smooth_step = int(np.searchsorted(cum, cum[-1] / 2) + 1)
    else:
        smooth_step = 0
    plateau = float(steps[0] / steps.sum()) if steps.sum() else 0.0
    # central 98 % of pixels: levels actually carrying the image
    c = np.cumsum(h) / tot
    lo, hi = int(np.searchsorted(c, 0.01) + 1), int(np.searchsorted(c, 0.99) + 1)
    out.update({
        "distinct_levels": int(occ.size),
        "effective_levels": round(float(np.exp(-(p * np.log(p)).sum())), 1),
        "min": int(occ[0]), "max": int(occ[-1]),
        "p1_p99": [lo, hi], "distinct_levels_p1_p99": int(((occ >= lo) & (occ <= hi)).sum()),
        "fill_ratio": round(occ.size / (occ[-1] - occ[0] + 1), 3),
        "mean_gap": round(float(gaps.mean()), 2) if gaps.size else 0.0,
        "smooth_step": smooth_step,
        "plateau_fraction": round(plateau, 3),
        "banding_index": round(smooth_step * plateau / 0.5, 2),
    })
    meta_p = os.path.join(site, "meta.json")
    if os.path.isfile(meta_p):
        with open(meta_p) as f:
            m = json.load(f)
        tx = m.get("texture") or {}
        out["converter_version"] = m.get("converter_version")
        if tx.get("mapping"):
            out["mapping"] = {k: tx["mapping"].get(k) for k in ("dn_lo", "dn_hi", "gamma", "dither_dn", "levels_per_dn")}
        elif tx.get("stretch_dn"):
            lo_, hi_ = tx["stretch_dn"]
            out["mapping"] = {"dn_lo": lo_, "dn_hi": hi_, "gamma": 1.0, "dither_dn": 0.0,
                              "levels_per_dn": round(254 / max(hi_ - lo_, 1e-9), 3)}
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("sites", nargs="+")
    ap.add_argument("--prefix", default="t_", help="tile prefix: t_ (texture) or a_ (albedo)")
    ap.add_argument("--max-tiles", type=int, default=None)
    ap.add_argument("--json", help="write the reports to this file")
    a = ap.parse_args()
    reps = [report(s, a.prefix, a.max_tiles) for s in a.sites]
    cols = ["distinct_levels", "effective_levels", "distinct_levels_p1_p99", "fill_ratio", "mean_gap",
            "smooth_step", "plateau_fraction", "banding_index"]
    print(f"{'site':40s} " + " ".join(f"{c[:14]:>14s}" for c in cols) + "  levels/DN")
    for r in reps:
        print(f"{r['site'][:40]:40s} " + " ".join(f"{str(r.get(c, '-')):>14s}" for c in cols)
              + f"  {(r.get('mapping') or {}).get('levels_per_dn', '-')}")
    if a.json:
        with open(a.json, "w") as f:
            json.dump(reps, f, indent=1)


if __name__ == "__main__":
    main()
