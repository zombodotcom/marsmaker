"""Build the compact index the viewer's live mode (?live=<site>) reads: every HiRISE DTM on the
public astrogeo-ard bucket, with what the browser needs to stream it straight from NASA/USGS.

    python viewer/tools/make_live_index.py [--catalog converter/catalog.json]
                                           [--out viewer/web/live_index.json]
                                           [--cache D:/marsmaker_data/live_cache] [--sites D:/marsmaker_data/sites]
                                           [--workers 8] [--limit N]

Per site it fetches (cached under --cache, so reruns are offline):
  * the first 64 KB of the DTM and of the chosen ortho COG (HTTP Range): TIFF layout (tiling,
    compression, predictor, sample format, overview sizes, whether all IFDs fit in 64 KB) and the
    georeference (ModelPixelScale / ModelTiepoint);
  * the DTM's CRS through rasterio (/vsicurl, header only);
  * the ortho's PDS label (A_AXIS_RADIUS, MAP_PROJECTION_TYPE) for the northing-scale fix, and the
    HiRISE RDR label for the sun geometry. hirise-pds.lpl.arizona.edu sends no CORS header for
    other origins, so the browser cannot read these labels itself: they are baked in here.

Output: {"v": 1, "base": <bucket prefix>, "fields": [...], "sites": [[...], ...]} with one array per
site in "fields" order (see FIELDS). A layout summary across all files is printed at the end and
written next to the cache as layout_summary.json.
"""
import argparse, collections, json, math, os, re, struct, sys, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "converter"))

BASE = "https://astrogeo-ard.s3.us-west-2.amazonaws.com/mars/mro/hirise/controlled/dtm/"
MARS_R = 3396190.0
HEAD = 65536
FIELDS = [
    "id",      # site folder name (same as converted sites/<id>)
    "title",   # rationale for creation (short human name)
    "lat", "lon",  # centre used for world coordinates (converter's convention), degrees (east lon)
    "dtm",     # DTM COG path relative to base + id + "/" (or to base when it contains a "/")
    "ortho",   # ortho COG path, same rule (null: none)
    "res",     # DTM grid post spacing (map units, m)
    "w", "h",  # DTM size in posts
    "nov",     # number of internal overviews
    "proj",    # "eqc" | "stere_n" | "stere_s"
    "c",       # [x, y] map coordinates (m) of the centre point
    "k",       # [kx, ky]: map metres per true metre along the map x / y axes at the centre
    "conv",    # bearing of true north in the map grid, degrees clockwise from map up
    "sun",     # [azimuth clockwise from true north towards the sun, elevation] deg, or null
    "oys",     # ortho northing scale to apply (1 = none); y_true = oys * y_file + dy
    "osh",     # ortho shift [dx, dy] metres applied after the scale
    "mb",      # DTM file size, MB
]


def fetch(url, rng=None, tries=3, timeout=60):
    for i in range(tries):
        try:
            req = urllib.request.Request(url, headers={"Range": f"bytes={rng[0]}-{rng[1]}"} if rng else {})
            with urllib.request.urlopen(req, timeout=timeout) as r:
                total = None
                cr = r.headers.get("Content-Range")
                if cr and "/" in cr:
                    total = int(cr.rsplit("/", 1)[1])
                return r.read(), total
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None, None
            time.sleep(1 + i)
        except Exception:
            time.sleep(1 + i)
    return None, None


# ------------------------------------------------------------------ minimal TIFF header parser
TYPES = {1: ("B", 1), 2: ("c", 1), 3: ("H", 2), 4: ("I", 4), 5: ("II", 8), 6: ("b", 1), 7: ("B", 1),
         8: ("h", 2), 9: ("i", 4), 10: ("ii", 8), 11: ("f", 4), 12: ("d", 8), 16: ("Q", 8), 17: ("q", 8), 18: ("Q", 8)}


