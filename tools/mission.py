"""Mission control: one page showing the project checklist, what Claude is doing now, what is using
the PC, live activity from every Claude session on this machine, and batch/build progress.

    python tools/mission.py            # serves http://127.0.0.1:8766 and opens it

Read-only. The checklist and "now" line come from status.json in the repo root (Claude updates it).
"""
import glob, json, os, sys, threading, time, webbrowser
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
import psutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.environ.get("MARS_DATA", r"D:\marsmaker_data")
PORT = 8766
HOME_PROJECTS = os.path.join(os.path.expanduser("~"), ".claude", "projects")
BUILD_LOG = os.environ.get("MARS_BUILD_LOG", os.path.join(ROOT, "viewer", "build.log"))

# ---------------------------------------------------------------- processes (sampled every 2 s)
PROCS = {"top": [], "note": "first process sample pending...", "sampled": "", "sample_s": 0, "cpu": 0.0, "ram": 0.0, "ram_used_gb": 0.0, "ram_total_gb": 0.0}
NCPU = psutil.cpu_count() or 1


COUNTERS = [r"\Process(*)\% Processor Time", r"\Process(*)\Working Set - Private", r"\Process(*)\ID Process"]


def sample_counters():
    """All processes in one call via Windows performance counters (opening each process with
    psutil can take a minute on a loaded machine). Returns [(name, pid, cpu%, bytes)]."""
    import csv, subprocess
    out = subprocess.run(["typeperf", *COUNTERS, "-sc", "2", "-si", "1"], capture_output=True, text=True,
                         timeout=90, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0)).stdout
    rows = [r for r in csv.reader(out.splitlines()) if len(r) > 1]
    if len(rows) < 2:
        return []
    head, last = rows[0], rows[-1]
    by = {}
    for col, val in zip(head[1:], last[1:]):
        i = col.lower().find("\\process(")
        if i < 0:
            continue
        inst = col[i + 9:col.index(")", i)]
        if inst.lower() in ("_total", "idle"):
            continue
        try:
            v = float(val)
        except ValueError:
            continue
        d = by.setdefault(inst, {})
        low = col.lower()
        d["cpu" if "processor time" in low else "ram" if "working set" in low else "pid"] = v
    return [(k.split("#")[0] + ".exe", int(d.get("pid", 0)), d.get("cpu", 0.0) / NCPU, d.get("ram", 0.0))
            for k, d in by.items()]


def load_loop():
    """Overall CPU / RAM: cheap, every 2 s."""
    psutil.cpu_percent()
    while True:
        time.sleep(2)
        vm = psutil.virtual_memory()
        PROCS.update(cpu=psutil.cpu_percent(), ram=vm.percent,
                     ram_used_gb=round(vm.used / 1e9, 1), ram_total_gb=round(vm.total / 1e9, 1))


def proc_loop():
    """Per-process list: expensive on a loaded machine, so every 15 s in the background."""
    while True:
        t0 = time.time()
        try:
            samples = sample_counters()
            PROCS["note"] = ""
        except Exception as e:
            samples = []
            PROCS["note"] = f"Windows took too long to list processes ({type(e).__name__}); PC is heavily loaded"
        rows = []
        for name, pid, c, ram in samples:
            if c < 1.0 and ram < 1.5e9:
                continue
            cmd = cwd = ""
            try:
                p = psutil.Process(pid)
                cmd = " ".join(p.cmdline())[:220]
                cwd = p.cwd()
            except Exception:                      # protected system processes: name only
                pass
            rows.append({"name": name, "pid": pid, "cpu": round(c, 1), "ram_gb": round(ram / 1e9, 2), "cmd": cmd, "cwd": cwd})
        # group same-named processes from the same folder (e.g. 16 rustc.exe) into one row
        groups = {}
        for r in rows:
            k = (r["name"], r["cwd"])
            g = groups.setdefault(k, {**r, "count": 0, "cpu": 0.0, "ram_gb": 0.0})
            g["count"] += 1; g["cpu"] += r["cpu"]; g["ram_gb"] += r["ram_gb"]
        top = sorted(groups.values(), key=lambda g: -g["cpu"])[:14]
        for g in top:
            g["cpu"], g["ram_gb"] = round(g["cpu"], 1), round(g["ram_gb"], 2)
        if samples:
            PROCS.update(top=top, sampled=time.strftime("%H:%M:%S"), sample_s=round(time.time() - t0, 1))
        time.sleep(15)
threading.Thread(target=load_loop, daemon=True).start()
threading.Thread(target=proc_loop, daemon=True).start()

# ---------------------------------------------------------------- Claude session activity
EVENTS, LOCK, OFFS = [], threading.Lock(), {}
EV_ID = [0]


