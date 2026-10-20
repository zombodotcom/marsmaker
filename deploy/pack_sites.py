"""Pack the sites listed in deploy/sites.txt for GitHub Pages, make card thumbnails and sites.json.

    python deploy/pack_sites.py [--data D:/marsmaker_data] [--out D:/marsmaker_data/pages_build]
                                [--jobs 3] [--repack] [--review]

* runs viewer/tools/web_pack.py (default --max-mpix 16) for every listed site that is not packed yet,
  at below-normal priority, into OUT/sites/<site>/
* writes OUT/thumbs/<site>.jpg (480x360 crop of the texture mosaic, centred on the densest data)
* writes OUT/sites.json, an array of {name, site, title, tag, lat/lon, size, tiles, m/post, rationale} used by
  index.html and by the viewer's in-game site menu
* --review also writes OUT/review/<site>.png: texture next to a hillshade of the packed heights,
  for eyeballing ortho/height alignment (not deployed)
"""
import argparse, json, os, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor
import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
WEB_PACK = os.path.join(REPO, 'viewer', 'tools', 'web_pack.py')


def read_list():
    out = []
    for line in open(os.path.join(HERE, 'sites.txt'), encoding='utf-8'):
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        f = [x.strip() for x in line.split('|')]
        out.append((f[0], f[1], f[2], f[3] if len(f) > 3 else ''))
    return out


def pack(src, out_root, repack):
    name = os.path.basename(src)
    dst = os.path.join(out_root, 'sites', name)
    if not repack and os.path.exists(os.path.join(dst, 'meta.json')):
        return name, 'already packed'
    flags = getattr(subprocess, 'BELOW_NORMAL_PRIORITY_CLASS', 0)
    r = subprocess.run([sys.executable, WEB_PACK, src, '--out', out_root], capture_output=True, text=True,
                       creationflags=flags)
    if r.returncode:
        raise RuntimeError(f'{name}: web_pack failed\n{r.stderr[-2000:]}')
    return name, r.stdout.strip().splitlines()[-1]


def mosaic(site, kind, scale):
    """Downsampled mosaic of one tile kind; returns float array (0 = nodata) and meta."""
    meta = json.load(open(os.path.join(site, 'meta.json')))
    sp = meta['tile_px'] - 1
    gx, gy = meta['grid_tiles']
    s = max(1, int(round(sp * scale)))
    img = np.zeros((gy * s, gx * s), np.float32)
    for t in meta['tiles']:
        fn = t.get(kind)
        if not fn or not os.path.exists(os.path.join(site, fn)):
            continue
        im = Image.open(os.path.join(site, fn))
        a = np.asarray(im.crop((0, 0, sp, sp)).resize((s, s), Image.BOX if kind != 'height' else Image.NEAREST),
                       np.float32)
        img[t['y'] * s:(t['y'] + 1) * s, t['x'] * s:(t['x'] + 1) * s] = a
    return img, meta


def best_crop(tex, ch, cw, detail=True):
    """Top-left of the ch x cw window to show: among windows that are >= 97% valid (or the most valid
    ones), the one with the most texture detail (sum of |gradient|), so cards show dunes, not flats."""
    H, W = tex.shape
    if H < ch or W < cw:
        return 0, 0

    def win_sum(a):
        c = np.pad(np.cumsum(np.cumsum(a.astype(np.float64), 0), 1), ((1, 0), (1, 0)))
        return c[ch:, cw:] - c[:-ch, cw:] - c[ch:, :-cw] + c[:-ch, :-cw]

    valid = tex > 0
    nv = win_sum(valid) / (ch * cw)
    gy, gx = np.gradient(tex)
    g = np.where(valid, np.minimum(np.hypot(gx, gy), 40), 0)   # clip so nodata edges don't win
    g[:, :2] = g[:, -2:] = 0
    g[:2] = g[-2:] = 0
    ok = nv >= min(0.97, nv.max() - 1e-9)
    s = np.where(ok, win_sum(g) if detail else nv, -1)
    i, j = np.unravel_index(np.argmax(s), s.shape)
    return int(i), int(j)


