"""hirise2engine — turn a HiRISE DTM (USGS CC0 COGs on AWS) into true 1:1 game-engine terrain.

The USGS COGs use a global equirectangular projection (standard parallel 0), so east-west
pixel spacing is only correct at the equator. We reproject a crop into a local transverse
Mercator grid centred on the site, which is metric and true-scale in both axes.

Outputs (in out/<name>/):
  heightmap_16bit.png   Unreal Landscape heightmap (also fine for Godot Terrain3D)
  heightmap_16bit.r16   Unity Terrain RAW (little-endian, top-down)
  texture.png           Orthoimage draped to the same grid
  heights_f32.npy       Raw metres, for engineers / ML
  meta.json             Real-world location, metres-per-pixel, height range, engine scale values

Usage:
  python hirise2engine.py ESP_013017_1325 --lat -47.23 --lon 19.48 --size 2016 --res 1.0 --name kaiser
"""
import argparse, json, math, os, sys
import numpy as np
import rasterio
from rasterio.crs import CRS
from rasterio.enums import Resampling
from rasterio.warp import reproject, transform_bounds
from rasterio.transform import from_origin
from rasterio.windows import from_bounds, transform as window_transform
from PIL import Image

MARS_R = 3396190.0
CATALOG = os.path.join(os.path.dirname(__file__), "catalog.json")


def find_item(obs_id):
    for item in json.load(open(CATALOG)):
        if obs_id in item["key"]:
            return item
    sys.exit(f"{obs_id} not in catalog.json (run tools/fetch_catalog.py)")


def local_crs(lat, lon):
    return CRS.from_proj4(f"+proj=tmerc +lat_0={lat} +lon_0={lon} +k=1 +x_0=0 +y_0=0 "
                          f"+R={MARS_R} +units=m +no_defs")


def _lowres(url, step):
    with rasterio.open(url) as s:
        shape = (int(s.height * s.res[0] / step), int(s.width * s.res[0] / step))
        a = s.read(1, out_shape=shape, resampling=Resampling.average).astype("float64")
        nod = s.nodata
        valid = (a > nod * 0.999) if (nod is not None and nod < 0) else (a != (nod or 0))
        return a, valid, s.transform * rasterio.Affine.scale(s.width / shape[1], s.height / shape[0])


def ortho_offset(dtm_url, ortho_url, step=8.0):
    """Some USGS orthos carry a wrong georeference. Find the (dx, dy) in metres that lines the
    ortho up with the DTM: coarse match on the valid-data footprint, refined by correlating
    slope (DTM) against image gradient (ortho)."""
    h, hv, ht = _lowres(dtm_url, step)
    o, ov, ot = _lowres(ortho_url, step)
    def centroid(valid, t):
        r, c = np.nonzero(valid)
        return t * (c.mean(), r.mean())
    (hx, hy), (ox, oy) = centroid(hv, ht), centroid(ov, ot)
    dx, dy = hx - ox, hy - oy
    # refine: gradient magnitude images on the DTM grid
    from scipy import signal
    hg = np.hypot(*np.gradient(np.where(hv, h, np.nan)))
    # paste the shifted ortho onto the DTM's low-res grid (same CRS, same pixel size)
    oo = np.full(h.shape, np.nan)
    c0 = int(round((ot.c + dx - ht.c) / step)); r0 = int(round((ht.f - (ot.f + dy)) / step))
    src = np.where(ov, o, np.nan)
    rs, cs = max(0, -r0), max(0, -c0)
    rd, cd = max(0, r0), max(0, c0)
    nr = min(src.shape[0] - rs, h.shape[0] - rd); nc = min(src.shape[1] - cs, h.shape[1] - cd)
    oo[rd:rd + nr, cd:cd + nc] = src[rs:rs + nr, cs:cs + nc]
    og = np.hypot(*np.gradient(oo))
    m = np.isfinite(hg) & np.isfinite(og)
    a = np.where(m, (hg - np.nanmean(hg[m])) / np.nanstd(hg[m]), 0)
    b = np.where(m, (og - np.nanmean(og[m])) / np.nanstd(og[m]), 0)
    cc = signal.fftconvolve(a, b[::-1, ::-1], mode="same")
    r, c = np.unravel_index(np.argmax(cc), cc.shape)
    sr, sc = r - cc.shape[0] // 2, c - cc.shape[1] // 2
    return dx + sc * step, dy - sr * step