def parse_tiff(buf):
    """IFD list from the first bytes of a (Big)TIFF. Values stored past the buffer are returned as
    ('far', offset, count). Returns dict(bigtiff, ifds=[{tag: value}], ifd_end=max byte used)."""
    bo = {b"II": "<", b"MM": ">"}[buf[:2]]
    ver = struct.unpack(bo + "H", buf[2:4])[0]
    big = ver == 43
    off = struct.unpack(bo + "Q", buf[8:16])[0] if big else struct.unpack(bo + "I", buf[4:8])[0]
    ifds, end = [], 0
    while off and off + 8 < len(buf):
        if big:
            n = struct.unpack(bo + "Q", buf[off:off + 8])[0]
            p, esz, inl = off + 8, 20, 8
        else:
            n = struct.unpack(bo + "H", buf[off:off + 2])[0]
            p, esz, inl = off + 2, 12, 4
        tags = {}
        for i in range(n):
            e = buf[p + i * esz:p + (i + 1) * esz]
            if len(e) < esz:
                return {"bigtiff": big, "ifds": ifds, "ifd_end": None}
            tag, typ = struct.unpack(bo + "HH", e[:4])
            cnt = struct.unpack(bo + ("Q" if big else "I"), e[4:4 + (8 if big else 4)])[0]
            fmt, sz = TYPES.get(typ, ("B", 1))
            nbytes = sz * cnt
            raw = e[esz - inl:]
            if nbytes <= inl:
                data = raw[:nbytes]
            else:
                vo = struct.unpack(bo + ("Q" if big else "I"), raw)[0]
                if vo + nbytes > len(buf):
                    tags[tag] = ("far", vo, cnt)
                    end = max(end, vo + nbytes)
                    continue
                data = buf[vo:vo + nbytes]
                end = max(end, vo + nbytes)
            if typ == 2:
                tags[tag] = data.rstrip(b"\0").decode("latin-1")
            else:
                vals = struct.unpack(bo + fmt[0] * (cnt * len(fmt)), data)
                tags[tag] = vals
        ifds.append(tags)
        q = p + n * esz
        end = max(end, q + (8 if big else 4))
        if q + (8 if big else 4) > len(buf):
            break
        off = struct.unpack(bo + ("Q" if big else "I"), buf[q:q + (8 if big else 4)])[0]
    return {"bigtiff": big, "ifds": ifds, "ifd_end": end, "next_ifd_beyond": bool(off and off + 8 >= len(buf))}


def layout(url):
    buf, total = fetch(url, (0, HEAD - 1))
    if not buf:
        return None
    t = parse_tiff(buf)
    ifds = t["ifds"]
    if not ifds:
        return None
    f = ifds[0]
    def g(d, k, dflt=None):
        v = d.get(k)
        return v[0] if isinstance(v, tuple) and v and v[0] != "far" else dflt
    main = [d for d in ifds if not (g(d, 254, 0) & 4)]   # NewSubfileType bit 2 = mask
    lv = []
    for d in main:
        lv.append([g(d, 256), g(d, 257)])
    info = {
        "size": total, "bigtiff": t["bigtiff"], "ifd_end": t["ifd_end"], "n_ifd": len(ifds),
        "header_complete": not t.get("next_ifd_beyond") and (t["ifd_end"] or HEAD + 1) <= HEAD,
        "tiled": 322 in f, "tile": [g(f, 322), g(f, 323)], "compression": g(f, 259, 1), "predictor": g(f, 317, 1),
        "bps": g(f, 258), "sample_format": g(f, 339, 1), "spp": g(f, 277, 1), "planar": g(f, 284, 1),
        "nodata": f.get(42113), "levels": lv, "masks": len(ifds) - len(main),
        "scale": list(f.get(33550, ()))[:2], "tie": list(f.get(33922, ()))[:6],
    }
    return info


# ------------------------------------------------------------------ labels
def label(url, cache_dir):
    import hirise2engine as h2e
    p = os.path.join(cache_dir, "labels", os.path.basename(url))
    txt = h2e.fetch_text(url, p)
    return h2e.parse_pds_label(txt) if txt else None


def crs_of(url):
    import rasterio
    with rasterio.Env(GDAL_DISABLE_READDIR_ON_OPEN="EMPTY_DIR", CPL_VSIL_CURL_ALLOWED_EXTENSIONS=".tif"):
        with rasterio.open("/vsicurl/" + url) as s:
            return s.crs.to_proj4(), list(s.bounds), s.res[0], s.width, s.height, len(s.overviews(1)), s.nodata


