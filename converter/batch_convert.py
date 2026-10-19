"""batch_convert — resumable, parallel, incremental HiRISE DTM -> game-engine tile converter.

Pipeline (producer/consumer):
  main thread      : reads catalog.json, HEADs the source files (ETag / size / Last-Modified),
                     decides what is new or changed vs. manifest.json, owns the manifest.
  download threads : (--downloads, default 16) fetch each site's DTM + matching ortho into
                     <data>/cache/<site>/, gated by a semaphore so at most --cache-sites sites
                     are on disk at once (bounded queue).
  worker processes : (--workers, default 12) convert one whole site each: reproject to a local
                     transverse-Mercator grid, cut 2017x2017 tiles (1 px overlap), 16-bit height
                     PNGs + 8-bit texture PNGs + meta.json. Processing is done in blocks of at
                     most 1 tile row x 4 tiles, so memory per worker stays well under 3 GB.
  On success the site's cache folder is deleted. The manifest is rewritten atomically after
  every site, outputs are written to <site>.tmp and renamed when complete, so Ctrl-C at any
  moment is safe: rerun the same command to resume.

Height encoding: 0 = nodata, 1..65535 = height_min..height_max (linear, shared by all tiles of
a site, so tiles line up vertically).

Texture encoding (v3): uint8 grey, 0 = nodata; a gentle reversible tone mapping of the ortho DN
(wide percentile window, gain capped at 4 grey levels per DN, mild gamma, +-0.5 DN deterministic
dither only when the source has few levels). The exact mapping is in meta.json texture.mapping.
Sun geometry of the ortho's observation (from the HiRISE RDR label) goes in meta.json "sun";
--albedo additionally writes approximate de-lit a_{row}_{col}.png tiles (see converter/delight.py).

Usage:
  python converter/batch_convert.py --filter dune
  python converter/batch_convert.py --limit 2 --workers 2 --downloads 4
  python converter/batch_convert.py --sites PSP_007018_1255_PSP_007229_1255 --workers 2 --albedo --low-priority
"""
import argparse, json, math, os, queue, re, shutil, sys, threading, time, traceback
import urllib.request
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))  # converter/: hirise2engine.py + catalog.json live here
sys.path.insert(0, HERE)

VERSION = 3                # bump when the output format changes -> everything is reprocessed
                           # 3: tone-mapped (not linear) textures, ortho northing-scale fix, "sun" block
TILE = 2017                # Unreal-friendly: 2016 quads = 16x16 components of 2x2 63-quad sections
STEP = TILE - 1            # tiles overlap by one pixel
CHUNK_TILES = 4            # tiles per reprojection block (memory bound)
MIN_FREE_GB = 10           # stop scheduling downloads below this much free space
DOWNLOAD_ORTHO_MAX = 400e6 # bigger orthos (0.25 m 'A' products) are read remotely via overviews


# ----------------------------------------------------------------------------- helpers
def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def head(url, tries=3):
    err = None
    for i in range(tries):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, method="HEAD"), timeout=30) as r:
                return {"url": url, "etag": (r.headers.get("ETag") or "").strip('"'),
                        "size": int(r.headers.get("Content-Length") or 0),
                        "last_modified": r.headers.get("Last-Modified")}
        except Exception as e:
            err = e
            time.sleep(2 * (i + 1))
    raise RuntimeError(f"HEAD failed: {url}: {err}")


class Stopped(Exception):
    pass


def download(src, dst, stop, tries=3):
    """Stream src (dict from head()) to dst. Resumes a partial file with a Range request on retry.
    Returns bytes actually transferred (0 when already cached)."""
    if os.path.exists(dst) and os.path.getsize(dst) == src["size"]:
        return 0
    tmp = dst + ".part"
    got, err = 0, None
    for attempt in range(1, tries + 1):
        try:
            have = os.path.getsize(tmp) if os.path.exists(tmp) else 0
            req = urllib.request.Request(src["url"])
            if 0 < have < src["size"]:
                req.add_header("Range", f"bytes={have}-")
                req.add_header("If-Range", '"%s"' % src["etag"])
            with urllib.request.urlopen(req, timeout=60) as r:
                mode = "ab" if (have and r.status == 206) else "wb"
                with open(tmp, mode) as f:
                    while True:
                        if stop.is_set():
                            raise Stopped()
                        chunk = r.read(1 << 20)
                        if not chunk:
                            break
                        f.write(chunk)
                        got += len(chunk)
            if os.path.getsize(tmp) != src["size"]:
                raise IOError(f"size {os.path.getsize(tmp)} != {src['size']}")
            os.replace(tmp, dst)
            return got
        except Stopped:
            raise
        except Exception as e:
            err = e
            if os.path.exists(tmp) and os.path.getsize(tmp) > src["size"]:
                os.remove(tmp)
            if attempt < tries:
                time.sleep(5 * attempt)
    raise RuntimeError(f"download failed after {tries} attempts: {os.path.basename(dst)}: {err}")


def dir_size(path):
    return sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(path) for f in fs)