# ----------------------------------------------------------------------------- PDS labels
def parse_pds_label(text):
    """Flat {KEY: value} of a PDS3 label (first occurrence wins; units like <DEG> and quotes stripped,
    numbers converted). Good enough for the scalar keywords we need."""
    out = {}
    for line in text.splitlines():
        line = line.split("/*")[0]
        if "=" not in line:
            continue
        k, v = (s.strip() for s in line.split("=", 1))
        if not k or k in out or not v:
            continue
        v = v.split("<")[0].strip().strip('"').strip()
        try:
            v = float(v)
        except ValueError:
            pass
        out[k] = v
    return out


def fetch_text(url, cache_path=None, timeout=30):
    """GET a small text file, cached on disk when cache_path is given. Returns None on failure."""
    import urllib.request
    if cache_path and os.path.isfile(cache_path) and os.path.getsize(cache_path) > 0:
        with open(cache_path, encoding="latin-1") as f:
            return f.read()
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            text = r.read().decode("latin-1")
    except Exception:
        return None
    if cache_path:
        os.makedirs(os.path.dirname(cache_path), exist_ok=True)
        with open(cache_path + ".part", "w", encoding="latin-1") as f:
            f.write(text)
        os.replace(cache_path + ".part", cache_path)
    return text


def obs_from_ortho(url):
    """'.../PSP_007018_1255_RED_C_01_ORTHO.tif' -> 'PSP_007018_1255'."""
    import re
    m = re.search(r"((?:PSP|ESP)_\d{6}_\d{4})_RED", os.path.basename(url or ""))
    return m.group(1) if m else None


def rdr_label_url(obs):
    """HiRISE RDR (RED mosaic) PDS label of an observation; it carries the illumination geometry
    that the DTM/ortho labels on astrogeo-ard do not."""
    orbit = int(obs.split("_")[1])
    o0 = orbit // 100 * 100
    return (f"https://hirise-pds.lpl.arizona.edu/PDS/RDR/{obs[:3]}/ORB_{o0:06d}_{o0 + 99:06d}/"
            f"{obs}/{obs}_RED.LBL")


def sun_from_rdr_label(lbl, url=None):
    """Sun direction from the RDR label. SUB_SOLAR_AZIMUTH and NORTH_AZIMUTH are both measured
    clockwise from the image's 3 o'clock direction, so the geographic azimuth (clockwise from north)
    is their difference. Elevation = 90 - INCIDENCE_ANGLE (at the image centre, w.r.t. the ellipsoid)."""
    need = ("INCIDENCE_ANGLE", "SUB_SOLAR_AZIMUTH", "NORTH_AZIMUTH")
    if not lbl or not all(isinstance(lbl.get(k), float) for k in need):
        return None
    return {
        "azimuth_deg": round((lbl["SUB_SOLAR_AZIMUTH"] - lbl["NORTH_AZIMUTH"]) % 360, 3),
        "elevation_deg": round(90 - lbl["INCIDENCE_ANGLE"], 3),
        "azimuth_convention": "degrees clockwise from north (direction towards the sun)",
        "incidence_deg": lbl["INCIDENCE_ANGLE"], "emission_deg": lbl.get("EMISSION_ANGLE"),
        "phase_deg": lbl.get("PHASE_ANGLE"), "sub_solar_azimuth_deg": lbl["SUB_SOLAR_AZIMUTH"],
        "north_azimuth_deg": lbl["NORTH_AZIMUTH"], "solar_longitude_deg": lbl.get("SOLAR_LONGITUDE"),
        "start_time": lbl.get("START_TIME"), "observation": lbl.get("OBSERVATION_ID") or lbl.get("PRODUCT_ID"),
        "source": url or "HiRISE RDR label",
    }