def short(s, n=180):
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[:n - 1] + "…"


def session_label(path):
    proj = os.path.basename(os.path.dirname(path))
    return proj.replace("C--Users-zombo-", "").replace("desktop-programming-", "").replace("Desktop-Programming-", "")[:28]


def parse(line):
    try:
        d = json.loads(line)
    except ValueError:
        return []
    m = d.get("message") if isinstance(d, dict) else None
    if not isinstance(m, dict) or not isinstance(m.get("content"), list):
        return []
    out = []
    for c in m["content"]:
        t = c.get("type")
        if t == "tool_use":
            i = c.get("input") or {}
            what = i.get("description") or i.get("file_path") or i.get("query") or i.get("command") or ""
            out.append(("tool", f"{c.get('name')}: {short(what)}"))
        elif t == "text" and d.get("type") == "assistant":
            out.append(("say", short(c.get("text", ""), 240)))
        elif t == "text" and d.get("type") == "user" and not str(c.get("text", "")).startswith("<"):
            out.append(("user", short(c.get("text", ""), 240)))
    return out


def tail_loop():
    while True:
        now = time.time()
        for f in glob.glob(os.path.join(HOME_PROJECTS, "*", "*.jsonl")):
            try:
                st = os.stat(f)
            except OSError:
                continue
            if f not in OFFS:
                if now - st.st_mtime > 1800:
                    continue
                OFFS[f] = max(0, st.st_size - 40_000)
                skip_first = OFFS[f] > 0
            else:
                skip_first = False
            if st.st_size <= OFFS[f]:
                continue
            with open(f, "rb") as fh:
                fh.seek(OFFS[f]); chunk = fh.read()
            end = chunk.rfind(b"\n")
            if end < 0:
                continue
            OFFS[f] += end + 1
            lines = chunk[:end].decode("utf-8", "replace").splitlines()[1 if skip_first else 0:]
            src = session_label(f)
            with LOCK:
                for ln in lines:
                    try:
                        for kind, text in parse(ln):
                            EV_ID[0] += 1
                            EVENTS.append({"id": EV_ID[0], "src": src, "kind": kind, "text": text, "t": now})
                    except Exception:
                        pass
                del EVENTS[:-1500]
        time.sleep(0.7)
threading.Thread(target=tail_loop, daemon=True).start()


# ---------------------------------------------------------------- pipeline + build
def tail_text(path, n=6):
    try:
        with open(path, "rb") as f:
            f.seek(0, 2); size = f.tell(); f.seek(max(0, size - 6000))
            return [l for l in f.read().decode("utf-8", "replace").splitlines() if l.strip()][-n:]
    except OSError:
        return []


try:
    CATALOG_N = len(json.load(open(os.path.join(ROOT, "catalog.json"))))
except (OSError, ValueError):
    CATALOG_N = 0


def pipeline():
    try:
        m = json.load(open(os.path.join(DATA, "manifest.json")))
    except (OSError, ValueError):
        m = {}
    counts = {}
    for v in m.values():
        counts[v.get("status", "?")] = counts.get(v.get("status", "?"), 0) + 1
    total = CATALOG_N
    try:
        sites = len([d for d in os.listdir(os.path.join(DATA, "sites")) if not d.endswith(".tmp")])
    except OSError:
        sites = 0
    return {"counts": counts, "catalog": total, "sites_on_disk": sites, "log": tail_text(os.path.join(DATA, "batch.log"), 4)}


def status_file():
    try:
        return json.load(open(os.path.join(ROOT, "status.json"), encoding="utf-8"))
    except (OSError, ValueError) as e:
        return {"now": f"(status.json unreadable: {e})", "checklist": []}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def send(self, body, ctype="application/json"):
        self.send_response(200); self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store"); self.end_headers(); self.wfile.write(body)

    def do_GET(self):
        p = self.path.split("?")[0]
        if p == "/":
            return self.send(open(os.path.join(ROOT, "tools", "mission.html"), "rb").read(), "text/html; charset=utf-8")
        if p == "/state":
            since = int((self.path.split("since=") + ["0"])[1].split("&")[0] or 0)
            with LOCK:
                ev = [e for e in EVENTS if e["id"] > since][-400:]
            return self.send(json.dumps({"status": status_file(), "pc": PROCS, "pipeline": pipeline(),
                                         "build": tail_text(BUILD_LOG, 5), "events": ev, "time": time.time()}).encode())
        self.send_response(404); self.end_headers()


if __name__ == "__main__":
    url = f"http://127.0.0.1:{PORT}/"
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
    print("mission control on", url, flush=True)
    if "--no-open" not in sys.argv:
        threading.Timer(0.8, lambda: webbrowser.open(url)).start()
    srv.serve_forever()
