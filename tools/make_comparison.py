"""Build the 'same 4 km of Mars at each satellite's resolution' layers for the web demo."""
import json, os, sys
import numpy as np
from PIL import Image
from rasterio.enums import Resampling
from rasterio.transform import from_origin
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from hirise2engine import local_crs, warp_to_local

site = sys.argv[1]                      # out/<site>
web = sys.argv[2]                       # output dir for the web demo
os.makedirs(web, exist_ok=True)
meta = json.load(open(os.path.join(site, "meta.json")))
h = np.load(os.path.join(site, "heights_f32.npy"))
n, res = meta["grid"], meta["metres_per_pixel"]
crs = local_crs(meta["center_lat"], meta["center_lon_east"])
half = (n - 1) * res / 2

def real(url, step):
    m = int(round((n - 1) * res / step)) + 1
    tf = from_origin(-half - step / 2, half + step / 2, step, step)
    return warp_to_local("/vsicurl/" + url, crs, tf, (m, m), Resampling.bilinear)

def block(a, f):
    m = (a.shape[0] - 1) // f + 1
    return np.array([[a[i*f:(i+1)*f, j*f:(j+1)*f].mean() for j in range(m)] for i in range(m)], "float32")

layers = {
  "mola":  real("https://planetarymaps.usgs.gov/mosaic/Mars_MGS_MOLA_DEM_mosaic_global_463m.tif", 463.0),
  "hrsc":  real("https://planetarymaps.usgs.gov/mosaic/Mars/HRSC_MOLA_Blend/Mars_HRSC_MOLA_BlendDEM_Global_200mp_v2.tif", 200.0),
  "ctx":   block(h, 20),
}
WEB_N = 1009                              # 4 m grid for the browser
step = (n - 1) // (WEB_N - 1)
layers["hirise"] = h[::step, ::step][:WEB_N, :WEB_N]

info = {}
for k, a in layers.items():
    a = np.nan_to_num(a, nan=float(np.nanmean(a)))
    info[k] = {"n": a.shape[0], "min": float(a.min()), "max": float(a.max()), "relief_m": float(a.max() - a.min())}
    # lossless 16-bit heights packed into an RGB PNG (red = high byte, green = low byte) for browsers
    q = np.round((a - a.min()) / max(float(a.max() - a.min()), 1e-3) * 65535).astype(np.uint16)
    rgb = np.zeros(a.shape + (3,), np.uint8); rgb[..., 0] = q >> 8; rgb[..., 1] = q & 255
    Image.fromarray(rgb, "RGB").save(os.path.join(web, f"{k}_h16.png"), optimize=True)
    print(k, info[k])

tex = Image.open(os.path.join(site, "texture.png")).resize((2048, 2048), Image.LANCZOS)
tex.save(os.path.join(web, "texture.jpg"), quality=85)
json.dump({"size_m": (n - 1) * res, "center_lat": meta["center_lat"], "center_lon": meta["center_lon_east"],
           "unreal": meta["unreal"], "ortho_shift_applied_m": meta["ortho_shift_applied_m"],
           "layers": info}, open(os.path.join(web, "layers.json"), "w"), indent=1)