def gather(item, cache_dir, sites_dir):
    import batch_convert as bc, hirise2engine as h2e
    sid = bc.site_name(item)
    cp = os.path.join(cache_dir, "sites", sid + ".json")
    if os.path.isfile(cp):
        with open(cp) as f:
            return json.load(f)
    dtm = item["assets"].get("dtm")
    ortho = bc.pick_ortho(item)
    rec = {"id": sid, "item_id": item["id"], "dtm": dtm, "ortho": ortho, "bbox": item["bbox"], "gsd": item["gsd"],
           "description": item.get("description", "")}
    try:
        rec["dtm_layout"] = layout(dtm)
        rec["dtm_crs"] = crs_of(dtm)
        if ortho:
            rec["ortho_layout"] = layout(ortho)
            rec["ortho_crs"] = crs_of(ortho)
            rec["ortho_label"] = h2e.ortho_label_info(label(ortho.rsplit(".", 1)[0] + ".LBL", cache_dir))
        obs = h2e.obs_from_ortho(ortho) if ortho else None
        if not obs:
            m = re.findall(r"(\d{6}_\d{4})", item["id"])
            obs = None
            if m:
                pre = re.search(r"((?:PSP|ESP)_" + m[0] + ")", sid)
                obs = pre.group(1) if pre else None
        rec["sun"] = h2e.sun_from_rdr_label(label(h2e.rdr_label_url(obs), cache_dir)) if obs else None
        mp = os.path.join(sites_dir, sid, "meta.json")
        if os.path.isfile(mp):
            with open(mp) as f:
                m = json.load(f)
            t = m.get("texture") or {}
            rec["converted"] = {"shift": t.get("ortho_shift_applied_m"), "yscale": t.get("ortho_northing_scale_applied"),
                                "lat": m.get("center_lat"), "lon": m.get("center_lon_east")}
    except Exception as e:
        rec["error"] = f"{type(e).__name__}: {e}"
    if "error" not in rec:
        os.makedirs(os.path.dirname(cp), exist_ok=True)
        with open(cp, "w") as f:
            json.dump(rec, f)
    return rec


def to_entry(rec):
    from rasterio.crs import CRS
    from rasterio.warp import transform as wt
    if rec.get("error") or not rec.get("dtm_crs") or not rec.get("dtm_layout"):
        return None
    proj4, bounds, res, w, h, nov, nodata = rec["dtm_crs"]
    crs = CRS.from_proj4(proj4)
    geo = CRS.from_proj4(f"+proj=longlat +R={MARS_R} +no_defs")
    w_, s_, e_, n_ = rec["bbox"]
    lat, lon = (s_ + n_) / 2, (w_ + e_) / 2
    if e_ - w_ > 90:   # converter: bbox wraps / encircles a pole -> raster centre
        (lon,), (lat,) = wt(crs, geo, [(bounds[0] + bounds[2]) / 2], [(bounds[1] + bounds[3]) / 2])
    if rec.get("converted") and rec["converted"].get("lat") is not None:
        lat, lon = rec["converted"]["lat"], rec["converted"]["lon"]
    lat_r = math.radians(lat)
    dlon = math.degrees(1.0 / (MARS_R * max(math.cos(lat_r), 1e-6)))
    dlat = math.degrees(1.0 / MARS_R)
    xs, ys = wt(geo, crs, [lon, lon + dlon, lon], [lat, lat, lat + dlat])
    ve = (xs[1] - xs[0], ys[1] - ys[0])
    vn = (xs[2] - xs[0], ys[2] - ys[0])
    kx, ky = math.hypot(*ve), math.hypot(*vn)
    conv = math.degrees(math.atan2(vn[0], vn[1]))
    proj = "eqc" if "+proj=eqc" in proj4 else ("stere_n" if "+lat_0=90" in proj4 else ("stere_s" if "+lat_0=-90" in proj4 else proj4))
    # ortho northing-scale fix (see converter/hirise2engine.ortho_yscale_candidate + batch_convert)
    oys, osh = 1.0, [0.0, 0.0]
    if rec.get("ortho") and rec.get("ortho_crs"):
        cv = rec.get("converted")
        if cv and cv.get("yscale") is not None:
            oys, osh = float(cv["yscale"]), [float(v) for v in (cv.get("shift") or [0, 0])]
        else:
            info = rec.get("ortho_label")
            if info and info.get("radius_km") and "EQUIRECT" in str(info.get("projection", "")).upper() and "+proj=eqc" in rec["ortho_crs"][0]:
                k = MARS_R / (info["radius_km"] * 1000.0)
                if abs(k - 1) > 1e-5:
                    db, ob = bounds, rec["ortho_crs"][1]
                    e0 = abs(ob[3] - db[3]) + abs(ob[1] - db[1])
                    e1 = abs(ob[3] * k - db[3]) + abs(ob[1] * k - db[1])
                    if e1 < 0.2 * e0 and e1 < 50 * res:
                        oys = k
    desc = rec.get("description", "")
    title = desc.split("rationale for creation was: ")[1].split(". This DTM")[0] if "rationale for creation was: " in desc else rec["item_id"]
    sun = rec.get("sun")
    r = lambda v, n: round(float(v), n)
    def rel(u):   # path under base; just the file name when it sits in the site's own folder
        p = u.split("/controlled/dtm/", 1)[1]
        return p[len(rec["id"]) + 1:] if p.startswith(rec["id"] + "/") else p
    return [rec["id"], title.strip(), r(lat, 5), r(lon, 5), rel(rec["dtm"]),
            rel(rec["ortho"]) if rec.get("ortho") else None,
            r(res, 6), w, h, nov, proj,
            [r(xs[0], 2), r(ys[0], 2)], [r(kx, 7), r(ky, 7)], r(conv, 4),
            [r(sun["azimuth_deg"], 2), r(sun["elevation_deg"], 2)] if sun else None,
            r(oys, 8), [r(osh[0], 2), r(osh[1], 2)],
            r((rec["dtm_layout"].get("size") or 0) / 1e6, 1)]


