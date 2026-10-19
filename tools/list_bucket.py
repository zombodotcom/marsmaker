"""List every object under the USGS HiRISE DTM prefix (public, no AWS account)."""
import json, sys, urllib.parse, urllib.request, xml.etree.ElementTree as ET

BUCKET = "https://astrogeo-ard.s3.us-west-2.amazonaws.com/"
PREFIX = "mars/mro/hirise/controlled/dtm/"
NS = {"s": "http://s3.amazonaws.com/doc/2006-03-01/"}

def list_all(prefix=PREFIX):
    token, out = None, []
    while True:
        q = {"list-type": "2", "prefix": prefix, "max-keys": "1000"}
        if token: q["continuation-token"] = token
        root = ET.fromstring(urllib.request.urlopen(BUCKET + "?" + urllib.parse.urlencode(q)).read())
        for c in root.findall("s:Contents", NS):
            out.append((c.find("s:Key", NS).text, int(c.find("s:Size", NS).text)))
        t = root.find("s:NextContinuationToken", NS)
        if t is None: return out
        token = t.text

if __name__ == "__main__":
    objs = list_all()
    json.dump(objs, open(sys.argv[1], "w"))
    print(len(objs), "objects", round(sum(s for _, s in objs) / 1e12, 3), "TB")
