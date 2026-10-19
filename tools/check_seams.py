"""check_seams — verify that neighbouring tiles of a converted site share bit-identical edge posts.

Tiles are TILE x TILE px with a 1 px overlap (step TILE-1), so the last row of {k}_{r}_{c} must equal
the first row of {k}_{r+1}_{c}, and the last column must equal the first column of {k}_{r}_{c+1}.
Checks h_ (uint16 height), t_ (uint8 texture) and a_ (uint8 albedo) tiles.

Usage:
  python tools/check_seams.py D:\\marsmaker_data\\sites\\PSP_007018_1255_PSP_007229_1255 [more sites...]
  python tools/check_seams.py --all [--data D:\\marsmaker_data]      # every site under <data>/sites
  python tools/check_seams.py SITE -v                               # list every mismatching seam
Exit code 1 if any mismatch is found.
"""
import argparse, os, re, sys
import numpy as np
from PIL import Image

KINDS = ("h", "t", "a")
NAME = re.compile(r"^([hta])_(\d+)_(\d+)\.png$")


def load(path):
    return np.asarray(Image.open(path))


def check_site(site, verbose=False):
    files = {}
    for f in os.listdir(site):
        m = NAME.match(f)
        if m:
            files[(m.group(1), int(m.group(2)), int(m.group(3)))] = os.path.join(site, f)
    res = {}
    for k in KINDS:
        keys = sorted((r, c) for (kk, r, c) in files if kk == k)
        if not keys:
            continue
        have = set(keys)
        cache = {}

        def get(rc):
            if rc not in cache:
                cache[rc] = load(files[(k, *rc)])
            return cache[rc]

        st = {"tiles": len(keys), "seams": 0, "posts": 0, "mismatch": 0, "nodata_mismatch": 0,
              "max_diff": 0, "bad_seams": []}
        for r, c in keys:
            for (nr, nc), ns in (((r + 1, c), "S"), ((r, c + 1), "E")):
                if (nr, nc) not in have:
                    continue
                a, b = get((r, c)), get((nr, nc))
                if ns == "S":
                    ea, eb = a[-1, :], b[0, :]
                else:
                    ea, eb = a[:, -1], b[:, 0]
                st["seams"] += 1
                st["posts"] += ea.size
                d = ea != eb
                n = int(d.sum())
                if n:
                    nod = int((d & ((ea == 0) | (eb == 0))).sum())
                    md = int(np.abs(ea[d].astype("int64") - eb[d].astype("int64")).max())
                    st["mismatch"] += n
                    st["nodata_mismatch"] += nod
                    st["max_diff"] = max(st["max_diff"], md)
                    st["bad_seams"].append((f"{k}_{r}_{c}", ns, f"{k}_{nr}_{nc}", n, md, nod))
            # free tiles we no longer need (row-major scan: rows < r-0 are done)
            for kk in [kk for kk in cache if kk[0] < r]:
                del cache[kk]
        res[k] = st
    return res


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("sites", nargs="*")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--data", default=r"D:\marsmaker_data")
    ap.add_argument("-v", "--verbose", action="store_true")
    a = ap.parse_args()
    sites = list(a.sites)
    if a.all:
        root = os.path.join(a.data, "sites")
        sites += [os.path.join(root, d) for d in sorted(os.listdir(root))
                  if os.path.isdir(os.path.join(root, d)) and not d.endswith((".tmp", ".old"))]
    if not sites:
        ap.error("give site folders or --all")
    total = 0
    for site in sites:
        res = check_site(site, a.verbose)
        parts = []
        for k, st in res.items():
            total += st["mismatch"]
            parts.append(f"{k}_: {st['mismatch']} mismatched / {st['posts']} shared posts "
                         f"({len(st['bad_seams'])}/{st['seams']} seams, max diff {st['max_diff']}"
                         f"{', nodata-vs-data ' + str(st['nodata_mismatch']) if st['nodata_mismatch'] else ''})")
        print(f"{os.path.basename(os.path.normpath(site))}: " + ("; ".join(parts) if parts else "no tiles"))
        if a.verbose:
            for k, st in res.items():
                for ta, ns, tb, n, md, nod in st["bad_seams"]:
                    print(f"    {ta} {ns}-> {tb}: {n} posts differ, max {md}" + (f", {nod} nodata" if nod else ""))
    print(f"TOTAL mismatched shared posts: {total}")
    sys.exit(1 if total else 0)


if __name__ == "__main__":
    main()
