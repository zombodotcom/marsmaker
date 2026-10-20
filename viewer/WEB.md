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

## Notes and limits

- In the browser, loading and the full-res chunk rebuilds run on the main thread, because
  wasm has no threads here. The page freezes briefly while a site decodes (about 2 s on a
  desktop for packed Russell) and may hitch slightly at chunk crossings.
- The far-terrain vertex budget is 4 M in the browser (16 M native), so the far mesh is
  decimated more and the full-res window around the buggy matters more.
- Audio is not used; Chrome's "AudioContext was not allowed to start" warning is harmless.
- Bevy logs WebGL2 capability warnings at startup (no compute, OIT, SSAO). They are expected.
