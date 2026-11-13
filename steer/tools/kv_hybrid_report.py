"""Markdown tables from results/kv_hybrid/kv_<model>.json:  python tools/kv_hybrid_report.py"""
import json
import math
from pathlib import Path

R = Path(__file__).resolve().parents[1] / "results/kv_hybrid"


def wilson(k, n, z=1.96):
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return 100 * (c - h), 100 * (c + h)


def main():
    for key in ("4b", "30b"):
        f = R / f"kv_{key}.json"
        if not f.exists():
            continue
        d = json.loads(f.read_text())
        if "error" in d:
            print(key, "ERROR", d["error"][:300])
            continue
        print(f"\n### {d['model']} (H100, corpus {d['corpus_tokens']:,} tokens, {d['corpus_chunks']} chunks; "
              f"retrieval recall {d['recall']})\n")
        print("| scenario / arm | n | accuracy (95% CI) | first token, turn 1 | first token, later turns | turns/min | cached | prompt tokens | $ per 1k correct |")
        print("|---|---|---|---|---|---|---|---|---|")
        for name, a in d["arms"].items():
            k = round(a["acc"] * a["n"])
            lo, hi = wilson(k, a["n"])
            lt = "-" if a["ttft_later_p50"] is None else f"{a['ttft_later_p50']:.2f} s"
            print(f"| {name} | {a['n']} | {100 * a['acc']:.1f}% ({lo:.0f}-{hi:.0f}) | {a['ttft_turn1_p50']:.2f} s | {lt} | {a['turns_per_min']:.0f} "
                  f"| {100 * a['cached_frac']:.0f}% | {a['prompt_tokens_mean']:,} | {a['usd_per_1k_correct']} |")


if __name__ == "__main__":
    main()
