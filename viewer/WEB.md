# Browser build

The viewer runs in the browser as `wasm32-unknown-unknown` on WebGL2. In the browser, site files
are fetched over HTTP from `sites/<site>/` relative to the page, and flags come from the URL
query string.

The web root used below is `D:\marsmaker_data\web\`:

```
D:\marsmaker_data\web\
  index.html          copied from viewer/web/index.html
  pkg\                wasm-bindgen output (marsmaker-viewer.js + marsmaker-viewer_bg.wasm)
  sites\<site>\       downsampled sites from tools/web_pack.py
  live.html           site picker for live mode (copied from viewer/web/)
  live_index.json     live-mode site index (viewer/web/, from tools/make_live_index.py)
```

## One-time setup

```
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
```

The CLI version must equal the `wasm-bindgen` crate version in `Cargo.lock`, currently 0.2.129.
Check it with `grep -A1 'name = "wasm-bindgen"' Cargo.lock`.

## Build

From `viewer/`:

```
cargo build --profile web --target wasm32-unknown-unknown -j 4
wasm-bindgen --target web --no-typescript --remove-name-section --remove-producers-section ^
    --out-dir D:\marsmaker_data\web\pkg target\wasm32-unknown-unknown\web\marsmaker-viewer.wasm
copy web\index.html D:\marsmaker_data\web\index.html
copy web\live.html D:\marsmaker_data\web\live.html
copy web\live_index.json D:\marsmaker_data\web\live_index.json
```

`--profile web` is the release profile with `opt-level = "s"`, thin LTO and one codegen unit,
for a smaller download. `--release` also works, at `target\wasm32-unknown-unknown\release\`,
but the file is much larger. A clean wasm build takes 10-15+ minutes with `-j 4`.

## Pack sites for the browser

Browser memory is limited, so sites are downsampled:

```
python tools\web_pack.py D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255 ^
                         D:\marsmaker_data\sites\PSP_003800_1325_PSP_004077_1325
```

The output goes to `D:\marsmaker_data\web\sites\<site>\`, or to `--out DIR`. The tool picks the
smallest factor (2, 3, 4, ...) that brings a site under `--max-mpix` million height posts
(default 16). You can force a factor with `--factor N`.

| Site | Factor | Resolution | Size |
| --- | --- | --- | --- |
| Russell | 2 | 2 m/post, 12 tiles of 1009 px | 12.8 MB |
| Proctor | 4 | 4 m/post, 38 tiles of 505 px | 15.8 MB |

Heights, ortho and albedo are averaged with a nodata-aware box filter. The format is unchanged,
so the native viewer also opens packed sites.

## Serve and open

```
cd /d D:\marsmaker_data\web
python -m http.server 8765 --bind 127.0.0.1
```

Then open:

- `http://127.0.0.1:8765/` loads the default site (Russell).
- `http://127.0.0.1:8765/?site=PSP_003800_1325_PSP_004077_1325&albedo&mode=real`
- `http://127.0.0.1:8765/?autodrive&weave=0.2&throttle=0.7`

Every native flag works as a query key without the `--`. Flags that take a value use `key=value`;
switches are bare (`albedo`, `realistic`, `autodrive`, `high-cam`). Examples: `mode=real`
(or `mode=fun`, the default), `gravity-mult=0.5`, `spawn=200,-4444`,
`heading=90`, `step=2`, `log-every=1`. `site=` is a folder name under `sites/`, or any URL
containing `/`. Click the canvas first so it gets keyboard focus. The physics log appears in the
browser console. `shot`/`seconds` are meant for native runs.

## Headless smoke test

```
node tools\web_smoke.mjs "http://127.0.0.1:8765/?autodrive&albedo" out.png 90
```

This needs Node 22 or newer and Chrome. It starts headless Chrome with a throwaway profile,
prints every console message and exception with a timestamp, saves a screenshot after N seconds,
and exits 1 if anything logged an error. Headless WebGL runs on SwiftShader (CPU), so expect
about 1 fps there. That's a property of the test environment, not of the build; a real GPU
browser runs at normal speed.

## Live mode: any of the 1,262 HiRISE DTMs, streamed from NASA

`?live=<site id>` loads a DTM and its orthoimage straight from the public astrogeo-ard bucket
(NASA/USGS, `https://astrogeo-ard.s3.us-west-2.amazonaws.com/`). The bucket answers
`Access-Control-Allow-Origin: *` and allows the `Range` header, so the page reads the
Cloud-Optimized GeoTIFFs with HTTP Range requests. There is no server of ours in the path.