def pick_ortho(item):
    """Ortho at the DTM's own resolution: RED_C (1 m DTMs) or RED_D (2 m DTMs); else RED_B (0.5 m),
    else RED_A (0.25 m, read remotely). Prefer the first observation named in the DTM id."""
    obs = re.findall(r"(\d{6}_\d{4})", item["id"])
    tifs = [v for k, v in item["assets"].items()
            if k.startswith("ortho_") and "label" not in k and "thumbnail" not in k and v.endswith("_ORTHO.tif")]
    for letter in "CDBA":
        cands = [v for v in tifs if f"_RED_{letter}_" in v]
        if cands:
            cands.sort(key=lambda v: next((i for i, o in enumerate(obs) if o in v), 99))
            return cands[0]
    return None


def atomic_json(obj, path):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=1)
    for i in range(10):
        try:
            os.replace(tmp, path)
            return
        except PermissionError:      # Windows: reader/AV holding the file
            time.sleep(0.2 * (i + 1))
    os.replace(tmp, path)


# ----------------------------------------------------------------------------- worker side
def _init_worker():
    os.environ.setdefault("GDAL_CACHEMAX", "256")
    os.environ.setdefault("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR")
    os.environ.setdefault("GDAL_HTTP_MAX_RETRY", "4")
    os.environ.setdefault("GDAL_HTTP_RETRY_DELAY", "3")
    os.environ.setdefault("CPL_VSIL_CURL_ALLOWED_EXTENSIONS", ".tif")


def _src_minmax(path):
    import numpy as np, rasterio
    lo, hi = np.inf, -np.inf
    with rasterio.open(path) as s:
        nod = s.nodata
        for _, w in s.block_windows(1):
            a = s.read(1, window=w)
            v = a[(a > nod * 0.999) if (nod is not None and nod < 0) else (a != nod)] if nod is not None else a
            v = v[np.isfinite(v)]
            if v.size:
                lo, hi = min(lo, float(v.min())), max(hi, float(v.max()))
    return lo, hi


def _align_scores(dtm, ortho, shifts, step=4.0):
    """Validate candidate ortho placements: (dx, dy) shifts in metres, or (dx, dy, yscale) with a
    northing scale applied first. Score = correlation between the ortho and a DTM hillshade,
    best over 8 sun azimuths (orthos are photos, lit from an unknown direction). Returns scores."""
    import numpy as np
    from rasterio.warp import reproject
    from rasterio.enums import Resampling
    from rasterio.crs import CRS
    import rasterio, hirise2engine as h2e
    h, hv, ht = h2e._lowres(dtm, step)
    o, ov, ot = h2e._lowres(ortho, step)
    with rasterio.open(dtm) as s:
        crs = s.crs
    h = np.where(hv, h, np.nanmean(np.where(hv, h, np.nan)))
    gy, gx = np.gradient(h)
    o = np.where(ov & (o > 0), o, np.nan).astype("float32")
    out = []
    for cand in shifts:
        dx, dy, ys = (tuple(cand) + (1.0,))[:3]
        dst = np.full(h.shape, np.nan, "float32")
        src_tf = rasterio.Affine.translation(dx, dy) * rasterio.Affine.scale(1.0, ys) * ot
        reproject(o, dst, src_transform=src_tf, src_crs=crs, src_nodata=np.nan,
                  dst_transform=ht, dst_crs=crs, dst_nodata=np.nan, resampling=Resampling.bilinear)
        m = hv & np.isfinite(dst)
        if m.sum() < 1000:
            out.append(-1.0)
            continue
        b = dst[m] - dst[m].mean()
        b /= b.std() + 1e-9
        best = -1.0
        for az in np.radians(np.arange(0, 360, 45)):
            a = -(gx[m] * np.sin(az) - gy[m] * np.cos(az))
            a = (a - a.mean()) / (a.std() + 1e-9)
            best = max(best, float((a * b).mean()))
        out.append(round(best, 4))
    return out


class _Seams:
    """Edge values of already-written tiles on the tile-boundary lines of the site grid (rows/cols that
    are multiples of STEP). apply() overwrites a tile's edges with known values (first writer wins,
    NaN = nodata is a value too), record() stores the tile's edges where still unknown. Corners are
    shared by a row line and a column line; since every tile applies before it records, both agree."""

    def __init__(self, W, H):
        self.W, self.H, self.rows, self.cols = W, H, {}, {}

    def _edges(self, a, r0, c0):
        n = a.shape[0]
        return ((self.rows, r0, c0, self.W, a[0, :]), (self.rows, r0 + n - 1, c0, self.W, a[-1, :]),
                (self.cols, c0, r0, self.H, a[:, 0]), (self.cols, c0 + n - 1, r0, self.H, a[:, -1]))

    def apply(self, a, r0, c0):
        import numpy as np
        for store, line, off, _, edge in self._edges(a, r0, c0):
            if line in store:
                vals, known = store[line]
                k = known[off:off + edge.size]
                if k.any():
                    edge[k] = vals[off:off + edge.size][k]

    def record(self, a, r0, c0):
        import numpy as np
        for store, line, off, length, edge in self._edges(a, r0, c0):
            if line not in store:
                store[line] = (np.full(length, np.nan, "float32"), np.zeros(length, bool))
            vals, known = store[line]
            k = ~known[off:off + edge.size]
            vals[off:off + edge.size][k] = edge[k]
            known[off:off + edge.size] = True


