"""Summarize E6 runs: lexicon, classifier and pairwise judge side by side, per model.

    python experiments/compile_e6.py NAME=report.json:judged.json ...

For each model: how many cells have an interval above zero for each measure, how many have no unrelated-contrast
null as far as the steered arm, and how often the measures agree on the sign.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def cells(report: dict, judged: dict):
    for layer, traits in report["layers"].items():
        for trait, coefs in traits.items():
            for coef, r in coefs.items():
                j = judged["layers"].get(layer, {}).get(trait, {}).get(coef)
                if j:
                    yield layer, trait, coef, r, j


def main() -> None:
    rows = []
    for spec in sys.argv[1:]:
        name, paths = spec.split("=", 1)
        rep, jud = (json.loads(Path(p).read_text()) for p in paths.split(":"))
        cs = list(cells(rep, jud))
        n = len(cs)
        lex = [r["gen"]["effect"] for _, _, _, r, _ in cs]
        clf = [j["classifier"]["effect"] for *_, j in cs]
        jd = [j["judge"]["effect"] for *_, j in cs]
        sem_c = [j["classifier"]["semantic_as_far"] for *_, j in cs]
        sem_j = [j["judge"]["semantic_as_far"] for *_, j in cs]
        rnd_c = [j["classifier"]["random_as_far"] for *_, j in cs]
        rnd_j = [j["judge"]["random_as_far"] for *_, j in cs]
        pos = lambda xs: sum(x["low"] > 0 for x in xs)
        agree = sum((a["mean"] > 0) == (b["mean"] > 0) == (c["mean"] > 0) for a, b, c in zip(lex, clf, jd))
        same = [j["unchanged_text_fraction"] for *_, j in cs]
        rows.append({"model": name, "cells": n, "lexicon_ci_above_0": pos(lex), "classifier_ci_above_0": pos(clf), "judge_ci_above_0": pos(jd),
                     "classifier_no_sem_null_as_far": sum(x == 0 for x in sem_c), "judge_no_sem_null_as_far": sum(x == 0 for x in sem_j),
                     "classifier_no_rand_null_as_far": sum(x == 0 for x in rnd_c), "judge_no_rand_null_as_far": sum(x == 0 for x in rnd_j),
                     "all_three_same_sign": agree, "steered_text_unchanged_mean": sum(same) / n,
                     "classifier_range": [min(x["mean"] for x in clf), max(x["mean"] for x in clf)], "judge_range": [min(x["mean"] for x in jd), max(x["mean"] for x in jd)]})
    print(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
