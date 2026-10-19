"""Live tile-matrix monitor for the batch converter.

    python tools/monitor.py            # serves http://127.0.0.1:8765 and opens it

Reads D:\\marsmaker_data\\manifest.json (written by batch_convert.py) and catalog.json.
Read-only: it never touches the pipeline's files.
"""
import json, os, re, sys, threading, time, webbrowser
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from urllib.parse import unquote
import psutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.environ.get("MARS_DATA", r"D:\marsmaker_data")
PORT = 8765


def dir_bytes(path):
    total = 0
    for dp, _, files in os.walk(path):
        for f in files:
            try: total += os.path.getsize(os.path.join(dp, f))
            except OSError: pass
    return total


def site_name(key):
    return key.rstrip("/").rsplit("/", 1)[-1]


CATALOG = []
for item in json.load(open(os.path.join(ROOT, "catalog.json"))):
    w, s, e, n = item["bbox"]
    r = re.search(r"rationale for creation was: (.*?)\. This DTM", item["description"])
    CATALOG.append({"site": site_name(item["key"]), "lat": (s + n) / 2, "lon": (w + e) / 2,
                    "what": r.group(1) if r else "", "gsd": item.get("gsd")})

# disk sizes are slow to walk; refresh them in the background
SIZES = {"cache": 0, "out": 0, "t": time.time(), "rate": 0.0}
def size_loop():
    prev = None
    while True:
        c, o = dir_bytes(os.path.join(DATA, "cache")), dir_bytes(os.path.join(DATA, "sites"))
        now = time.time()
        if prev:
            SIZES["rate"] = max(0.0, ((c + o) - prev[0]) / (now - prev[1]))
        prev = ((c + o), now)
        SIZES.update(cache=c, out=o, t=now)
        time.sleep(3)
threading.Thread(target=size_loop, daemon=True).start()
psutil.cpu_percent()
NET = {"last": psutil.net_io_counters().bytes_recv, "t": time.time(), "rate": 0.0}


# ---- live activity: tail Claude session transcripts (this machine only, read-only) ----
HOME_PROJECTS = os.path.join(os.path.expanduser("~"), ".claude", "projects")
TASK_GLOB = os.path.join(os.environ.get("TEMP", ""), "claude", "*", "*", "tasks", "*.output")
EVENTS, EV_LOCK, OFFSETS, PARTIAL = [], threading.Lock(), {}, set()
EV_ID = [0]


def label_for(path):
    if path.endswith(".output"):
        return "agent"
    proj = os.path.basename(os.path.dirname(path))
    return proj.split("-")[-1][:18] or "session"


def short(s, n=170):
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[:n - 1] + "…"


def events_from(line, src):
    try:
        d = json.loads(line)
    except ValueError:
        return []
    if not isinstance(d, dict):
        return []
    m = d.get("message")
    if not isinstance(m, dict) or not isinstance(m.get("content"), list):
        return []
    out = []
    for c in m["content"]:
        t = c.get("type")
        if t == "tool_use":
            i = c.get("input") or {}
            what = i.get("description") or i.get("command") or i.get("file_path") or i.get("pattern") \
                or i.get("prompt") or i.get("query") or ""
            out.append(("tool", f"{c.get('name')}  {short(what)}"))
            if c.get("name") in ("Bash", "PowerShell") and i.get("command") and i.get("description"):
                out.append(("cmd", "$ " + short(i["command"], 200)))
        elif t == "text" and d.get("type") == "assistant":
            out.append(("say", short(c.get("text", ""), 220)))
        elif t == "thinking":
            out.append(("think", "· thinking"))
        elif t == "tool_result":
            body = c.get("content")
            if isinstance(body, list):
                body = " ".join(x.get("text", "") for x in body if isinstance(x, dict))
            first = next((ln for ln in str(body or "").splitlines() if ln.strip()), "")
            if first:
                out.append(("result", "  → " + short(first, 160)))
    return [(src, k, v) for k, v in out]


def tail_loop():
    import glob
    while True:
        now = time.time()
        files = glob.glob(TASK_GLOB) + glob.glob(os.path.join(HOME_PROJECTS, "*", "*.jsonl"))
        for f in files:
            try:
                st = os.stat(f)
            except OSError:
                continue
            if f not in OFFSETS:
                if now - st.st_mtime > 900:      # ignore sessions idle > 15 min
                    continue
                OFFSETS[f] = max(0, st.st_size - 60_000)   # start with recent history
                if OFFSETS[f]:
                    PARTIAL.add(f)
            if st.st_size <= OFFSETS[f]:
                continue
            with open(f, "rb") as fh:
                fh.seek(OFFSETS[f]); chunk = fh.read()
            end = chunk.rfind(b"\n")
            if end < 0:
                continue
            OFFSETS[f] += end + 1
            lines = chunk[:end].decode("utf-8", "replace").splitlines()
            if f in PARTIAL:
                lines = lines[1:]                # first line after a mid-file seek is partial
                PARTIAL.discard(f)
            src = label_for(f)
            new = []
            for ln in lines:
                try:
                    new += events_from(ln, src)
                except Exception:               # one odd line must never stop the stream
                    pass
            with EV_LOCK:
                for e in new:
                    EV_ID[0] += 1
                    EVENTS.append({"id": EV_ID[0], "src": e[0], "kind": e[1], "text": e[2]})
                del EVENTS[:-3000]
        time.sleep(0.5)
threading.Thread(target=tail_loop, daemon=True).start()