def convert_site(job):
    """Runs in a worker process. Never raises: returns a result dict."""
    t0 = time.time()
    out_final, out_tmp = job["out_dir"], job["out_dir"] + ".tmp"
    try:
        import numpy as np, rasterio
        from rasterio.enums import Resampling
        from rasterio.transform import from_origin
        from rasterio.warp import transform_bounds, transform as warp_transform
        from scipy import ndimage
        from PIL import Image
        import hirise2engine as h2e

        warnings = []
        shutil.rmtree(out_tmp, ignore_errors=True)
        os.makedirs(out_tmp)
        dtm = job["dtm_path"]

        # --- centre, CRS, resolution
        w, s, e, n = job["bbox"]
        lat, lon = (s + n) / 2, (w + e) / 2
        with rasterio.open(dtm) as src:
            src_crs = src.crs.to_proj4()
            src_res = src.res[0]
            if e - w > 90:   # bbox wraps the antimeridian / encircles a pole: use the raster centre
                cx, cy = src.xy(src.height // 2, src.width // 2)
                (lon,), (lat,) = warp_transform(src.crs, "EPSG:4326", [cx], [cy])  # only for angles
                warnings.append("bbox spans >90 deg lon; centre taken from raster")
        res = 1.0 if job["gsd"] < 1.5 else 2.0
        crs = h2e.local_crs(round(lat, 6), round(lon, 6))

        # --- coarse footprint (16 px cells) -> tight grid that is a whole number of tiles
        cs = 16 * res
        with rasterio.open(dtm) as src:
            l, b, r, t = transform_bounds(src.crs, crs, *src.bounds, densify_pts=101)
        cx0, cy1 = math.floor(l / cs) * cs, math.ceil(t / cs) * cs
        cshape = (math.ceil((cy1 - b) / cs) + 1, math.ceil((r - cx0) / cs) + 1)
        ctf = from_origin(cx0, cy1, cs, cs)
        cm = h2e.warp_to_local(dtm, crs, ctf, cshape, Resampling.nearest)
        cvalid = ndimage.binary_dilation(np.isfinite(cm), iterations=1)
        if not cvalid.any():
            raise RuntimeError("DTM has no valid data")
        rr, cc = np.nonzero(cvalid)
        xmin, xmax = cx0 + cc.min() * cs, cx0 + (cc.max() + 1) * cs
        ymax, ymin = cy1 - rr.min() * cs, cy1 - (rr.max() + 1) * cs
        kx0, kx1 = math.floor(xmin / res), math.ceil(xmax / res)
        ky1, ky0 = math.ceil(ymax / res), math.floor(ymin / res)
        nx = max(1, math.ceil((kx1 - kx0) / STEP))
        ny = max(1, math.ceil((ky1 - ky0) / STEP))
        kx0 -= (nx * STEP - (kx1 - kx0)) // 2
        ky1 += (ny * STEP - (ky1 - ky0)) // 2
        W, H = nx * STEP + 1, ny * STEP + 1
        # vertex (row i, col j) sits at x=(kx0+j)*res, y=(ky1-i)*res metres from the site centre
        def vx(j): return (kx0 + j) * res
        def vy(i): return (ky1 - i) * res

        def tile_has_data(tr, tc):   # from the coarse mask
            x0, x1, y1, y0 = vx(tc * STEP), vx(tc * STEP + STEP), vy(tr * STEP), vy(tr * STEP + STEP)
            c0, c1 = int((x0 - cx0) // cs), int((x1 - cx0) // cs) + 1
            r0, r1 = int((cy1 - y1) // cs), int((cy1 - y0) // cs) + 1
            sub = cvalid[max(0, r0):max(0, r1), max(0, c0):max(0, c1)]
            return bool(sub.any())

        # --- height range (bilinear never leaves the source range)
        zmin, zmax = _src_minmax(dtm)
        rng = max(zmax - zmin, 1e-3)
        k = 65534.0 / rng

        # --- texture prep
        tex_src, shift, yscale, mapping, align = None, (0.0, 0.0), 1.0, None, None
        olbl = h2e.ortho_label_info(_read_label(job.get("ortho_label_path")))
        sun = h2e.sun_from_rdr_label(_read_label(job.get("rdr_label_path")), job.get("rdr_label_src"))
        if job.get("ortho") and sun is None:
            warnings.append("sun geometry unavailable (HiRISE RDR label not fetched)")
        if job.get("ortho"):
            tex_src = job["ortho"]
            try:
                with rasterio.open(dtm) as a, rasterio.open(tex_src) as b_:
                    same = a.crs == b_.crs
                    ortho_crs = b_.crs
                if same:
                    est = tuple(float(v) for v in h2e.ortho_offset(dtm, tex_src))
                    step = max(4.0, 2 * src_res)
                    k_y = h2e.ortho_yscale_candidate(olbl, ortho_crs)
                    cands = [(0.0, 0.0), est] + ([(0.0, 0.0, k_y)] if k_y else [])
                    sc = _align_scores(dtm, tex_src, cands, step)
                    s0, s1 = sc[0], sc[1]
                    align = {"ortho_offset_estimate_m": [round(est[0], 1), round(est[1], 1)],
                             "score_zero": s0, "score_estimate": s1}
                    # ortho_offset is noisy at the 10s-of-metres level on bland terrain; only take
                    # its answer when it is clearly better than trusting the file's georeference
                    if math.hypot(*est) >= step and s1 > s0 + 0.03 and s1 > 0.05:
                        shift = est
                    if k_y:
                        s2 = sc[2]
                        align["northing_scale_candidate"] = round(k_y, 7)
                        align["score_northing_scale"] = s2
                        # label-derived northing scale (see h2e.ortho_yscale_candidate): take it when
                        # it clearly beats the alternatives, then look for a small residual shift
                        # second, independent evidence: DTM and ortho of one product share a footprint,
                        # so the scaled ortho's north/south edges should land on the DTM's
                        with rasterio.open(dtm) as a, rasterio.open(tex_src) as b_:
                            db, ob = a.bounds, b_.bounds
                        e0 = abs(ob.top - db.top) + abs(ob.bottom - db.bottom)
                        e1 = abs(ob.top * k_y - db.top) + abs(ob.bottom * k_y - db.bottom)
                        align["edge_mismatch_m"] = {"as_is": round(e0, 1), "scaled": round(e1, 1)}
                        edges_fit = e1 < 0.2 * e0 and e1 < 50 * src_res
                        if (s2 > max(s0, s1 if shift != (0.0, 0.0) else -1.0) + 0.03 and s2 > 0.05) or \
                                (edges_fit and s2 >= max(s0, s1) - 0.01):
                            yscale = k_y
                            offs = [(0.0, 0.0)] + [(dx, dy) for dx in (-step, 0.0, step)
                                                   for dy in (-step, 0.0, step) if dx or dy]
                            r = _align_scores(dtm, tex_src, [(dx, dy, k_y) for dx, dy in offs], step)
                            best = int(np.argmax(r))
                            shift = offs[best] if best and r[best] > r[0] + 0.01 else (0.0, 0.0)
                            align["score_northing_scale_final"] = r[best] if shift != (0.0, 0.0) else r[0]
                    if max(sc) < 0.05 and yscale == 1.0:
                        warnings.append(f"texture alignment unverified (scores {sc})")
                else:
                    warnings.append("ortho CRS differs from DTM CRS; no offset correction")
            except Exception as ex:
                warnings.append(f"ortho_offset failed ({ex}); no correction")
                shift, yscale = (0.0, 0.0), 1.0
            mapping = h2e.make_texture_mapping(h2e.sample_dn(tex_src, 4.0))

        # --- main pass: blocks of 1 tile row x CHUNK_TILES tiles
        # Each block is warped on its own, and the warp result at a given post depends (at the float32
        # rounding level, ~1e-4 m) on the block's window, so tiles from different blocks would disagree on
        # their shared edge posts after quantisation. Seams carries every written tile's edge values so the
        # neighbour in the next block reuses them: shared posts are bit-identical in h_ and t_ (and a_).
        h_seams, t_seams = _Seams(W, H), _Seams(W, H)
        tiles, nvalid = [], 0
        for tr in range(ny):
            for tc0 in range(0, nx, CHUNK_TILES):
                tcs = [tc for tc in range(tc0, min(nx, tc0 + CHUNK_TILES)) if tile_has_data(tr, tc)]
                if not tcs:
                    continue
                a, bnd = tcs[0], tcs[-1]
                col0, ncols = a * STEP, (bnd - a) * STEP + TILE
                row0 = tr * STEP
                btf = from_origin(vx(col0) - res / 2, vy(row0) + res / 2, res, res)
                hb = h2e.warp_to_local(dtm, crs, btf, (TILE, ncols), Resampling.bilinear)
                tb = None
                if tex_src:
                    tb = h2e.warp_to_local(tex_src, crs, btf, (TILE, ncols), Resampling.bilinear, shift=shift,
                                           yscale=yscale)
                for tc in tcs:
                    c = (tc - a) * STEP
                    h = hb[:, c:c + TILE]              # views: edits also reach the next tile of this block
                    h_seams.apply(h, row0, tc * STEP)
                    if tb is not None:
                        t_seams.apply(tb[:, c:c + TILE], row0, tc * STEP)
                    fin = np.isfinite(h)
                    nv = int(fin.sum())
                    if nv == 0:
                        continue
                    h_seams.record(h, row0, tc * STEP)
                    if tb is not None:
                        t_seams.record(tb[:, c:c + TILE], row0, tc * STEP)
                    q = np.zeros(h.shape, "<u2")
                    q[fin] = np.clip(np.rint((h[fin] - zmin) * k) + 1, 1, 65535).astype("<u2")
                    hname = f"h_{tr}_{tc}.png"
                    Image.fromarray(q).save(os.path.join(out_tmp, hname), compress_level=6)
                    tname = None
                    if tb is not None:
                        t8 = h2e.encode_texture(tb[:, c:c + TILE], mapping, tr * STEP, tc * STEP)
                        tname = f"t_{tr}_{tc}.png"
                        Image.fromarray(t8).save(os.path.join(out_tmp, tname), compress_level=6)
                    nvalid += nv
                    ex_m, ny_m = vx(tc * STEP), vy(tr * STEP)
                    tiles.append({"x": tc, "y": tr, "height": hname, "texture": tname,
                                  "valid_fraction": round(nv / h.size, 4),
                                  "top_left_m": {"east": ex_m, "north": ny_m},
                                  "unreal_location_cm": [ex_m * 100, -ny_m * 100, None],
                                  "unity_position_m": [ex_m, None, vy(tr * STEP + STEP)]})
                del hb, tb
        if not tiles:
            raise RuntimeError("no tile contained valid data")

        size_m = STEP * res
        u_scale_z = 128 * 100 * rng / 65534            # Unreal: z_cm = loc + (v-32768)*scale_z/128
        u_loc_z = (zmin + 32767 * rng / 65534) * 100
        y_size = rng * 65535 / 65534                    # Unity: h = pos_y + v/65535*size_y
        y_pos = zmin - rng / 65534
        for t_ in tiles:
            t_["unreal_location_cm"][2] = u_loc_z
            t_["unity_position_m"][1] = y_pos
        meta = {
            "source": job["key"], "id": job["id"], "description": job["description"],
            "license": "CC0-1.0 (NASA/JPL/University of Arizona/USGS)",
            "source_dtm": job["dtm_src"], "source_ortho": job.get("ortho_src"),
            "source_crs": src_crs, "source_res_m": src_res,
            "converter_version": VERSION, "created": now(),
            "center_lat": lat, "center_lon_east": lon, "mars_radius_m": h2e.MARS_R,
            "projection": crs.to_proj4(),
            "metres_per_pixel": res, "tile_px": TILE, "tile_overlap_px": 1, "tile_size_m": size_m,
            "grid_tiles": [nx, ny], "grid_px": [W, H],
            "grid_top_left_m": {"east": vx(0), "north": vy(0)},
            "tile_naming": "h_{row}_{col}.png (height) / t_{row}_{col}.png (texture) / optional "
                           "a_{row}_{col}.png (approximate albedo, see 'albedo'), row 0 = north; "
                           "tile (col,row) top-left vertex = grid_top_left + (col*tile_size_m, -row*tile_size_m); "
                           "tiles with no data are not written",
            "height_encoding": "uint16, 0 = nodata, v in 1..65535 -> height_min_m + (v-1)*height_range_m/65534",
            "height_min_m": zmin, "height_max_m": zmax, "height_range_m": rng,
            "valid_fraction": round(nvalid / (len(tiles) * TILE * TILE), 4),
            "texture": None if not tex_src else {
                "encoding": "uint8 greyscale, 0 = nodata; v = 1 + round(254 * clip((dn - dn_lo) / (dn_hi - dn_lo), "
                            "0, 1) ** gamma), with +-dither_dn DN of deterministic noise added to dn first",
                "inverse": "dn = dn_lo + (dn_hi - dn_lo) * ((v - 1) / 254) ** (1 / gamma); I/F = dn * "
                           "iof_scaling_factor + iof_offset (ortho_label)",
                "mapping": mapping,
                "stretch_dn": [mapping["dn_lo"], mapping["dn_hi"]],   # pre-v3 key; no longer a linear stretch
                "ortho_label": olbl,
                "ortho_shift_applied_m": [round(shift[0], 2), round(shift[1], 2)],
                "ortho_northing_scale_applied": yscale,
                "alignment_check": align},
            "sun": sun,
            "unreal": {
                "import": "World Partition landscape tiled import expects *_x{col}_y{row}.png names: rename h_{row}_{col}.png accordingly, or import tiles one by one at unreal_location_cm",
                "landscape": {"quads_per_section": 63, "sections_per_component": "2x2", "components": "16x16 per tile"},
                "scale_xy": res * 100, "scale_z": u_scale_z, "location_z_cm": u_loc_z,
                "note": "scale_z = range*100/512 (exact: 128*100*range/65534 for the 1..65535 encoding); "
                        "Unreal Y points south, so Y = -north",
            },
            "unity": {
                "heightmap": "convert PNG to 16-bit RAW (Windows byte order); Unity needs 2^n+1 resolution: "
                             "import as 2049 and let Unity resample, or crop",
                "terrain_size_m": [size_m, y_size, size_m], "position_y_m": y_pos,
                "tile_position": "unity_position_m = [east, y, north] of each tile's south-west corner",
            },
            "warnings": warnings,
            "tiles": tiles,
        }
        with open(os.path.join(out_tmp, "meta.json"), "w") as f:
            json.dump(meta, f, indent=1)
        if job.get("albedo") and tex_src:
            try:
                import delight
                delight.delight_site(out_tmp, label_cache=job.get("label_cache"))
            except Exception as ex:
                warnings.append(f"albedo failed: {type(ex).__name__}: {ex}")
                with open(os.path.join(out_tmp, "meta.json"), "w") as f:
                    json.dump(meta, f, indent=1)

        # --- 256 px hillshade preview of the whole site grid (nodata = black)
        pres = max(W, H) * res / 256
        pw, ph = max(1, round(W * res / pres)), max(1, round(H * res / pres))
        pg = h2e.warp_to_local(dtm, crs, from_origin(vx(0) - res / 2, vy(0) + res / 2, pres, pres), (ph, pw),
                               Resampling.average)
        gy, gx = np.gradient(np.where(np.isfinite(pg), pg, np.nanmean(pg)), pres)
        az, alt = math.radians(315), math.radians(45)
        dzde, dzdn = gx, -gy                            # rows run south
        lx, ly, lz = math.sin(az) * math.cos(alt), math.cos(az) * math.cos(alt), math.sin(alt)  # light from NW
        hs = (-dzde * lx - dzdn * ly + lz) / np.sqrt(dzde ** 2 + dzdn ** 2 + 1)
        hs = np.where(np.isfinite(pg), np.clip(hs, 0, 1) * 254 + 1, 0).astype("uint8")
        Image.fromarray(hs).save(os.path.join(out_tmp, "preview.png"))

        # --- publish atomically
        old = out_final + ".old"
        shutil.rmtree(old, ignore_errors=True)
        if os.path.exists(out_final):
            os.replace(out_final, old)
        os.replace(out_tmp, out_final)
        shutil.rmtree(old, ignore_errors=True)
        return {"status": "ok", "convert_s": round(time.time() - t0, 1), "out_bytes": dir_size(out_final),
                "n_tiles": len(tiles), "grid_tiles": [nx, ny], "res": res,
                "height_min_m": zmin, "height_max_m": zmax, "valid_fraction": meta["valid_fraction"],
                "ortho_shift_m": [round(shift[0], 1), round(shift[1], 1)], "alignment": align, "source_crs": src_crs,
                "warnings": warnings}
    except Exception as ex:
        shutil.rmtree(out_tmp, ignore_errors=True)
        return {"status": "failed", "stage": "convert", "convert_s": round(time.time() - t0, 1),
                "error": f"{type(ex).__name__}: {ex}", "trace": traceback.format_exc()[-1500:]}


# ----------------------------------------------------------------------------- orchestrator
def _read_label(path):
    if not path or not os.path.isfile(path):
        return None
    import hirise2engine as h2e
    with open(path, encoding="latin-1") as f:
        return h2e.parse_pds_label(f.read())


def set_low_priority():
    try:
        import psutil
        psutil.Process().nice(psutil.BELOW_NORMAL_PRIORITY_CLASS if os.name == "nt" else 10)
    except Exception as e:
        print(f"could not lower priority: {e}")


def site_name(item):
    return item["key"].rstrip("/").split("/")[-1]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--filter", help="regex matched (case-insensitive) against the site description")
    ap.add_argument("--limit", type=int, help="max number of sites to process this run")
    ap.add_argument("--workers", type=int, default=12, help="conversion processes")
    ap.add_argument("--downloads", type=int, default=16, help="download threads")
    ap.add_argument("--cache-sites", type=int, default=20, help="max sites held in the download cache")
    ap.add_argument("--data", default=r"D:\marsmaker_data", help="cache + manifest root")
    ap.add_argument("--out", default=None, help="output root (default <data>/sites)")
    ap.add_argument("--no-texture", action="store_true", help="skip orthoimage textures")
    ap.add_argument("--catalog", default=os.path.join(HERE, "catalog.json"))
    ap.add_argument("--sites", help="comma-separated site folder names (e.g. PSP_007018_1255_PSP_007229_1255)")
    ap.add_argument("--albedo", action="store_true", help="also write approximate de-lit a_*.png tiles")
    ap.add_argument("--force", action="store_true", help="reprocess matching sites even if up to date")
    ap.add_argument("--low-priority", action="store_true", help="run (and spawn workers) at below-normal priority")
    a = ap.parse_args()
    if a.low_priority:
        set_low_priority()          # worker processes inherit the priority class

    data = os.path.abspath(a.data)
    out_root = os.path.abspath(a.out or os.path.join(data, "sites"))
    for p in (data, out_root):
        if os.path.splitdrive(p)[0].upper() == "C:" and not os.environ.get("MARS_ALLOW_C"):
            sys.exit(f"refusing to put bulk data on C: ({p}); set MARS_ALLOW_C=1 if C: is your only drive")
    cache = os.path.join(data, "cache")
    os.makedirs(cache, exist_ok=True)
    label_cache = os.path.join(data, "labels")
    os.makedirs(out_root, exist_ok=True)
    man_path = os.path.join(data, "manifest.json")
    manifest = json.load(open(man_path)) if os.path.exists(man_path) else {}
    log = open(os.path.join(data, "batch.log"), "a", buffering=1)

    def say(msg):
        line = f"{datetime.now():%H:%M:%S} {msg}"
        print(line, flush=True)
        log.write(line + "\n")

    def save():
        atomic_json(manifest, man_path)

    items = json.load(open(a.catalog))
    if a.filter:
        rx = re.compile(a.filter, re.I)
        items = [i for i in items if rx.search(i.get("description", ""))]
    if a.sites:
        want = {x.strip() for x in a.sites.split(",") if x.strip()}
        items = [i for i in items if site_name(i) in want]
    texture = not a.no_texture
    say(f"run start: {len(items)} sites match; texture={texture} workers={a.workers} downloads={a.downloads}")

    # --- current source state (HEAD) for every matching site
    def state(item):
        o = pick_ortho(item) if texture else None
        return item, head(item["assets"]["dtm"]), (head(o) if o else None)
    states = []
    with ThreadPoolExecutor(32) as ex:
        futs = [(i, ex.submit(state, i)) for i in items]
        for i, f in futs:
            try:
                states.append(f.result())
            except Exception as e:
                say(f"HEAD error {site_name(i)}: {e} (left as is this run)")

    todo, skipped = [], 0
    for item, dsrc, osrc in states:
        sid = site_name(item)
        m = manifest.get(sid)
        unchanged = not a.force and bool(m and m.get("status") in ("done", "skipped") and m.get("version") == VERSION
                         and m["dtm"]["etag"] == dsrc["etag"]
                         and (not texture or (m.get("texture") and (m.get("ortho") or {}).get("etag") == (osrc or {}).get("etag")))
                         and os.path.isfile(os.path.join(out_root, sid, "meta.json")))
        if unchanged:
            skipped += 1
            m["status"] = "skipped"
            m["checked"] = now()
        else:
            reason = "new" if not m else ("changed" if m.get("status") in ("done", "skipped") else "retry")
            todo.append((item, dsrc, osrc, reason))
    if a.limit is not None:
        todo = todo[:a.limit]
    for item, dsrc, osrc, reason in todo:
        sid = site_name(item)
        prev = manifest.get(sid) or {}
        manifest[sid] = {"status": "queued", "reason": reason, "key": item["key"], "id": item["id"],
                         "description": item["description"], "dtm": dsrc, "ortho": osrc, "texture": texture,
                         "version": VERSION, "queued": now(),
                         **({"previous": {k: prev.get(k) for k in ("status", "updated", "error")}} if prev else {})}
    save()
    say(f"{skipped} up to date (skipped), {len(todo)} to process "
        f"({sum(r == 'new' for *_, r in todo)} new, {sum(r == 'changed' for *_, r in todo)} changed, "
        f"{sum(r == 'retry' for *_, r in todo)} retry)")
    if not todo:
        return

    stop = threading.Event()
    slots = threading.Semaphore(a.cache_sites)
    events = queue.Queue()          # (sid, fields, final) -- the main thread is the only manifest writer
    pool = ProcessPoolExecutor(a.workers, initializer=_init_worker)
    dl = ThreadPoolExecutor(a.downloads)
    running = {}                    # sid -> future, to report 'converting' once a worker picks it up
    inflight = [0]                  # download tasks not yet finished
    lock = threading.Lock()

    def fetch_and_submit(item, dsrc, osrc):
        try:
            _fetch_and_submit(item, dsrc, osrc)
        finally:
            with lock:
                inflight[0] -= 1

    def _fetch_and_submit(item, dsrc, osrc):
        while not slots.acquire(timeout=0.5):
            if stop.is_set():
                return
        if stop.is_set():
            slots.release()
            return
        sid, t0 = site_name(item), time.time()
        sdir = os.path.join(cache, sid)
        try:
            if min(shutil.disk_usage(data).free, shutil.disk_usage(out_root).free) < MIN_FREE_GB * 1e9:
                stop.set()
                raise RuntimeError(f"less than {MIN_FREE_GB} GB free; stopping")
            events.put((sid, {"status": "downloading", "started": now()}, False))
            os.makedirs(sdir, exist_ok=True)
            dpath = os.path.join(sdir, f"{dsrc['etag'][:12]}_{os.path.basename(dsrc['url'])}")
            nbytes = download(dsrc, dpath, stop)
            opath = None
            if osrc:
                if osrc["size"] <= DOWNLOAD_ORTHO_MAX:
                    opath = os.path.join(sdir, f"{osrc['etag'][:12]}_{os.path.basename(osrc['url'])}")
                    nbytes += download(osrc, opath, stop)
                else:
                    opath = "/vsicurl/" + osrc["url"]   # 0.25 m product: let GDAL read the overviews
            olbl_path = rdr_path = rdr_url = None
            if osrc:   # small PDS labels: photometric scaling / projection radius, and the sun geometry
                import hirise2engine as h2e
                ou = osrc["url"].rsplit(".", 1)[0] + ".LBL"
                olbl_path = os.path.join(label_cache, os.path.basename(ou))
                if h2e.fetch_text(ou, olbl_path) is None:
                    olbl_path = None
                obs = h2e.obs_from_ortho(osrc["url"])
                if obs:
                    rdr_url = h2e.rdr_label_url(obs)
                    rdr_path = os.path.join(label_cache, os.path.basename(rdr_url))
                    if h2e.fetch_text(rdr_url, rdr_path) is None:
                        rdr_path = None
            dl_info = {"download_s": round(time.time() - t0, 1), "downloaded_bytes": nbytes,
                       "ortho_mode": None if not osrc else ("remote" if opath.startswith("/vsicurl/") else "cached")}
            job = {"key": item["key"], "id": item["id"], "description": item["description"], "bbox": item["bbox"],
                   "gsd": item["gsd"], "dtm_path": dpath, "dtm_src": dsrc["url"],
                   "ortho": opath, "ortho_src": osrc["url"] if osrc else None,
                   "ortho_label_path": olbl_path, "rdr_label_path": rdr_path, "rdr_label_src": rdr_url,
                   "albedo": a.albedo, "label_cache": label_cache,
                   "out_dir": os.path.join(out_root, sid)}
        except Stopped:
            slots.release()
            return
        except Exception as e:
            slots.release()
            events.put((sid, {"status": "failed", "stage": "download", "error": str(e),
                              "seconds": round(time.time() - t0, 1)}, True))
            return

        def done(fut):
            try:
                r = fut.result()
            except Exception as e:   # broken pool, cancelled
                r = {"status": "failed", "stage": "convert", "error": f"{type(e).__name__}: {e}"}
            if r["status"] == "ok":
                shutil.rmtree(sdir, ignore_errors=True)
            r["status"] = "done" if r["status"] == "ok" else "failed"
            slots.release()
            events.put((sid, {**dl_info, **r, "seconds": round(time.time() - t0, 1)}, True))
        try:
            fut = pool.submit(convert_site, job)
            events.put((sid, {**dl_info, "_future": fut}, False))
            fut.add_done_callback(done)
        except Exception as e:
            slots.release()
            events.put((sid, {"status": "failed", "stage": "submit", "error": str(e)}, True))

    t_start = time.time()
    inflight[0] = len(todo)
    for item, dsrc, osrc, _ in todo:
        dl.submit(fetch_and_submit, item, dsrc, osrc)

    n_done = n_ok = 0
    tot_dl = tot_out = 0
    interrupted = False
    try:
        while n_done < len(todo):
            try:
                sid, fields, final = events.get(timeout=1.0)
            except queue.Empty:
                changed = False
                for s_, f_ in list(running.items()):
                    if f_.running() and manifest[s_]["status"] != "converting":
                        manifest[s_]["status"] = "converting"
                        manifest[s_]["convert_started"] = now()
                        changed = True
                if changed:
                    save()
                if stop.is_set() and inflight[0] <= 0 and not running and events.empty():
                    break    # stopped early (disk full); nothing left in flight
                continue
            rec = manifest[sid]
            fut = fields.pop("_future", None)
            if fut is not None:
                running[sid] = fut
            rec.update(fields)
            rec["updated"] = now()
            if not final:
                save()
                continue
            running.pop(sid, None)
            if rec["status"] == "done":
                for k in ("trace", "stage", "error"):
                    rec.pop(k, None)
            n_done += 1
            save()
            tot_dl += rec.get("downloaded_bytes", 0) or 0
            if rec["status"] == "done":
                n_ok += 1
                tot_out += rec["out_bytes"]
                extra = f"{rec['n_tiles']} tiles, {rec['out_bytes'] / 1e6:.0f} MB, shift {rec['ortho_shift_m']}"
            else:
                extra = f"FAILED[{rec.get('stage')}] {rec.get('error')}"
            el = time.time() - t_start
            say(f"[{n_done}/{len(todo)}] {sid} {rec['status']} dl {rec.get('download_s', '-')}s "
                f"conv {rec.get('convert_s', '-')}s  {extra}  | {tot_dl / 1e9:.1f} GB in, "
                f"{tot_out / 1e9:.2f} GB out, {el / 60:.1f} min")
    except KeyboardInterrupt:
        interrupted = True
        say("Ctrl-C: stopping; finished sites are saved, rerun to resume")
    finally:
        stop.set()
        for rec in manifest.values():      # anything in flight goes back to the queue
            if rec.get("status") in ("downloading", "converting"):
                rec["status"] = "queued"
        save()
        dl.shutdown(wait=not interrupted, cancel_futures=True)
        pool.shutdown(wait=not interrupted, cancel_futures=True)
    el = time.time() - t_start
    say(f"run end: {n_ok} done, {n_done - n_ok} failed, {len(todo) - n_done} not finished; wall {el / 60:.1f} min; "
        f"{tot_dl / 1e9:.2f} GB downloaded; {tot_out / 1e9:.2f} GB written; "
        f"{(el / n_done if n_done else 0):.1f} s/site wall")
    if interrupted:
        os._exit(130)


if __name__ == "__main__":
    main()
