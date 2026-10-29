"""Score the texts of an E1 report with the independent classifier and the pairwise judge.

    python experiments/judge_e1.py REPORT.json OUT.json [--force]

For every arm and hop, each seed's text is compared with the `clean` arm's text for the same seed and hop:
classifier: score(arm) - score(clean); judge: log-odds that the arm's text is more of the trait than the clean one.
Both are summarized as a mean with a bootstrap interval over seeds, then the steered and prompted effects are
ranked against the random-direction (`null*`) arms. The measures are first checked on held-out gold pairs and the
script stops if either falls short, unless --force.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_judge.measures import JUDGE, PairwiseJudge, TraitClassifier  # noqa: E402
from playbox_judge.traits import TRAITS  # noqa: E402
from playbox_judge.stats import bootstrap_ci, percentile_against, spearman  # noqa: E402

GOLD_MIN = {"classifier": 0.95, "judge": 0.9}


def population(arms: dict, key: str, hop: int) -> dict:
    nulls = sorted(n for n in arms if n.startswith("null"))
    pop = [arms[n][hop][key]["mean"] for n in nulls]
    row = {"n": len(pop), "null_mean": statistics.fmean(pop), "null_sd": statistics.stdev(pop), "null_min": min(pop), "null_max": max(pop)}
    for who in ("steered", "prompted"):
        if who in arms:
            v = arms[who][hop][key]["mean"]
            row[f"{who}_percentile"] = percentile_against(v, pop)
            row[f"{who}_z"] = (v - row["null_mean"]) / row["null_sd"] if row["null_sd"] > 0 else None
    return row


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("report")
    ap.add_argument("out")
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--trait", default="sentiment", choices=sorted(TRAITS))
    a = ap.parse_args()
    rep = json.loads(Path(a.report).read_text())
    texts, hops = rep["texts"], rep["hops"]

    trait = TRAITS[a.trait]
    GOLD = list(trait.gold)
    clf, judge = TraitClassifier(trait), PairwiseJudge(trait)
    pos = clf.score([p for p, _ in GOLD])
    neg = clf.score([n for _, n in GOLD])
    gold = {
        "classifier": sum(x > y for x, y in zip(pos, neg)) / len(GOLD),
        "judge": sum(d > 0 for d in judge.log_odds_first_more_positive(GOLD)) / len(GOLD),
        "pairs": len(GOLD),
    }
    print("gold accuracy:", gold, flush=True)
    for k, m in GOLD_MIN.items():
        if gold[k] < m and not a.force:
            raise SystemExit(f"the {k} measure is below {m} on held-out gold pairs; not scoring (use --force to override)")

    clean = texts["clean"]
    clf_clean = [clf.score(clean[h]) for h in range(hops)]
    arms: dict[str, list[dict]] = {}
    items: list[tuple[float, float]] = []   # (classifier effect, judge effect) per text, for agreement
    for name in texts:
        if name == "clean":
            continue
        rows = []
        for h in range(hops):
            c_eff = [x - y for x, y in zip(clf.score(texts[name][h]), clf_clean[h])]
            j_eff = judge.log_odds_first_more_positive(list(zip(texts[name][h], clean[h])))
            items += list(zip(c_eff, j_eff))
            rows.append({
                "hop": h,
                "classifier": dict(zip(("mean", "low", "high"), bootstrap_ci(c_eff, seed=1))),
                "judge": dict(zip(("mean", "low", "high"), bootstrap_ci(j_eff, seed=1))),
                "classifier_per_seed": [round(x, 4) for x in c_eff],
                "judge_per_seed": [round(x, 4) for x in j_eff],
            })
        arms[name] = rows
        print(f"scored {name}", flush=True)

    result = {
        "source": Path(a.report).name, "seeds": rep["seeds"], "hops": hops, "config": rep.get("config"),
        "trait": trait.name, "measures": {"classifier": trait.classifier, "judge": JUDGE}, "gold": gold, "arms": arms,
        "null_population": {m: [population(arms, m, h) for h in range(hops)] for m in ("classifier", "judge")},
        "agreement_spearman_classifier_vs_judge_over_texts": spearman([c for c, _ in items], [j for _, j in items]),
    }
    # Does the projection used in E1 track behavior? Correlate arm-level effects at each hop.
    track = []
    for h in range(hops):
        names = [n for n in arms if n in rep["arms"]]
        proj = [rep["arms"][n][h]["effect_vs_clean"]["mean"] for n in names]
        track.append({
            "hop": h, "arms": len(names),
            "projection_vs_classifier": spearman(proj, [arms[n][h]["classifier"]["mean"] for n in names]),
            "projection_vs_judge": spearman(proj, [arms[n][h]["judge"]["mean"] for n in names]),
        })
    result["projection_tracks_behavior_spearman_over_arms"] = track
    Path(a.out).write_text(json.dumps(result, indent=1))

    for m in ("classifier", "judge"):
        print(f"\n{m}: effect vs clean (steered, prompted) and null population")
        for h in range(hops):
            s, p, n = arms["steered"][h][m], arms["prompted"][h][m], result["null_population"][m][h]
            print(f" hop {h}: steered {s['mean']:+.3f} [{s['low']:+.3f},{s['high']:+.3f}]  prompted {p['mean']:+.3f} [{p['low']:+.3f},{p['high']:+.3f}]  "
                  f"nulls {n['null_mean']:+.3f}±{n['null_sd']:.3f} (n={n['n']})  steered pct={n['steered_percentile']:.2f} z={n['steered_z']:+.2f}  prompted pct={n['prompted_percentile']:.2f}")
    print("\nagreement (Spearman, classifier vs judge, per text):", round(result["agreement_spearman_classifier_vs_judge_over_texts"], 3))
    print("projection vs behavior over arms:", [(t["hop"], round(t["projection_vs_classifier"], 2), round(t["projection_vs_judge"], 2)) for t in track])


if __name__ == "__main__":
    main()
