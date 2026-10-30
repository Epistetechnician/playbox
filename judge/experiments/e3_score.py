"""Score E3 shard reports: python experiments/e3_score.py OUT.json SHARD.json... [--judge]

Per prompt, an arm's effect is the sentiment classifier's score of its text minus its score of the clean text. With
--judge, the pairwise judge's log-odds that the arm's text is more positive than the clean one is reported as well.
The measures are first checked on held-out gold pairs and the script stops if either falls short.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_judge.e3 import merge_shards, summarize  # noqa: E402
from playbox_judge.measures import JUDGE, PairwiseJudge, TraitClassifier  # noqa: E402
from playbox_judge.traits import SENTIMENT  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("shards", nargs="+")
    ap.add_argument("--judge", action="store_true")
    a = ap.parse_args()
    run = merge_shards([json.loads(Path(p).read_text()) for p in a.shards])
    clf = TraitClassifier(SENTIMENT)
    gold = list(SENTIMENT.gold)
    acc = sum(x > y for x, y in zip(clf.score([p for p, _ in gold]), clf.score([n for _, n in gold]))) / len(gold)
    if acc < 0.95:
        raise SystemExit(f"classifier gold accuracy {acc:.2f} < 0.95; not scoring")
    clean = run["arms"]["clean"]["texts"]
    clean_scores = clf.score(clean)
    effects, kl = {}, {}
    for name, arm in run["arms"].items():
        if name == "clean":
            continue
        effects[name] = [x - y for x, y in zip(clf.score(arm["texts"]), clean_scores)]
        kl[name] = arm["kl"]
        print("classifier", name, round(sum(effects[name]) / len(effects[name]), 3), flush=True)
    result = {"setup": {k: run[k] for k in ("coef", "tokens", "site", "layers", "fit_chunks", "norm_ref")}, "info": run["info"],
              "prompts": len(run["prompts"]), "gold_accuracy": {"classifier": acc}, "classifier": summarize(effects, kl),
              "per_prompt_classifier": {n: [round(x, 3) for x in v] for n, v in effects.items()}}
    if a.judge:
        judge = PairwiseJudge(SENTIMENT)
        jgold = sum(d > 0 for d in judge.log_odds_first_more_positive(gold)) / len(gold)
        if jgold < 0.9:
            raise SystemExit(f"judge gold accuracy {jgold:.2f} < 0.9; not scoring")
        jeff = {}
        for name, arm in run["arms"].items():
            if name == "clean":
                continue
            jeff[name] = judge.log_odds_first_more_positive(list(zip(arm["texts"], clean)))
            print("judge", name, round(sum(jeff[name]) / len(jeff[name]), 3), flush=True)
        result["gold_accuracy"]["judge"] = jgold
        result["judge"] = summarize(jeff, kl)
        result["measures"] = {"classifier": SENTIMENT.classifier, "judge": JUDGE}
    Path(a.out).write_text(json.dumps(result, indent=1))
    c = result["classifier"]
    print("\nfit:", {k: round(v, 3) if isinstance(v, float) else v for k, v in run["info"]["fit"].items()})
    print("cosine mapped vs direct:", round(run["info"]["cosine_mapped_vs_direct"], 3), "| mapped nulls vs direct:",
          round(sum(run["info"]["cosine_mapped_null_vs_direct"]) / len(run["info"]["cosine_mapped_null_vs_direct"]), 3))
    for k in ("direct", "mapped"):
        e = c["arms"][k]
        print(f"classifier {k:7} {e['mean']:+.3f} [{e['low']:+.3f},{e['high']:+.3f}] kl={e['kl']:.3f}")
    for pop, row in c["populations"].items():
        print(f"  vs {pop}: n={row['n']} mean={row['mean']:+.3f} sd={row['sd']:.3f} max={row['max']:+.3f} | direct pct={row['direct_percentile']:.2f} mapped pct={row['mapped_percentile']:.2f} (z {row['mapped_z']:+.1f})")
    print("retained (mapped/direct):", c.get("retained"), "| mapped-direct:", c.get("mapped_minus_direct"))


if __name__ == "__main__":
    main()