def thumbnail(site, out_jpg, review_png=None, detail=True):
    meta = json.load(open(os.path.join(site, 'meta.json')))
    sp = meta['tile_px'] - 1
    mpp = meta['metres_per_pixel']
    # aim for ~4 m per thumbnail pixel so dunes are visible, but at least 360 px of data height
    scale = min(1.0, mpp / 4.0)
    tex, _ = mosaic(site, 'texture', scale)
    while (tex.shape[0] < 360 or tex.shape[1] < 480) and scale < 1.0:
        scale = min(1.0, scale * 1.5)
        tex, _ = mosaic(site, 'texture', scale)
    ch, cw = min(360, tex.shape[0]), min(480, tex.shape[1])
    i, j = best_crop(tex, ch, cw, detail)
    crop = tex[i:i + ch, j:j + cw]
    v = crop[crop > 0]
    lo, hi = (np.percentile(v, [1, 99]) if v.size else (0, 255))
    g = np.clip((crop - lo) / max(hi - lo, 1), 0, 1)
    # Mars tint: butterscotch ramp
    rgb = np.stack([60 + 195 * g, 30 + 150 * g ** 1.1, 15 + 105 * g ** 1.3], -1)
    rgb[crop == 0] = (20, 12, 10)
    im = Image.fromarray(rgb.astype(np.uint8)).resize((480, 360), Image.LANCZOS) if (ch, cw) != (360, 480) \
        else Image.fromarray(rgb.astype(np.uint8))
    im.save(out_jpg, quality=82, optimize=True, progressive=True)
    if review_png:
        hq, _ = mosaic(site, 'height', scale)
        hc = hq[i:i + ch, j:j + cw].astype(np.float64)
        h = meta['height_min_m'] + (hc - 1) * meta['height_range_m'] / 65534
        m = hc > 0
        h[~m] = np.mean(h[m]) if m.any() else 0
        px = mpp / scale
        gy_, gx_ = np.gradient(h, px)
        hs = np.clip(0.5 + 0.7 * (gx_ * 0.7 - gy_ * 0.7), 0, 1)
        hs[~m] = 0
        both = np.concatenate([g, np.ones((ch, 6)), hs], 1)
        Image.fromarray((both * 255).astype(np.uint8)).save(review_png)


def rationale(desc):
    m = re.search(r"rationale for creation was: (.*?)\. This DTM", desc or '')
    return m.group(1) if m else ''


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--data', default=r'D:\marsmaker_data')
    ap.add_argument('--out', default=r'D:\marsmaker_data\pages_build')
    ap.add_argument('--jobs', type=int, default=3)
    ap.add_argument('--repack', action='store_true')
    ap.add_argument('--review', action='store_true')
    a = ap.parse_args()
    sites = read_list()
    os.makedirs(os.path.join(a.out, 'sites'), exist_ok=True)
    with ThreadPoolExecutor(a.jobs) as ex:
        for name, msg in ex.map(lambda s: pack(os.path.join(a.data, 'sites', s[0]), a.out, a.repack), sites):
            print(f'{name}: {msg}', flush=True)
    os.makedirs(os.path.join(a.out, 'thumbs'), exist_ok=True)
    if a.review:
        os.makedirs(os.path.join(a.out, 'review'), exist_ok=True)
    listed, entries = {s[0] for s in sites}, []
    for name, title, tag, opt in sites:
        d = os.path.join(a.out, 'sites', name)
        meta = json.load(open(os.path.join(d, 'meta.json')))
        thumbnail(d, os.path.join(a.out, 'thumbs', name + '.jpg'),
                  os.path.join(a.out, 'review', name + '.png') if a.review else None, opt != 'thumb=valid')
        files = os.listdir(d)
        size = sum(os.path.getsize(os.path.join(d, f)) for f in files)
        biggest = max(os.path.getsize(os.path.join(d, f)) for f in files)
        assert biggest < 95e6, f'{name}: a file is {biggest / 1e6:.0f} MB'
        entries.append({
            'site': name, 'title': title, 'tag': tag,
            'lat': round(meta['center_lat'], 3), 'lon_east': round(meta['center_lon_east'], 3),
            'size_mb': round(size / 1e6, 1), 'tiles': len(meta['tiles']),
            'metres_per_post': meta['metres_per_pixel'],
            'area_km2': round(sum(t.get('valid_fraction', 1) for t in meta['tiles']) * meta['tile_size_m'] ** 2 / 1e6, 1),
            'relief_m': round(meta['height_max_m'] - meta['height_min_m']),
            'albedo': any('albedo' in t for t in meta['tiles']),
            'rationale': rationale(meta.get('description')),
            'dtm_id': meta.get('id'),
        })
        print(f'{name}: {size / 1e6:.1f} MB, thumbnail done', flush=True)
    stale = [n for n in os.listdir(os.path.join(a.out, 'sites')) if n not in listed]
    if stale:
        print('not in sites.txt (left on disk, not deployed):', ', '.join(stale))
    # A JSON array of objects: the viewer's in-game map menu (src/sites.rs) reads each 'name' and fetches
    # sites/<name>/meta.json; index.html reads the card fields. 'site' is the same as 'name'.
    for e in entries:
        e['name'] = e['site']
    json.dump(entries, open(os.path.join(a.out, 'sites.json'), 'w'), indent=1)
    total = sum(e['size_mb'] for e in entries)
    print(f'{len(entries)} sites, {total:.0f} MB of site data -> {os.path.join(a.out, "sites.json")}')


if __name__ == '__main__':
    main()