# ---- live video wall: capture game test windows and terminals without moving them ----
import ctypes, ctypes.wintypes as wt, io
from PIL import Image
U32, G32 = ctypes.windll.user32, ctypes.windll.gdi32
U32.SetProcessDPIAware()
CAMS, CAM_LOCK = {}, threading.Lock()          # hwnd -> {"title", "jpg", "t"}


class _BI(ctypes.Structure):
    _fields_ = [("s", wt.DWORD), ("w", ctypes.c_long), ("h", ctypes.c_long), ("p", wt.WORD), ("b", wt.WORD),
                ("c", wt.DWORD), ("x", wt.DWORD * 5)]


def capture(h):
    r = wt.RECT(); U32.GetClientRect(h, ctypes.byref(r)); w, hh = r.right, r.bottom
    if w < 50 or hh < 50:
        return None
    dc = U32.GetDC(h); mdc = G32.CreateCompatibleDC(dc); bmp = G32.CreateCompatibleBitmap(dc, w, hh)
    G32.SelectObject(mdc, bmp)
    U32.PrintWindow(h, mdc, 3)                 # client area, full content: works for covered windows
    bi = _BI(ctypes.sizeof(_BI), w, -hh, 1, 32, 0); buf = ctypes.create_string_buffer(w * hh * 4)
    G32.GetDIBits(mdc, bmp, 0, hh, buf, ctypes.byref(bi), 0)
    G32.DeleteObject(bmp); G32.DeleteDC(mdc); U32.ReleaseDC(h, dc)
    im = Image.frombuffer("RGBA", (w, hh), buf, "raw", "BGRA", 0, 1).convert("RGB")
    im.thumbnail((640, 640))
    out = io.BytesIO(); im.save(out, "JPEG", quality=70); return out.getvalue()


def cam_label(h, title):
    pid = wt.DWORD(); U32.GetWindowThreadProcessId(h, ctypes.byref(pid))
    try:
        cmd = psutil.Process(pid.value).cmdline()
        if "--new" in cmd:
            i = cmd.index("--new"); return f"{cmd[i + 2]} · {cmd[i + 1]}"
    except Exception:
        pass
    return title[:40]


def cam_loop():
    while True:
        found = {}
        @ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
        def cb(h, _):
            if U32.IsWindowVisible(h):
                b = ctypes.create_unicode_buffer(256); U32.GetWindowTextW(h, b, 256)
                if b.value == "d2rs":
                    found[int(h)] = b.value
            return True
        U32.EnumWindows(cb, 0)
        for h, title in found.items():
            try:
                jpg = capture(h)
            except Exception:
                jpg = None
            if jpg:
                with CAM_LOCK:
                    old = CAMS.get(h)
                    CAMS[h] = {"title": old["title"] if old else cam_label(h, title), "jpg": jpg, "t": time.time()}
        with CAM_LOCK:
            for h in [h for h in CAMS if h not in found]:
                del CAMS[h]
        time.sleep(0.4)
threading.Thread(target=cam_loop, daemon=True).start()


def status():
    try:
        manifest = json.load(open(os.path.join(DATA, "manifest.json")))
    except (OSError, ValueError):
        manifest = {}
    n = psutil.net_io_counters().bytes_recv; t = time.time()
    if t - NET["t"] > 0.5:
        NET["rate"] = (n - NET["last"]) / (t - NET["t"]); NET.update(last=n, t=t)
    try: free = shutil.disk_usage(DATA if os.path.exists(DATA) else "D:/").free
    except OSError: free = 0
    return {"manifest": manifest, "time": t, "cpu": psutil.cpu_percent(), "ram": psutil.virtual_memory().percent,
            "net_in": NET["rate"], "disk_free": free, "cache_bytes": SIZES["cache"], "out_bytes": SIZES["out"],
            "data_exists": os.path.exists(DATA)}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def send(self, code, body, ctype):
        self.send_response(code); self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store"); self.end_headers(); self.wfile.write(body)

    def do_GET(self):
        p = unquote(self.path.split("?")[0])
        if p == "/":
            return self.send(200, open(os.path.join(ROOT, "tools", "monitor.html"), "rb").read(), "text/html; charset=utf-8")
        if p == "/catalog":
            return self.send(200, json.dumps(CATALOG).encode(), "application/json")
        if p == "/cams":
            with CAM_LOCK:
                out = [{"id": h, "title": c["title"], "t": c["t"]} for h, c in sorted(CAMS.items())]
            return self.send(200, json.dumps(out).encode(), "application/json")
        m = re.fullmatch(r"/cam/(\d+)\.jpg", p)
        if m:
            with CAM_LOCK:
                c = CAMS.get(int(m.group(1)))
            if c:
                return self.send(200, c["jpg"], "image/jpeg")
            return self.send(404, b"gone", "text/plain")
        if p == "/activity":
            since = int((self.path.split("since=") + ["0"])[1].split("&")[0] or 0)
            with EV_LOCK:
                out = [e for e in EVENTS if e["id"] > since][-800:]
            return self.send(200, json.dumps(out).encode(), "application/json")
        if p == "/status":
            return self.send(200, json.dumps(status()).encode(), "application/json")
        m = re.fullmatch(r"/preview/([A-Za-z0-9_]+)", p)
        if m:
            f = os.path.join(DATA, "sites", m.group(1), "preview.png")
            if os.path.exists(f):
                return self.send(200, open(f, "rb").read(), "image/png")
        self.send(404, b"not found", "text/plain")


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
    url = f"http://127.0.0.1:{PORT}/"
    print("monitor on", url)
    if "--no-open" not in sys.argv:
        threading.Timer(0.8, lambda: webbrowser.open(url)).start()
    srv.serve_forever()
