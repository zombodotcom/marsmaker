"""Make a web-sized copy of a MarsMaker site for the browser build.

    python tools/web_pack.py SITE_DIR [SITE_DIR ...] [--out D:/marsmaker_data/web]
                             [--factor N] [--max-mpix 16] [--no-albedo]

For each site, writes OUT/sites/<site name>/ with the same layout and encodings the viewer reads
(meta.json, h_/t_/a_ tiles), at 1/N of the resolution:

* heights, ortho and albedo are downsampled with a nodata-aware box filter (window 2*(N//2)+1
  centred on each kept post, so posts stay exactly on the original grid lines), averaging the
  linear uint16/uint8 codes of valid samples only; a post is nodata if the original post was.
* tiles keep their 2016 m footprint (tile_px becomes 2016/N + 1, still with a 1 px overlap), the
  tile grid is unchanged, and metres_per_pixel is multiplied by N.
* work is done one tile row at a time (plus a filter halo), so memory stays bounded.

--factor picks N explicitly (must divide 2016). By default N is the smallest of 2, 3, 4, 6, 8
that brings the total post count under --max-mpix million posts (default 16).
"""
import argparse, json, os, shutil, sys
import numpy as np
from PIL import Image

FACTORS = [2, 3, 4, 6, 8, 12, 16]


def box_avg(codes, f):
    """Nodata-aware box mean (odd window 2*(f//2)+1) over a 2-D array of codes (0 = nodata)."""
    k = 2 * (f // 2) + 1
    v = codes.astype(np.float64)
    m = (codes > 0).astype(np.float64)

    def box(a):
        p = k // 2
        a = np.pad(a, p)
        c = np.cumsum(np.cumsum(a, 0), 1)
        c = np.pad(c, ((1, 0), (1, 0)))
        return c[k:, k:] - c[:-k, k:] - c[k:, :-k] + c[:-k, :-k]

    s, n = box(v * m), box(m)
    out = np.where(n > 0, s / np.maximum(n, 1), 0)
    return np.where(m > 0, np.clip(np.round(out), 1, None), 0)


def pack(site, out_root, factor, max_mpix, albedo):
    meta = json.load(open(os.path.join(site, 'meta.json')))
    tp = meta['tile_px']
    sp = tp - 1
    W, H = meta['grid_px']
    if factor is None:
        factor = next((f for f in FACTORS if sp % f == 0 and len(meta['tiles']) * (sp // f + 1) ** 2 <= max_mpix * 1e6), FACTORS[-1])
    f = factor
    assert sp % f == 0, f'factor {f} must divide the tile step {sp}'
    nsp = sp // f
    name = os.path.basename(os.path.normpath(site))
    out = os.path.join(out_root, 'sites', name)
    os.makedirs(out, exist_ok=True)
    halo = f  # >= filter radius
    by_row = {}
    for t in meta['tiles']:
        by_row.setdefault(t['y'], []).append(t)
    kinds = [('height', np.uint16), ('texture', np.uint8)] + ([('albedo', np.uint8)] if albedo else [])
    new_tiles = []
    for row in sorted(by_row):
        r0 = row * sp
        b0, b1 = max(r0 - halo, 0), min(r0 + sp + 1 + halo, H)
        for kind, dtype in kinds:
            files = {t['x']: t.get(kind) for t in by_row[row] if t.get(kind)}
            if not files:
                continue
            band = np.zeros((b1 - b0, W), dtype=np.uint32)
            # neighbouring tile rows provide the filter halo
            for nrow in (row - 1, row, row + 1):
                for t in by_row.get(nrow, []):
                    fn = t.get(kind)
                    if not fn:
                        continue
                    a = np.array(Image.open(os.path.join(site, fn)))
                    tr0, tc0 = nrow * sp, t['x'] * sp
                    rr0, rr1 = max(tr0, b0), min(tr0 + a.shape[0], b1)
                    if rr0 >= rr1:
                        continue
                    cc1 = min(tc0 + a.shape[1], W)
                    src = a[rr0 - tr0:rr1 - tr0, :cc1 - tc0]
                    dst = band[rr0 - b0:rr1 - b0, tc0:cc1]
                    np.copyto(dst, src, where=src > 0)
            avg = box_avg(band, f)
            for t in by_row[row]:
                fn = files.get(t['x'])
                if not fn:
                    continue
                c0 = t['x'] * sp
                tile = avg[r0 - b0:r0 - b0 + sp + 1:f, c0:c0 + sp + 1:f]
                full = np.zeros((nsp + 1, nsp + 1), dtype=dtype)
                full[:tile.shape[0], :tile.shape[1]] = tile.astype(dtype)
                Image.fromarray(full).save(os.path.join(out, fn), optimize=False, compress_level=6)
                if kind == 'height':
                    nt = dict(t)
                    nt['valid_fraction'] = round(float((full > 0).mean()), 4)
                    for k in ('unreal_location_cm', 'unity_position_m'):
                        nt.pop(k, None)
                    if not albedo:
                        nt.pop('albedo', None)
                    new_tiles.append(nt)
            print(f'  {name}: row {row} {kind} done', flush=True)
    nm = {k: v for k, v in meta.items() if k not in ('unreal', 'unity')}
    nm.update({
        'metres_per_pixel': meta['metres_per_pixel'] * f,
        'tile_px': nsp + 1,
        'grid_px': [(W - 1) // f + 1, (H - 1) // f + 1],
        'tiles': sorted(new_tiles, key=lambda t: (t['y'], t['x'])),
        'web_pack': {'factor': f, 'source_metres_per_pixel': meta['metres_per_pixel'], 'source_tile_px': tp,
                     'filter': f'nodata-aware box {2 * (f // 2) + 1}x{2 * (f // 2) + 1} on linear codes'},
    })
    if not albedo:
        nm.pop('albedo', None)
    json.dump(nm, open(os.path.join(out, 'meta.json'), 'w'), indent=1)
    if os.path.exists(os.path.join(site, 'preview.png')):
        shutil.copy(os.path.join(site, 'preview.png'), out)
    size = sum(os.path.getsize(os.path.join(out, x)) for x in os.listdir(out))
    posts = len(new_tiles) * (nsp + 1) ** 2
    print(f'{name}: factor {f} -> {nm["metres_per_pixel"]} m/post, {len(new_tiles)} tiles of {nsp + 1} px, '
          f'{posts / 1e6:.1f} M posts, {size / 1e6:.1f} MB in {out}')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('sites', nargs='+')
    ap.add_argument('--out', default=r'D:\marsmaker_data\web')
    ap.add_argument('--factor', type=int)
    ap.add_argument('--max-mpix', type=float, default=16)
    ap.add_argument('--no-albedo', action='store_true')
    a = ap.parse_args()
    for s in a.sites:
        pack(s, a.out, a.factor, a.max_mpix, not a.no_albedo)


if __name__ == '__main__':
    sys.exit(main())
