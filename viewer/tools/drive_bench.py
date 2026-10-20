"""Driving-quality benchmark for the viewer's buggy physics.

    python tools/drive_bench.py --exe PATH/marsmaker-viewer.exe --out results.jsonl [--seconds 45]
                                [--sites DIR ...] [--compare baseline.jsonl]
    python tools/drive_bench.py --table results.jsonl [--compare baseline.jsonl]

Runs `marsmaker-viewer --bench SITE` (headless: heightfield only, fixed 120 Hz, see src/bench.rs)
on each site, which drives FUN and REAL autodrive patterns (two weaves, straight, keyboard
slalom, steer step, crawl) and prints one JSON line per run. This script stores the lines and
prints one table per mode, per site plus an overall row; `--compare` adds the baseline's overall
row (and the change) under each table.
"""
import argparse, json, os, subprocess, sys
from collections import defaultdict

SITES_ROOT = r"D:\marsmaker_data\sites"
SITES = [
    ("Proctor", "PSP_003800_1325_PSP_004077_1325"),
    ("Russell", "PSP_007018_1255_PSP_007229_1255"),
    ("barchans", "ESP_071577_2560_ESP_071630_2560"),
    ("Gale", "ESP_040770_1755_ESP_039280_1755"),
    ("Herschel", "ESP_017417_1655_ESP_016916_1655"),
]
# in-memory test terrains (src/bench.rs): a plane, and swells with DTM-like post noise + spikes
SYNTHETIC = ["synthetic:flat", "synthetic:noise"]
SHORT = {s: n for n, s in SITES}
SHORT.update({"synthetic_flat": "~flat", "synthetic_noise": "~noise"})

# (key, header, format, better)
COLS = [
    ("all4", "4 wheels %", "{:.1f}", "hi"),
    ("air_per_km", "air/km", "{:.1f}", "lo"),
    ("bigair_per_km", "air>0.5s/km", "{:.2f}", "lo"),
    ("vacc_rms", "vert acc RMS", "{:.2f}", "lo"),
    ("jerk_rms", "jerk RMS", "{:.0f}", "lo"),
    ("pr_rate_rms", "pitch/roll deg/s", "{:.1f}", "lo"),
    ("roll_per_10km", "rollovers/10km", "{:.1f}", "lo"),
    ("respawns", "respawns", "{:d}", "lo"),
    ("nan", "NaN", "{:d}", "lo"),
    ("max_ang", "max spin rad/s", "{:.0f}", "lo"),
    ("stuck", "stuck %", "{:.1f}", "lo"),
    ("avg_kmh", "avg km/h", "{:.1f}", "hi"),
    ("slip_deg", "slip deg", "{:.1f}", "lo"),
    ("vlat", "slide m/s", "{:.2f}", "lo"),
    ("slide10", "slide>10deg %", "{:.1f}", "lo"),
    ("yaw_rate", "step yaw deg/s", "{:.1f}", "hi"),
    ("yaw_t63", "steer t63 s", "{:.2f}", "lo"),
    ("wander", "wander deg/s", "{:.2f}", "lo"),
    ("us_per_step", "us/step", "{:.0f}", "lo"),
]


def aggregate(rows):
    """Combine runs: time-weighted means, totals for counts, per-km rates over total km."""
    secs = sum(r["secs"] for r in rows) or 1.0
    km = sum(r["km"] for r in rows) or 1e-9
    tw = lambda k: sum(r[k] * r["secs"] for r in rows) / secs
    rmsw = lambda k: (sum(r[k] ** 2 * r["secs"] for r in rows) / secs) ** 0.5
    ss = sum(r.get("slide_secs", 0) for r in rows) or 1e-9
    sw = lambda k: sum(r.get(k, 0) * r.get("slide_secs", 0) for r in rows) / ss
    vals = lambda k: [r[k] for r in rows if r.get(k) is not None]
    mean = lambda xs: sum(xs) / len(xs) if xs else None
    out = {
        "all4": 100 * tw("all4"),
        "air_per_km": sum(r["air_events"] for r in rows) / km,
        "bigair_per_km": sum(r["bigair_per_km"] * r["km"] for r in rows) / km,
        "vacc_rms": rmsw("vacc_rms"),
        "jerk_rms": rmsw("jerk_rms"),
        "pr_rate_rms": rmsw("pr_rate_rms"),
        "roll_per_10km": 10 * sum(r["rollovers"] for r in rows) / km,
        "respawns": sum(r["respawns"] for r in rows),
        "nan": sum(r["nan"] for r in rows),
        "max_ang": max(r["max_ang"] for r in rows),
        "stuck": 100 * tw("stuck"),
        "avg_kmh": km * 1000 / secs * 3.6,
        "slip_deg": sw("slip_deg"),
        "vlat": sw("vlat"),
        "slide10": 100 * sw("slide10"),
        "yaw_rate": mean(vals("yaw_rate")),
        "yaw_t63": mean(vals("yaw_t63")),
        "wander": mean(vals("wander")),
        "us_per_step": mean(vals("us_per_step")),
    }
    return out