def ortho_label_info(lbl):
    """Photometric scaling + projection radius from an *_ORTHO.LBL (astrogeo-ard asset)."""
    if not lbl:
        return None
    info = {k_out: lbl.get(k_in) for k_out, k_in in (
        ("sample_bits", "SAMPLE_BITS"), ("iof_scaling_factor", "SCALING_FACTOR"), ("iof_offset", "OFFSET"),
        ("projection", "MAP_PROJECTION_TYPE"), ("radius_km", "A_AXIS_RADIUS"),
        ("center_latitude", "CENTER_LATITUDE"), ("center_longitude", "CENTER_LONGITUDE"))}
    return info


def ortho_yscale_candidate(info, src_crs):
    """Many orthos on astrogeo-ard were placed with northings computed on the label's *local*
    radius (A_AXIS_RADIUS, e.g. 3384.42 km) but tagged with the 3396.19 km IAU sphere, so their
    northing is short by R_iau/R_local (11 km at Russell crater). Returns that factor for
    equirectangular orthos whose label radius differs, else None. Callers must validate it."""
    if not info or not info.get("radius_km") or "EQUIRECT" not in str(info.get("projection", "")).upper():
        return None
    if "+proj=eqc" not in src_crs.to_proj4():
        return None
    k = MARS_R / (info["radius_km"] * 1000.0)
    return k if abs(k - 1) > 1e-5 else None


# ----------------------------------------------------------------------------- texture tone mapping
TEX_PCT = (0.05, 99.95)   # wide window: only the extreme tails are clipped
TEX_MAX_GAIN = 4.0        # never spread one source DN over more than ~4 output grey levels
TEX_DITHER_GAIN = 2.0     # above this many output levels per DN, de-quantise with +-0.5 DN noise


