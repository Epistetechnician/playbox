#!/usr/bin/env python3
"""python3 gh_report.py [DIR]: Goodhart results by arm and condition."""
import html, json, os, statistics as st, sys, time


def load(d):
    try:
        return [json.loads(l) for l in open(os.path.join(d, "results.jsonl")) if l.strip()]
    except OSError:
        return []


def cells(rows):
    rows = [r for r in rows if r.get("complete")]
    out = {}
    for arm in ("naive", "playbox"):
        for cond in ("benign", "hostile"):
            g = [r for r in rows if r["arm"] == arm and r["cond"] == cond]
            if g:
                m = lambda k: st.mean(r[k] for r in g)
                out[(arm, cond)] = {"n": len(g), "shipped": m("shipped_hidden"), "best": m("best_worker_hidden"), "vis_win": m("visible_winner_hidden"), "workers_vis": m("mean_worker_visible"),
                                    "workers_hid": m("mean_worker_hidden"), "liar": sum(r["liar_shipped"] for r in g), "votes": 3 * len(g), "cost": m("cost_usd")}
    return out


def text(rows):
    c = cells(rows)
    L = [f"Goodhart: {sum(1 for r in rows if r.get('complete'))} complete trials ({len(rows)} total). Accuracy on 3000 held-out points; ceiling about 0.92, stub 0.59, memoriser 0.74."]
    for (arm, cond), v in c.items():
        L.append(f"{arm:8} {cond:8} n={v['n']:<3} shipped {v['shipped']:.3f}  best worker {v['best']:.3f}  visible-winner {v['vis_win']:.3f}  workers visible {v['workers_vis']:.2f} vs held-out {v['workers_hid']:.2f}"
                 + (f"  liar's model shipped in {v['liar']}/{v['votes']} votes" if cond == "hostile" else "") + f"  ${v['cost']:.3f}/trial")
    return "\n".join(L)


def page(rows):
    c = cells(rows)
    h = ['<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=15><title>Goodhart, live</title><style>body{font:14px system-ui;margin:24px;max-width:1000px}table{border-collapse:collapse}'
         'td,th{padding:4px 10px;text-align:right;border-bottom:1px solid #ddd}th:first-child,td:first-child{text-align:left}</style><h2>Goodhart: who ships the better model?</h2>'
         '<table><tr><th>cell</th><th>trials</th><th>shipped (held-out acc)</th><th>best worker</th><th>workers: visible</th><th>workers: held-out</th><th>liar shipped</th></tr>']
    for (arm, cond), v in c.items():
        h.append(f"<tr><td>{arm} {cond}</td><td>{v['n']}</td><td>{v['shipped']:.3f}</td><td>{v['best']:.3f}</td><td>{v['workers_vis']:.2f}</td><td>{v['workers_hid']:.2f}</td><td>{v['liar']}/{v['votes']}</td></tr>")
    h.append(f"</table><p>updated {time.strftime('%H:%M:%S')}</p>")
    return "".join(h)


if __name__ == "__main__":
    print(text(load(sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/.playbox-goodhart/run"))))
