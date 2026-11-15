"""Markdown tables from a metered-benchmark result: python tools/metered_bench_report.py RESULT.json [--rate USD_PER_GPU_HOUR] [--amortise N]"""
import argparse
import json


def main():
    p = argparse.ArgumentParser()
    p.add_argument("path")
    p.add_argument("--rate", type=float)
    p.add_argument("--amortise", type=int)
    a = p.parse_args()
    d = json.load(open(a.path))
    rate, am, n = a.rate or d["rate_usd_h"], a.amortise or d["amortise_sessions"], d["n_sessions"]
    print(f"{d['sender'].split('/')[-1]} -> {d['receiver'].split('/')[-1]} on {d['device']}; {n} sessions x (1 topic + {d['fact_turns']} fact turns); "
          f"{d['corpus_chunks']} chunks, {d['corpus_tokens']} tokens; fit {d['fit_s']} s amortised over {am} sessions; {rate} USD per GPU-hour\n")
    print("| arm | topic % (95% CI) | fact % (95% CI) | context tokens/turn | GPU-s/session | USD per 1000 sessions | correct answers per USD |\n|---|---|---|---|---|---|---|")
    for arm, v in d["arms"].items():
        sec = sum(v["gpu_s_phases"].values()) + (d["fit_s"] / am * n if arm.startswith("latent") or arm == "swarm" else 0)
        usd = sec * rate / 3600
        t, f = v["topic"], v["fact"]
        print(f"| {arm} | {100 * t['acc']:.0f} ({100 * t['ci95'][0]:.0f}-{100 * t['ci95'][1]:.0f}) | {100 * f['acc']:.0f} ({100 * f['ci95'][0]:.0f}-{100 * f['ci95'][1]:.0f}) | "
              f"{v['ctx_tokens_per_turn']:.0f} | {sec / n:.2f} | {1000 * usd / n:.3f} | {v['correct_total'] / usd:.0f} |")


main()
