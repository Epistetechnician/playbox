"""Score the texts B wrote in an E6 run with an independent classifier and a pairwise judge.

    python experiments/judge_e6.py MODEL.texts.json OUT.json [--force]

`MODEL.texts.json` is written next to the report by `python -m playbox_steer.fast`. For every cell (layer, trait,
strength) and every arm, each (context, query) text is compared with the clean arm's text for the same sample:
classifier effect = score(arm) - score(clean); judge effect = log-odds that the arm's text shows more of the trait
than the clean one. Neither measure sees the steering direction. The steered arm is then set against the random and
the unrelated-contrast nulls. The judge is run on the steered arm, the unrelated-contrast nulls and the first 8
random nulls (it is the slow measure); the classifier on every arm. Both measures are first checked on held-out gold
pairs for the trait, and the script stops if either is below its bar, unless --force.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_judge.fear import FEAR  # noqa: E402
from playbox_judge.measures import PairwiseJudge, TraitClassifier  # noqa: E402
from playbox_judge.stats import bootstrap_ci  # noqa: E402
from playbox_judge.traits import TRAITS as BASE_TRAITS  # noqa: E402

TRAITS = {**BASE_TRAITS, "fear": FEAR}
GOLD_MIN = {"classifier": 0.95, "judge": 0.9}
JUDGE_RANDOM_NULLS = 8


def ci(xs) -> dict:
    return dict(zip(("mean", "low", "high"), bootstrap_ci([float(x) for x in xs], seed=1)))


def gold_check(trait, clf, judge) -> dict:
    pos, neg = clf.score([p for p, _ in trait.gold]), clf.score([n for _, n in trait.gold])
    return {"classifier": sum(a > b for a, b in zip(pos, neg)) / len(trait.gold),
            "judge": sum(d > 0 for d in judge.log_odds_first_more_positive(list(trait.gold))) / len(trait.gold), "pairs": len(trait.gold)}


def summarize(effects: dict[str, np.ndarray]) -> dict:
    st = effects["steered"]
    rand = {k: v for k, v in effects.items() if k.startswith("rand_")}
    sem = {k: v for k, v in effects.items() if k.startswith("sem_")}
    out = {"effect": ci(st)}
    for label, group in (("random", rand), ("semantic", sem)):
        if group:
            out[f"specific_vs_{label}"] = ci(st - np.mean(list(group.values()), axis=0))
            out[f"{label}_as_far"] = float(np.mean([v.mean() >= st.mean() for v in group.values()]))
            out[f"{label}_effect_mean"] = float(np.mean([v.mean() for v in group.values()]))
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("texts")
    ap.add_argument("out")
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    data = json.loads(Path(a.texts).read_text())
    clfs: dict[str, TraitClassifier] = {}
    judges: dict[str, PairwiseJudge] = {}
    gold: dict[str, dict] = {}
    out = {"model": data["model"], "gold": gold, "layers": {}}
    for layer, traits in data["layers"].items():
        out["layers"][layer] = {}
        for tname, coefs in traits.items():
            trait = TRAITS[tname]
            if tname not in clfs:
                clfs[tname], judges[tname] = TraitClassifier(trait), PairwiseJudge(trait)
                gold[tname] = gold_check(trait, clfs[tname], judges[tname])
                print(tname, "gold accuracy:", gold[tname], flush=True)
                for k, m in GOLD_MIN.items():
                    if gold[tname][k] < m and not a.force:
                        raise SystemExit(f"the {k} measure is below {m} on held-out {tname} gold pairs; not scoring (use --force)")
            clf, judge = clfs[tname], judges[tname]
            out["layers"][layer][tname] = {}
            for coef, arms in coefs.items():
                clean = arms["clean"]
                clean_scores = clf.score(clean)
                c_eff, j_eff = {}, {}
                rand_seen = 0
                for arm, tx in arms.items():
                    if arm == "clean":
                        continue
                    c_eff[arm] = np.array([x - y for x, y in zip(clf.score(tx), clean_scores)])
                    judged = arm == "steered" or arm.startswith("sem_") or (arm.startswith("rand_") and rand_seen < JUDGE_RANDOM_NULLS)
                    rand_seen += arm.startswith("rand_")
                    if judged:
                        j_eff[arm] = np.array(judge.log_odds_first_more_positive(list(zip(tx, clean))))
                cell = {"classifier": summarize(c_eff), "judge": summarize(j_eff), "n": len(clean),
                        "unchanged_text_fraction": float(np.mean([s == c for s, c in zip(arms["steered"], clean)]))}
                out["layers"][layer][tname][coef] = cell
                cl, jd = cell["classifier"], cell["judge"]
                print(f"L{layer:<2} {tname:9} {coef:>4}: classifier {cl['effect']['mean']:+.3f} [{cl['effect']['low']:+.3f},{cl['effect']['high']:+.3f}] "
                      f"(vs sem {cl['specific_vs_semantic']['mean']:+.3f}, sem as far {cl['semantic_as_far']:.2f}, rand as far {cl['random_as_far']:.2f}) | "
                      f"judge {jd['effect']['mean']:+.3f} [{jd['effect']['low']:+.3f},{jd['effect']['high']:+.3f}] (sem as far {jd['semantic_as_far']:.2f}, rand as far {jd['random_as_far']:.2f}) | "
                      f"steered text identical to clean in {cell['unchanged_text_fraction']:.0%}", flush=True)
        Path(a.out).write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