def sample_dn(url, step=4.0, shift=(0.0, 0.0)):
    """Nearest-neighbour sample of the ortho DN on a ~step metre lattice (keeps true quantisation,
    unlike averaging). Returns the valid (>0) values as float64."""
    with rasterio.open(url) as s:
        f = max(1, int(step / s.res[0]))
        a = s.read(1, out_shape=(max(1, s.height // f), max(1, s.width // f)), resampling=Resampling.nearest)
    v = a[a > 0].astype("float64")
    return v


def make_texture_mapping(v):
    """Choose a gentle, reversible DN -> uint8 mapping from sample DN values v (valid only).
    v8 = 1 + round(254 * clip((dn - dn_lo) / (dn_hi - dn_lo), 0, 1) ** gamma)."""
    if v.size == 0:
        return {"dn_lo": 0.0, "dn_hi": 1.0, "gamma": 1.0, "dither_dn": 0.0}
    vmax = float(v.max())
    bits = 8 if vmax <= 255 else (10 if vmax <= 1023 else (12 if vmax <= 4095 else 16))
    lo, hi = (float(x) for x in np.percentile(v, TEX_PCT))
    med = float(np.median(v))
    widened = False
    min_win = 254.0 / TEX_MAX_GAIN
    if hi - lo < min_win:            # few real grey levels: do not stretch them into bands
        c = (lo + hi) / 2
        lo, hi = c - min_win / 2, c + min_win / 2
        if lo < 1:
            lo, hi = 1.0, 1.0 + min_win
        widened = True
    xm = min(max((med - lo) / (hi - lo), 0.05), 0.95)
    gamma = float(np.clip(math.log(0.45) / math.log(xm), 0.75, 1.33))   # mild: median towards 45% grey
    levels_per_dn = 254.0 / (hi - lo)
    return {
        "dn_lo": round(lo, 3), "dn_hi": round(hi, 3), "gamma": round(gamma, 4),
        "dither_dn": 0.5 if levels_per_dn > TEX_DITHER_GAIN and bits == 8 else 0.0,
        "levels_per_dn": round(levels_per_dn, 3), "window_widened_to_cap_gain": widened,
        "source_bits_detected": bits, "source_dn_median": med,
        "source_distinct_dn_in_sample": int(np.unique(v).size),
        "percentiles": list(TEX_PCT), "max_gain": TEX_MAX_GAIN,
    }


def _hash01(rows, cols):
    """Deterministic per-pixel noise in [0,1) from global pixel coordinates (so the shared edge
    pixels of neighbouring tiles get identical values)."""
    h = (rows.astype("uint32") * np.uint32(0x9E3779B1)) ^ (cols.astype("uint32") * np.uint32(0x85EBCA77))
    h ^= h >> np.uint32(15)
    h *= np.uint32(0x2C1B3C6D)
    h ^= h >> np.uint32(12)
    h *= np.uint32(0x297A2D39)
    h ^= h >> np.uint32(15)
    return h.astype("float64") / 4294967296.0


def encode_texture(dn, mp, row0=0, col0=0):
    """float DN array (NaN / <=0 = nodata) -> uint8 texture (0 = nodata). row0/col0 = global pixel
    offset of dn[0, 0] in the site grid, for seam-consistent dithering."""
    fin = np.isfinite(dn) & (dn > 0)
    out = np.zeros(dn.shape, "uint8")
    x = dn[fin].astype("float64")
    if mp.get("dither_dn"):
        r, c = np.nonzero(fin)
        x = x + (_hash01(r + row0, c + col0) - 0.5) * 2 * mp["dither_dn"]
    y = np.clip((x - mp["dn_lo"]) / (mp["dn_hi"] - mp["dn_lo"]), 0, 1) ** mp.get("gamma", 1.0)
    out[fin] = np.clip(np.rint(1 + 254 * y), 1, 255).astype("uint8")
    return out


def decode_texture(v8, tex_meta):
    """uint8 texture -> approximate source DN (NaN = nodata). Understands both the v3 'mapping'
    block and the older linear 'stretch_dn' encoding."""
    mp = tex_meta.get("mapping") if tex_meta else None
    v = v8.astype("float64")
    nod = v8 == 0
    if mp:
        y = np.clip((v - 1) / 254, 0, 1) ** (1.0 / mp.get("gamma", 1.0))
        dn = mp["dn_lo"] + y * (mp["dn_hi"] - mp["dn_lo"])
    else:
        lo, hi = tex_meta["stretch_dn"]
        dn = lo + (v - 1) / 254 * (hi - lo)
    dn[nod] = np.nan
    return dn


def warp_to_local(url, dst_crs, dst_transform, shape, resampling, band=1, shift=(0.0, 0.0), yscale=1.0):
    """Read just the needed part of a remote COG (using overviews when coarser) and reproject.
    yscale multiplies the source northings (see ortho_yscale_candidate), shift is then added."""
    with rasterio.open(url) as src:
        src_tf = rasterio.Affine.translation(*shift) * rasterio.Affine.scale(1.0, yscale) * src.transform
        l, b, r, t = transform_bounds(dst_crs, src.crs,
                                      dst_transform.c, dst_transform.f - shape[0] * -dst_transform.e,
                                      dst_transform.c + shape[1] * dst_transform.a, dst_transform.f,
                                      densify_pts=21)
        pad = 20 * src.res[0]
        win = from_bounds(l - pad, b - pad, r + pad, t + pad, src_tf).round_offsets().round_lengths()
        # read at roughly target resolution so remote overviews are used for big orthos
        src_px = src.res[0] * (math.pi * MARS_R / 180 if src.crs.is_geographic else 1)  # metres
        dec = max(1, int(abs(dst_transform.a) / src_px / 1.5))
        out_shape = (max(1, int(win.height / dec)), max(1, int(win.width / dec)))
        data = src.read(band, window=win, out_shape=out_shape, resampling=Resampling.average,
                        boundless=True, fill_value=src.nodata or 0, masked=False).astype("float32")
        wt = window_transform(win, src_tf) * rasterio.Affine.scale(win.width / out_shape[1],
                                                               win.height / out_shape[0])
        nodata = src.nodata
        if nodata is not None:
            data[(data <= nodata * 0.999) if nodata < 0 else (data == nodata)] = np.nan
        dst = np.full(shape, np.nan, dtype="float32")
        reproject(data, dst, src_transform=wt, src_crs=src.crs, src_nodata=np.nan,
                  dst_transform=dst_transform, dst_crs=dst_crs, dst_nodata=np.nan, resampling=resampling)
        return dst


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("obs_id")
    ap.add_argument("--lat", type=float, required=True)
    ap.add_argument("--lon", type=float, required=True)
    ap.add_argument("--size", type=int, default=2016, help="metres across (square)")
    ap.add_argument("--res", type=float, default=1.0, help="metres per pixel")
    ap.add_argument("--name", default=None)
    a = ap.parse_args()

    item = find_item(a.obs_id)
    name = a.name or a.obs_id
    out = os.path.join(os.path.dirname(__file__), "out", name)
    os.makedirs(out, exist_ok=True)

    n = int(round(a.size / a.res)) + 1            # Unreal likes 2017, 4033, 8129 ...
    half = (n - 1) * a.res / 2
    crs = local_crs(a.lat, a.lon)
    tf = from_origin(-half - a.res / 2, half + a.res / 2, a.res, a.res)

    print("reading DTM ...")
    h = warp_to_local(item["assets"]["dtm"], crs, tf, (n, n), Resampling.bilinear)
    if np.isnan(h).mean() > 0.01:
        print(f"warning: {np.isnan(h).mean():.1%} of the crop is outside the DTM")
    h = np.where(np.isnan(h), np.nanmin(h), h)
    np.save(os.path.join(out, "heights_f32.npy"), h)

    zmin, zmax = float(h.min()), float(h.max())
    rng = max(zmax - zmin, 1e-3)
    q = np.round((h - zmin) / rng * 65535).astype("<u2")
    Image.fromarray(q, mode="I;16").save(os.path.join(out, "heightmap_16bit.png"))
    q.tofile(os.path.join(out, "heightmap_16bit.r16"))

    print("reading orthoimage ...")
    ortho_key = next(k for k in item["assets"] if k.startswith("ortho_") and "_RED_C_" in item["assets"][k]
                     and item["assets"][k].endswith(".tif") and "thumbnail" not in k and "label" not in k)
    shift = ortho_offset(item["assets"]["dtm"], item["assets"][ortho_key])
    if math.hypot(*shift) > 5:
        print(f"ortho georeference off by {shift[0]:.0f} m E, {shift[1]:.0f} m N - corrected")
    tex = warp_to_local(item["assets"][ortho_key], crs, tf, (n, n), Resampling.bilinear, shift=shift)
    mapping = make_texture_mapping(tex[np.isfinite(tex) & (tex > 0)][::7])
    Image.fromarray(encode_texture(tex, mapping)).save(os.path.join(out, "texture.png"))

    meta = {
        "source": item["key"], "description": item["description"], "license": "CC0-1.0 (NASA/JPL/UArizona/USGS)",
        "center_lat": a.lat, "center_lon_east": a.lon, "mars_radius_m": MARS_R,
        "projection": crs.to_proj4(), "grid": n, "metres_per_pixel": a.res, "size_m": (n - 1) * a.res,
        "ortho_shift_applied_m": [round(shift[0], 1), round(shift[1], 1)],
        "texture": {"encoding": "uint8 greyscale, 0 = nodata; v = 1 + round(254 * clip((dn - dn_lo) / "
                                "(dn_hi - dn_lo), 0, 1) ** gamma) (+-dither_dn DN noise before mapping)",
                    "inverse": "dn = dn_lo + (dn_hi - dn_lo) * ((v - 1) / 254) ** (1 / gamma)",
                    "mapping": mapping},
        "height_min_m": zmin, "height_max_m": zmax, "height_range_m": rng,
        "unreal": {
            "import": "Landscape > Import from File > heightmap_16bit.png",
            "scale_xy": a.res * 100,                     # cm per vertex
            "scale_z": rng * 100 / 512,                  # Unreal maps 16-bit to +/-256 * Z/100 m
            "location_z_cm": (zmin + rng / 2) * 100,     # puts heights at true Mars elevation
        },
        "unity": {"raw": "heightmap_16bit.r16", "byte_order": "Windows", "resolution": n,
                  "terrain_size_m": [(n - 1) * a.res, rng, (n - 1) * a.res], "position_y_m": zmin},
    }
    json.dump(meta, open(os.path.join(out, "meta.json"), "w"), indent=1)
    print(json.dumps({k: meta[k] for k in ("grid", "size_m", "height_min_m", "height_max_m", "unreal")}, indent=1))


if __name__ == "__main__":
    main()
