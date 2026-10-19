"""Download STAC metadata for every HiRISE DTM into a local catalog.json.

Two bucket layouts exist:
  old: mars/.../dtm/<site>            (STAC item at the prefix root, files in <site>/)
  new: mars/.../dtm/<site>/<site>.json (STAC item inside the folder; ~280 newer DTMs)
The new items' dtm/hillshade hrefs point at the prefix root (404), so every asset href is
checked against the bucket listing and repaired to the copy inside the site folder.
Ortho asset keys are normalised to "ortho_<name>" so batch_convert.pick_ortho finds them.

Usage: python converter/list_bucket.py index.json && python converter/fetch_catalog.py index.json converter/catalog.json
"""
import json, sys, urllib.request
from concurrent.futures import ThreadPoolExecutor

BUCKET = "https://astrogeo-ard.s3.us-west-2.amazonaws.com/"
PREFIX = "mars/mro/hirise/controlled/dtm/"
EXISTS = set()


def fix_href(href, site):
    key = href.split(".amazonaws.com/", 1)[-1]
    if key in EXISTS:
        return href
    moved = f"{PREFIX}{site}/{key.rsplit('/', 1)[-1]}"
    return BUCKET + moved if moved in EXISTS else None


def fetch(key):
    try:
        d = json.load(urllib.request.urlopen(BUCKET + key, timeout=60))
    except Exception:
        return None
    site_key = key[:-len(".json")].rsplit("/", 1)[0] if key.endswith(".json") else key
    site = site_key[len(PREFIX):]
    assets = {}
    for k, v in d["assets"].items():
        href = fix_href(v["href"], site)
        if href is None:
            continue
        if "_ORTHO" in k and not k.startswith("ortho_"):
            k = "ortho_" + k
        assets[k] = href
    if "dtm" not in assets:
        return None
    p = d["properties"]
    return {
        "key": site_key, "id": d["id"], "description": p.get("description", ""),
        "gsd": p.get("gsd"), "bbox": d.get("bbox"), "date": p.get("start_datetime"),
        "assets": assets,
    }


def is_item(key, size):
    rest = key[len(PREFIX):]
    if size <= 0 or rest.endswith("/"):
        return False
    if "/" not in rest:
        return True                                   # old layout
    folder, name = rest.split("/", 1)
    return name == folder + ".json"                   # new layout


if __name__ == "__main__":
    if len(sys.argv) < 3 or sys.argv[1] in ("-h", "--help"):
        sys.exit(__doc__)
    index = json.load(open(sys.argv[1]))
    EXISTS.update(k for k, _ in index)
    keys = [k for k, s in index if is_item(k, s)]
    with ThreadPoolExecutor(32) as ex:
        items = [i for i in ex.map(fetch, keys) if i]
    seen, unique = set(), []
    for i in items:                                   # a site may be listed in both layouts
        if i["id"] not in seen:
            seen.add(i["id"]); unique.append(i)
    json.dump(unique, open(sys.argv[2], "w"), indent=1)
    print(len(unique), "DTMs catalogued")
