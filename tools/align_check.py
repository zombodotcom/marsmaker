"""Residual texture/terrain misalignment on the most-valid tile of a site: cross-correlate a
synthetic hillshade lit from several directions with the texture; report best shift in pixels."""
import json, os, sys
import numpy as np
from PIL import Image
from scipy import signal

def residual(site, maxs=60):
    meta = json.load(open(os.path.join(site, "meta.json")))
    t = max(meta["tiles"], key=lambda t: t["valid_fraction"])
    q = np.array(Image.open(os.path.join(site, t["height"]))).astype("f8")
    tx = np.array(Image.open(os.path.join(site, t["texture"]))).astype("f8")
    m = (q > 0) & (tx > 0)
    h = meta["height_min_m"] + (q - 1) * meta["height_range_m"] / 65534
    h[~m] = np.nanmean(np.where(m, h, np.nan))
    gy, gx = np.gradient(h)
    best = None
    for az in np.radians([0, 45, 90, 135, 180, 225, 270, 315]):  # sun direction unknown: try all
        hs = -(gx * np.sin(az) - gy * np.cos(az))
        a = np.where(m, hs - hs[m].mean(), 0); a /= a[m].std() + 1e-9
        b = np.where(m, tx - tx[m].mean(), 0); b /= b[m].std() + 1e-9
        c = 1024
        r0, c0 = [max(0, (s - c) // 2) for s in a.shape]
        A, B = a[r0:r0 + c, c0:c0 + c], b[r0:r0 + c, c0:c0 + c]
        cc = signal.fftconvolve(B, A[::-1, ::-1], mode="same") / m[r0:r0 + c, c0:c0 + c].sum()
        k = cc.shape[0] // 2
        win = cc[k - maxs:k + maxs + 1, k - maxs:k + maxs + 1]
        i, j = np.unravel_index(np.argmax(win), win.shape)
        if best is None or win[i, j] > best[0]:
            best = (win[i, j], i - maxs, j - maxs, int(np.degrees(az)), win[maxs, maxs])
    return meta["id"], meta["texture"]["ortho_shift_applied_m"], best

for s in sys.argv[1:]:
    sid, sh, (peak, dr, dc, az, at0) = residual(s)
    print(f"{os.path.basename(s)} applied_shift={sh} residual_px(row,col)=({dr},{dc}) peak={peak:.3f} at0={at0:.3f} sun_az={az}")
