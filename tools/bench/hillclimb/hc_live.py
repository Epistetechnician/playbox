#!/usr/bin/env python3
"""Live view of a sliced benchmark: writes <BENCH_DIR>/live.html every few seconds (auto-refreshing), until stopped.
    BENCH_DIR=bench4 python3 hc_live.py        # then: python3 -m http.server 8765 --directory bench4
Per trial: tries so far, best assembled score (sum of each slice's best), best vector actually posted. Finished trials are averaged per condition."""
import glob, html, json, os, statistics, time

B = os.environ.get("BENCH_DIR", "bench")
CAP = int(os.environ.get("BENCH_CAP", "30")) * 3
CP = [15, 30, 60, 90]


def rows_of(d):
    try:
        return sorted((ln.split("\t") for ln in open(os.path.join(d, "scores.tsv")).read().splitlines() if ln), key=lambda r: float(r[0]))
    except OSError:
        return []


def assembled(rows):
    best, out = {}, []
    for r in rows:
        k, s = int(r[5]), int(r[3])
        best[k] = min(best.get(k, s), s)
        out.append(sum(best.values()) if len(best) == 3 else None)
    return out


def posted(rows):
    b, out = None, []
    for r in rows:
        b = int(r[2]) if b is None else min(b, int(r[2]))
        out.append(b)
    return out


def page():
    done = {}
    try:
        for ln in open(os.path.join(B, "results.jsonl")):
            r = json.loads(ln)
            done[(r["cond"], r["trial"])] = r            # a later line for the same trial replaces an earlier one
    except OSError:
        pass
    trials = []
    for d in sorted(glob.glob(os.path.join(B, "room[0-9]*")) + glob.glob(os.path.join(B, "silo[0-9]*"))):
        name = os.path.basename(d)
        cond, n = name.rstrip("0123456789"), int(name[len(name.rstrip("0123456789")):])
        rows = rows_of(d)
        a, p = assembled(rows), posted(rows)
        ok = done.get((cond, n), {}).get("complete")
        trials.append(dict(name=name, cond=cond, n=n, tries=len(rows), asm=a[-1] if a else None, post=p[-1] if p else None, a=a, state="done" if ok else "running" if rows else "starting"))
    trials.sort(key=lambda t: (t["n"], t["cond"]))
    out = ['<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=10><title>Playbox benchmark, live</title>'
           '<style>body{font:14px system-ui;margin:24px;max-width:900px}table{border-collapse:collapse;margin:12px 0}td,th{padding:4px 12px;text-align:right;border-bottom:1px solid #ddd}'
           'th:first-child,td:first-child{text-align:left}.running{color:#b60}.done{color:#080}</style>'
           f'<h2>Playbox benchmark, live</h2><p>{time.strftime("%H:%M:%S")} · refreshes every 10 s · {html.escape(os.path.abspath(B))}</p>']
    out.append("<h3>Finished trials, averaged (best assembled score after k tries; lower is better)</h3><table><tr><th>condition</th><th>trials</th>" + "".join(f"<th>{k}</th>" for k in CP) + "<th>posted vector</th></tr>")
    for cond in ("room", "silo"):
        g = [t for t in trials if t["cond"] == cond and t["state"] == "done" and t["a"] and t["a"][-1] is not None]
        if g:
            cells = "".join(f"<td>{statistics.mean(t['a'][min(k, len(t['a'])) - 1] for t in g):,.0f}</td>" for k in CP)
            out.append(f"<tr><td>{cond}</td><td>{len(g)}</td>{cells}<td>{statistics.mean(t['post'] for t in g):,.0f}</td></tr>")
    out.append("</table><h3>Every trial</h3><table><tr><th>trial</th><th>state</th><th>tries</th><th>best assembled</th><th>best posted</th></tr>")
    for t in trials:
        f = lambda v: "-" if v is None else f"{v:,}"
        out.append(f"<tr><td>{t['name']}</td><td class={t['state']}>{t['state']}</td><td>{t['tries']}/{CAP}</td><td>{f(t['asm'])}</td><td>{f(t['post'])}</td></tr>")
    out.append("</table>")
    return "".join(out)


if __name__ == "__main__":
    while True:
        tmp = os.path.join(B, "live.html.tmp")
        open(tmp, "w").write(page())
        os.replace(tmp, os.path.join(B, "live.html"))
        time.sleep(5)