- `http://127.0.0.1:8765/?live=PSP_007018_1255_PSP_007229_1255` streams Russell Crater.
- `http://127.0.0.1:8765/live.html` is the site picker: a searchable list plus a Mars map with
  one dot per DTM. Click a dot or row to select a site, then "Drive here" (double-click jumps
  straight in). `live.html?q=dunes` pre-fills the search.
- Natively: `marsmaker-viewer --live <site id>`. The index defaults to `viewer/web/live_index.json`,
  or pass `--live-index FILE`.

All the usual flags still work (`&autodrive`, `&high-cam`, `&realistic`, ...). `live-mpp=M` sets
the finest post spacing to stream (default about 2 m). While loading, the HUD shows MB fetched
out of the total for the DTM, then for the ortho.

### What it does

1. It reads `live_index.json` (next to the page) and looks up the site.
2. It fetches the first 64 KB of the DTM COG and parses the TIFF IFDs (`src/live.rs`). That
   covers the overview sizes, tile offsets and byte counts, compression, predictor, nodata and
   georeference. Anything past 64 KB is fetched on demand; 41 RED_A orthos need that.
3. It picks an overview. That is the finest one whose post spacing in true metres is at least
   2 m. It moves to the next one if the data would go over 16 M posts, or if the level is over
   64 MB and the next one is still 6 m or finer. Then it fetches every non-empty tile of that
   level: tiles are coalesced into requests of up to 3 MB, about 6 run in parallel, and each is
   inflated with miniz_oxide and un-predicted.
4. It resamples onto a north-up grid in true metres. The origin is the site centre, and the
   tiles are 505 posts with a 1-post overlap. Heights are bilinear, nodata-aware, and NaN where
   there is no data.
5. It fetches only the ortho tiles that overlap. The ortho level is the coarsest one no coarser
   than 1.3x the grid spacing. Each ortho is placed through its own georeference, with the
   northing fix from the index applied. The DN go through the converter's tone mapping (0.05 /
   99.95 percentiles, gamma toward 45 % grey at the median, the same dither hash), so the
   textures look like the converted sites.
6. The rest is the normal loader: spawn picker, decimated meshes, the full-res window and the
   physics.

What the COGs actually use, from a 64 KB header sample of all 1,262 DTMs and their chosen orthos
(`make_live_index.py` writes it to `live_cache/layout_summary.json`):

| | DTM | Ortho |
| --- | --- | --- |
| Format | BigTIFF, tiled 512x512, 1 band | same |
| Compression | DEFLATE + predictor 2, all 1,262 | DEFLATE + predictor 2, all 1,261 |
| Samples | float32, nodata -3.4028227e38 | uint16, 0 = nodata |
| Overviews | 2-7 (mostly 4-6) | 2-8 |
| IFDs in first 64 KB | 1,262 | 1,220 (41 need a 2nd read) |
| Projection | 1,106 equirectangular (lat_ts = 0), 87 north polar stereographic, 69 south polar stereographic | same as its DTM |

Predictor 2 on float32 is integer differencing of the 32-bit words, which is what libtiff does.
LZW, uncompressed data, predictor 3 and 8/16/32/64-bit int and float samples are also handled,
even though no file uses them.

### Geometry caveats

- There is no reprojection. The grid is the file's own map grid scaled to true metres at the site
  centre, using `k` from the index.
  - Equirectangular files have standard parallel 0, so x is scaled by cos(lat). Compared with
    the converter's transverse Mercator, distances are off by about 0.1-0.3 % at the far ends of
    a 10+ km strip, because cos(lat) changes along it. That is a few metres.
  - Polar stereographic is conformal, and its scale changes by less than 0.2 % across a site.
    "Up" is grid north rather than true north, so the image sun is rotated by `conv` (the grid
    bearing of true north) to match.
- About 80 orthos have northings computed on the label's local radius but are tagged with the
  3396.19 km IAU sphere (see `converter/hirise2engine.ortho_yscale_candidate`). The index bakes in
  the fix: the converter's own decision where a site was converted, else the label radius plus
  the converter's edge-fit test. The converter's correlation-based residual shift is used only
  for converted sites.

### Measured

These are headless Chrome runs (`tools/web_smoke.mjs`, local page, bucket over the internet).
Download time is from the first byte requested to the last byte decoded. Load time also includes
resampling, spawn and meshes.