def summarise(recs):
    c = collections.Counter()
    for rec in recs:
        for kind in ("dtm", "ortho"):
            L = rec.get(kind + "_layout")
            if not L:
                c[f"{kind}: missing"] += 1
                continue
            c[f"{kind}: comp={L['compression']} pred={L['predictor']} bps={L['bps']} fmt={L['sample_format']} spp={L['spp']}"] += 1
            c[f"{kind}: tile={L['tile']} tiled={L['tiled']} bigtiff={L['bigtiff']}"] += 1
            c[f"{kind}: header in 64KB={L['header_complete']}"] += 1
            c[f"{kind}: overviews={len(L['levels']) - 1} masks={L['masks']}"] += 1
            if kind == "dtm":
                c[f"dtm: nodata={L['nodata']}"] += 1
        if rec.get("dtm_crs"):
            p = rec["dtm_crs"][0]
            c["dtm crs: " + ("eqc" if "eqc" in p else re.sub(r"\+lon_0=\S+ ", "", p))] += 1
        c[f"sun: {'yes' if rec.get('sun') else 'no'}"] += 1
    return dict(sorted(c.items()))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalog", default=os.path.join(REPO, "converter", "catalog.json"))
    ap.add_argument("--out", default=os.path.join(REPO, "viewer", "web", "live_index.json"))
    ap.add_argument("--cache", default=r"D:\marsmaker_data\live_cache")
    ap.add_argument("--sites", default=r"D:\marsmaker_data\sites")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--limit", type=int)
    a = ap.parse_args()
    items = json.load(open(a.catalog))
    items = [i for i in items if i["assets"].get("dtm")][: a.limit]
    t0, done, recs = time.time(), 0, []
    with ThreadPoolExecutor(a.workers) as ex:
        for rec in ex.map(lambda it: gather(it, a.cache, a.sites), items):
            recs.append(rec)
            done += 1
            if done % 50 == 0:
                print(f"{done}/{len(items)} ({time.time() - t0:.0f}s)", flush=True)
    entries, failed = [], []
    for rec in recs:
        e = to_entry(rec)
        (entries.append(e) if e else failed.append((rec["id"], rec.get("error"))))
    entries.sort(key=lambda e: e[0])
    out = {"v": 1, "base": BASE, "fields": FIELDS, "sites": entries}
    with open(a.out, "w") as f:
        json.dump(out, f, separators=(",", ":"))
    s = summarise(recs)
    with open(os.path.join(a.cache, "layout_summary.json"), "w") as f:
        json.dump({"summary": s, "failed": failed}, f, indent=1)
    for k, v in s.items():
        print(f"{v:6d}  {k}")
    print(f"{len(entries)} sites -> {a.out} ({os.path.getsize(a.out) / 1e3:.0f} KB); {len(failed)} failed: {failed[:10]}")


if __name__ == "__main__":
    main()
