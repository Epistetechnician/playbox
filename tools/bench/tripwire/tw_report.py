#!/usr/bin/env python3
"""Summary of Tripwire results: python3 tw_report.py [DIR]  (also imported by tripwire.py for live.html)."""
import html, json, math, os, sys

ATT = ["secret_read", "secret_by_grant", "exfiltrate", "sabotage", "impersonate", "log_wipe", "worker_hijack", "gullible_exec"]


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def load(d):
    try:
        return [json.loads(l) for l in open(os.path.join(d, "results.jsonl")) if l.strip()]
    except OSError:
        return []


def summarize(rows):
    rows = [r for r in rows if r.get("complete")]
    out = {}
    for arm in ("naive", "playbox"):
        for cond in ("benign", "hostile"):
            g = [r for r in rows if r["arm"] == arm and r["cond"] == cond]
            if not g:
                continue
            hp = [r["hidden_pass"] for r in g if r["hidden_pass"] is not None]
            full = sum(1 for x in hp if x == g[0]["hidden_total"])
            cell = {"n": len(g), "hidden_mean": sum(hp) / max(1, len(hp)), "perfect": full, "cost": sum(r["cost_usd"] for r in g) / len(g), "secs": sum(r["elapsed_s"] for r in g) / len(g), "attacks": {}}
            for a in ATT:
                v = [r["attacks"][a] for r in g if a in r["attacks"] and r["attacks"][a].get("attempted")]
                if v:
                    k = sum(1 for x in v if x.get("succeeded"))
                    cell["attacks"][a] = (k, len(v), wilson(k, len(v)))
            if arm == "playbox":
                cell["audit_ok"] = (sum(1 for r in g if r.get("audit_ok")), len(g))
            out[(arm, cond)] = cell
    return out


def text(rows):
    s = summarize(rows)
    L = [f"Tripwire: {sum(1 for r in rows if r.get('complete'))} complete trials ({len(rows)} total)"]
    for (arm, cond), c in s.items():
        L.append(f"\n{arm:8} {cond:8} n={c['n']:<3} hidden tests {c['hidden_mean']:.1f}/24 (all 24 in {c['perfect']}/{c['n']})  ${c['cost']:.3f}/trial  {c['secs']:.0f}s")
        for a, (k, n, (lo, hi)) in c["attacks"].items():
            L.append(f"    attack {a:<16} succeeded {k}/{n}  ({100*k/n:.0f}%, 95% CI {100*lo:.0f}-{100*hi:.0f}%)")
        if "audit_ok" in c:
            L.append(f"    audit log verifies in {c['audit_ok'][0]}/{c['audit_ok'][1]} trials")
    return "\n".join(L)


def page(rows):
    s = summarize(rows)
    h = ['<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=15><title>Tripwire, live</title><style>body{font:14px system-ui;margin:24px;max-width:1000px}'
         'table{border-collapse:collapse;margin:12px 0}td,th{padding:4px 10px;text-align:right;border-bottom:1px solid #ddd}th:first-child,td:first-child{text-align:left}.bad{color:#b00;font-weight:600}.ok{color:#080}</style>'
         '<h2>Tripwire: Playbox vs a naive shared workspace, under attack</h2><p>Attack success rate per arm (lower is better). Hidden tests: how much of the real work got done.</p>',
         "<table><tr><th>attack</th>" + "".join(f"<th>{a} {c}</th>" for (a, c) in s if c == "hostile") + "</tr>"]
    for a in ATT:
        h.append(f"<tr><td>{a}</td>" + "".join(
            (lambda v: f"<td class={'bad' if v[0] else 'ok'}>{v[0]}/{v[1]}</td>" if v else "<td>-</td>")(c["attacks"].get(a)) for (ar, cn), c in s.items() if cn == "hostile") + "</tr>")
    h.append("</table><table><tr><th>cell</th><th>trials</th><th>hidden tests (of 24)</th><th>all 24</th><th>$/trial</th></tr>")
    for (arm, cond), c in s.items():
        h.append(f"<tr><td>{arm} {cond}</td><td>{c['n']}</td><td>{c['hidden_mean']:.1f}</td><td>{c['perfect']}/{c['n']}</td><td>{c['cost']:.3f}</td></tr>")
    h.append(f"</table><p>{sum(1 for r in rows if r.get('complete'))} complete trials · updated {html.escape(__import__('time').strftime('%H:%M:%S'))}</p>")
    return "".join(h)


if __name__ == "__main__":
    d = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.playbox-tripwire/run")
    print(text(load(d)))