| Site | Grid | From NASA | Download | Loaded |
| --- | --- | --- | --- | --- |
| Russell, PSP_007018_1255_PSP_007229_1255 (54 S, equirect.) | 2.18 m, 33 tiles | 26.8 MB | 6.0 s | 7.4 s |
| Gale, ESP_018643_1745_ESP_019065_1745 (5 S, not converted) | 4.58 m, 55 tiles | 36.3 MB | 7.2 s | 10.6 s |
| NPLD trough, ESP_019210_2665_ESP_019158_2665 (86.5 N, polar st.) | 2.45 m, 54 tiles | 44.0 MB | 9.0 s | 12.6 s |

A native sample of 20 random sites loaded with no failures. The median was 36 MB in 5.7 s and
the maximum 72 MB. Post spacing ranged from 2.0 to 4.1 m.

Correctness: `tools/compare_live.py` compares the live Russell grid with the converted 1 m site
(`D:\marsmaker_data\sites`) at every live post, 4.9 M common posts:

- Mean difference -0.011 m, median |d| 0.06 m, p95 0.42 m, p99 0.79 m. The extremes come from
  overview averaging on steep slopes.
- Min/max 57.70/646.92 m live vs 57.56/646.96 m converted.
- The ortho correlates at 0.997 with the converted texture, and the best shift is 0 posts.

For sites that were never converted, the ortho correlates best with a hillshade lit from the
image's own sun (+0.26 to +0.47) within 1-2 posts of zero shift. For the polar site, using the
true-north azimuth without rotating it drops that correlation to 0.09. This confirms the
rotation.

```
cargo run -- --live PSP_007018_1255_PSP_007229_1255 --live-dump D:\marsmaker_data\live_test\russell
python tools\compare_live.py D:\marsmaker_data\live_test\russell D:\marsmaker_data\sites\PSP_007018_1255_PSP_007229_1255
```

`--live-dump OUT` streams a site natively and writes `OUT.json` (grid, bytes, seconds),
`OUT.f32` (absolute heights) and `OUT.tex` (u8 texture mosaic), then exits without opening a
window.

### The index

```
python tools\make_live_index.py        # about 20 min the first time with 8 workers; cached afterwards
```

It writes `web/live_index.json`, about 355 KB (112 KB gzipped) for 1,262 sites: one array per
site, in the order of its `fields` list. Each entry has:

- id, title, centre lat/lon
- the DTM and ortho file names
- DTM resolution, size and overview count
- projection, centre map coordinates, `k`, `conv`
- the image sun (azimuth/elevation)
- the ortho northing scale and shift
- the full DTM size in MB

The sun comes from the HiRISE RDR labels on hirise-pds.lpl.arizona.edu. That host only allows
CORS from pds-imaging.jpl.nasa.gov, so the browser can't read the labels and they are
precomputed here. 1,262 of 1,262 sites have one. Responses are cached in
`D:\marsmaker_data\live_cache\`. The ortho choice follows the converter's `pick_ortho` (RED_C,
then D, B, A).

### Limits

- Each load downloads 20-70 MB. Nothing is cached between page loads beyond the browser's own
  HTTP cache, which may keep the ranges.
- Resolution is about 2-4.6 m per post. That is the overview the 16 M-post budget allows,
  coarser than the 1 m converted sites. On native, `--live-mpp 1` asks for more if the budget
  allows.
- No albedo (`a_`) textures in live mode. The de-lighting needs the converter.
- Loading and decoding run on the main thread in the browser. The page keeps drawing between
  requests, but freezes for a second or two while it resamples.
- If NASA moves or renames files, re-run `converter/list_bucket.py`, `converter/fetch_catalog.py`
  and `tools/make_live_index.py`.

### Shipping it

Next to the viewer page, copy these:

```
live.html          from viewer/web/live.html
live_index.json    from viewer/web/live_index.json
pkg/               the wasm-bindgen output of this build (same files as before)
```

If the viewer page isn't called `index.html` (for example `play.html`), set
`<meta name="play-page" content="play.html">` in live.html. The "All 1,262 sites" link in the
viewer page passes its own name to the picker as `live.html?play=<name>` anyway. `sites/` is
only needed for the `?site=` mode.

## Notes and limits

- In the browser, loading and the full-res chunk rebuilds run on the main thread, because
  wasm has no threads here. The page freezes briefly while a site decodes (about 2 s on a
  desktop for packed Russell) and may hitch slightly at chunk crossings.
- The far-terrain vertex budget is 4 M in the browser (16 M native), so the far mesh is
  decimated more and the full-res window around the buggy matters more.
- Audio is not used; Chrome's "AudioContext was not allowed to start" warning is harmless.
- Bevy logs WebGL2 capability warnings at startup (no compute, OIT, SSAO). They are expected.