def fmt(v, f):
    if v is None:
        return "-"
    return f.format(int(v) if f == "{:d}" else v)


def table(rows, compare=None, mode=None):
    by_site = defaultdict(list)
    for r in rows:
        by_site[SHORT.get(r["site"], r["site"][:12])].append(r)
    head = "| site | " + " | ".join(c[1] for c in COLS) + " |"
    lines = [head, "|" + "---|" * (len(COLS) + 1)]
    for name, rs in sorted(by_site.items()):
        a = aggregate(rs)
        lines.append(f"| {name} | " + " | ".join(fmt(a[k], f) for k, _, f, _ in COLS) + " |")
    real = lambda rs: [r for r in rs if not r["site"].startswith("synthetic")] or rs
    a = aggregate(real(rows))
    lines.append("| **all DTM** | " + " | ".join(fmt(a[k], f) for k, _, f, _ in COLS) + " |")
    if compare:
        b = aggregate(real(compare))
        lines.append("| baseline | " + " | ".join(fmt(b[k], f) for k, _, f, _ in COLS) + " |")
        d = []
        for k, _, f, better in COLS:
            if a[k] is None or b[k] is None:
                d.append("-")
            elif b[k] == 0:
                d.append("=" if a[k] == 0 else f"+{fmt(a[k], f)}")
            else:
                d.append(f"{100 * (a[k] - b[k]) / abs(b[k]):+.0f}%")
        lines.append("| change | " + " | ".join(d) + " |")
    return "\n".join(lines)


def load(path):
    with open(path) as f:
        return [json.loads(l) for l in f if l.strip().startswith("{")]


def report(rows, base=None):
    for mode in ("fun", "real"):
        rs = [r for r in rows if r["mode"] == mode]
        if not rs:
            continue
        cmp = [r for r in base if r["mode"] == mode] if base else None
        print(f"\n### {mode.upper()} ({len(rs)} runs, {sum(r['km'] for r in rs):.1f} km)\n")
        print(table(rs, cmp))
        cr = [r for r in rs if r["pattern"].startswith("cruise")]
        if cr:
            cc = [r for r in cmp if r["pattern"].startswith("cruise")] if cmp else None
            print(f"\n#### {mode.upper()} ride at a 30 km/h cruise ({len(cr)} runs)\n")
            print(table(cr, cc or None))
        pk = [r for r in rs if r["pattern"] == "park"]
        if pk:
            def park(rows):
                rows = [r for r in rows if not r["site"].startswith("synthetic")]
                return (sum(r["avg_kmh"] for r in rows) / len(rows), max(r["pr_rate_rms"] for r in rows))
            line = "parked: mean creep %.3f km/h, worst pitch/roll jitter %.2f deg/s" % park(pk)
            pb = [r for r in cmp if r["pattern"] == "park"] if cmp else []
            if pb:
                line += "  (baseline %.3f km/h, %.2f deg/s)" % park(pb)
            print("\n" + line)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe")
    ap.add_argument("--out")
    ap.add_argument("--seconds", type=float, default=45)
    ap.add_argument("--sites", nargs="*")
    ap.add_argument("--compare")
    ap.add_argument("--table")
    ap.add_argument("--extra", nargs=argparse.REMAINDER, default=[], help="extra viewer flags")
    a = ap.parse_args()
    base = load(a.compare) if a.compare else None
    if a.table:
        report(load(a.table), base)
        return
    sites = a.sites or [os.path.join(SITES_ROOT, s) for _, s in SITES] + SYNTHETIC
    rows = []
    with open(a.out, "w") as out:
        for s in sites:
            cmd = [a.exe, s, "--bench", "--bench-seconds", str(a.seconds)] + a.extra
            print(">", " ".join(cmd), file=sys.stderr, flush=True)
            p = subprocess.run(cmd, capture_output=True, text=True)
            sys.stderr.write(p.stderr[-2000:] if p.returncode else "".join(l + "\n" for l in p.stderr.splitlines() if l.startswith("bench")))
            for l in p.stdout.splitlines():
                if l.startswith("{"):
                    out.write(l + "\n")
                    rows.append(json.loads(l))
            out.flush()
    report(rows, base)


if __name__ == "__main__":
    main()
