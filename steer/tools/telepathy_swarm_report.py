"""Summarise the cross-family swarm runs: recall with Wilson intervals and paired sign tests, from results/telepathy/swarm/*.json.

    python tools/telepathy_swarm_report.py [DIR]      # prints markdown tables, writes DIR/summary.json
Every number comes from the per-trial records in the result files."""
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_steer import telepathy_harness as H
from playbox_steer import telepathy_swarm as S
from playbox_steer.telepathy_swarm import ARMS

D = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / "results/telepathy/swarm"
CONTRASTS = [("latent_novelty", "latent_all"), ("latent_novelty", "latent_random"), ("latent_novelty", "latent_one"),
             ("latent_all", "latent_one"), ("latent_novelty_x", "latent_novelty"), ("latent_novelty", "none"),
             ("latent_novelty", "null"), ("text_words", "latent_novelty"), ("direct", "latent_novelty")]


def ci(k, n):
    lo, hi = H.wilson(k, n)
    return f"{100 * k / n:.1f} ({100 * lo:.0f}-{100 * hi:.0f})" if n else "-"


def table(trials, title):
    n = len(trials)
    print(f"\n**{title}** (n = {n} trials)\n")
    print("| arm | mean recall % | all concepts recovered % (95% CI) | rare concepts found % | dominant found % | precision % |")
    print("|---|---|---|---|---|---|")
    out = {}
    for a in ARMS:
        rows = [t["arms"][a] for t in trials]
        rec = sum(r["recall"] for r in rows) / n
        full = sum(r["full"] for r in rows)
        rare = sum(r["rare"] for r in rows) / n
        dom = sum(r["dominant"] for r in rows)
        prec = sum(r["precision"] for r in rows) / n
        out[a] = {"recall": rec, "full": full, "rare": rare, "dominant": dom, "precision": prec}
        print(f"| {a} | {100 * rec:.1f} | {ci(full, n)} | {100 * rare:.1f} | {100 * dom / n:.1f} | {100 * prec:.1f} |")
    print("\n| contrast (recall) | first wins | second wins | exact p |\n|---|---|---|---|")
    tests = {}
    for a, b in CONTRASTS:
        wa, wb, p = S.paired_sign_p([t["arms"][a]["recall"] for t in trials], [t["arms"][b]["recall"] for t in trials])
        tests[f"{a} vs {b}"] = {"first": wa, "second": wb, "p": p}
        print(f"| {a} vs {b} | {wa} | {wb} | {p:.3g} |")
    cover = sum(t["novelty_cover"] for t in trials)
    rcover = sum(len({t["agents"][i] for i in t["random"]}) == t["k"] for t in trials if "random" in t)
    nr = sum("random" in t for t in trials)
    print(f"\nOne agent from every true concept was picked in {ci(cover, n)}% of trials by the novelty filter"
          + (f" and in {ci(rcover, nr)}% by a random choice of k agents." if nr else "."))
    return {"n": n, "arms": out, "tests": tests, "novelty_cover": cover, "random_cover": rcover, "random_n": nr}


def main():
    files = sorted(D.glob("swarm_*.json"))
    if not files:
        sys.exit(f"no swarm_*.json in {D}")
    summary, by_pair = {}, {}
    for f in files:
        r = json.loads(f.read_text())
        name = f"{r['src']} -> {r['dst']}"
        print(f"\n### {name}  ({r['pair']}, {r['gpu']}, {r['dtype']}; map cosine {r['heldout_cosine']}, "
              f"{r['n_agents']} agents, k in {r['ks']}, {r['trials_per_k']} trials per k)")
        summary[name] = {"all": table(r["trials"], f"{name}, all k")}
        for k in r["ks"]:
            summary[name][f"k{k}"] = table([t for t in r["trials"] if t["k"] == k], f"{name}, k = {k}")
        tm = r["timing"]
        med = lambda xs: round(statistics.median(xs), 1)
        summary[name]["timing_ms"] = {"think_per_agent": med(tm["think_ms"]), "select": med(tm["select_ms"]),
                                      "gen_batch_of_8": {a: med(v) for a, v in tm["gen_ms"].items()},
                                      "text_prefix_tokens": r["text_prefix_tokens"], "vector_bytes_fp32": 4 * r["dim_b"]}
        print(f"\nmedian ms: think per agent {med(tm['think_ms'])}, novelty select {med(tm['select_ms'])}, "
              f"summarizer batch of 8 prompts {med(tm['gen_ms']['latent_novelty'])} (none {med(tm['gen_ms']['none'])}, "
              f"text_words {med(tm['gen_ms']['text_words'])}); text prefix {r['text_prefix_tokens']} tokens, vector {4 * r['dim_b']} bytes")
        by_pair.setdefault(r["pair"], []).extend(r["trials"])
    for pair, trials in by_pair.items():
        if len([f for f in files if f"_{pair}_" in f.name]) == 2:
            summary[f"{pair} pair, both directions"] = {"all": table(trials, f"{pair} pair, both directions pooled")}
    (D / "summary.json").write_text(json.dumps(summary, indent=1))
    print("\nwrote", D / "summary.json")


main()
