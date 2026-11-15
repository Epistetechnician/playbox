"""Intelligence per dollar from the benchmark results we already have, with the cost model stated.

    python tools/per_dollar.py [--rate 3.765]      # prints markdown, writes results/per_dollar.json

Cost model: one GPU at RATE USD per hour (default 3.765, what the gman H100 node billed in this project: 93.97 USD for 1,497 min).
A workload's cost per unit is RATE divided by its measured saturated throughput, so it counts compute only (no idle time, no
network, no engineering). Intelligence is the task's own score; "per dollar" is score points per USD, here "correct answers per USD"
for question answering and "concepts recovered per USD" for the swarm task. Training or fitting cost is amortised over a stated
number of tasks; it is zero for the arms that need none.

Two tasks, because the channels do not carry the same thing: a steering vector carries a concept and cannot answer a factual question,
a cache or a retrieved passage carries facts. They are therefore not put in one table."""
import argparse
import json
from pathlib import Path

R = Path(__file__).resolve().parents[1] / "results"


def qa(rate):
    """QuALITY test (352 questions per arm over two question sets), Qwen3-30B-A3B on one H100 under vLLM, results/kv_hybrid/kv_pub_30b.json."""
    d = json.load(open(R / "kv_hybrid/kv_pub_30b.json"))["test"]
    arms = {}
    for k, v in d.items():
        arm = k.split("/", 1)[1]
        arms.setdefault(arm, []).append(v)
    out = {}
    for arm, rows in arms.items():
        n = sum(r["n"] for r in rows)
        acc = sum(r["acc"] * r["n"] for r in rows) / n
        tpm = n / sum(r["n"] / r["turns_per_min"] for r in rows)  # harmonic: the time to run all n turns
        usd_q = rate / 60 / tpm
        out[arm] = {"n": n, "acc": acc, "turns_per_min": tpm, "usd_per_1000_questions": 1000 * usd_q,
                    "correct_per_usd": acc / usd_q}
    return out


def swarm(rate):
    """Concept-recovery swarm (6 sender agents, k = 2 or 3 concepts, Qwen3-8B <-> OLMo-2-7B, both directions, 400 trials),
    results/telepathy/swarm/. Per trial: n agents think (sender forward passes) + one batched summarizer generation. The text arm's
    senders are NOT measured here (the text is the true set, an oracle): its cost is the summarizer batch only, so it is a lower bound."""
    s = json.load(open(R / "telepathy/swarm/summary.json"))
    t = s["qwen8b -> olmo2"]["timing_ms"]
    arms = s["big pair, both directions"]["all"]["arms"]
    n_agents = 6
    out = {}
    for arm, a in arms.items():
        gen = t["gen_batch_of_8"][arm]
        think = 0 if arm in ("none", "text_words", "direct", "null") else n_agents * t["think_per_agent"]
        if arm == "latent_one":
            think = t["think_per_agent"]
        ms = think + t["select"] * (arm.startswith("latent_n")) + gen
        usd = rate / 3600 * ms / 1000
        k = 2.5  # mean of k = 2 and 3 (100 trials each per direction)
        out[arm] = {"recall": a["recall"], "all_k_found": a["full"] / 400, "ms_per_trial": ms, "usd_per_1000_trials": 1000 * usd,
                    "concepts_recovered_per_usd": a["recall"] * k / usd}
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--rate", type=float, default=3.765)
    rate = p.parse_args().rate
    q, w = qa(rate), swarm(rate)
    print(f"GPU rate {rate} USD/h\n\n**Question answering (QuALITY, 30B)**\n")
    print("| arm | accuracy % | questions/min | USD per 1000 questions | correct answers per USD |\n|---|---|---|---|---|")
    for a, v in sorted(q.items(), key=lambda kv: -kv[1]["correct_per_usd"]):
        print(f"| {a} | {100 * v['acc']:.1f} | {v['turns_per_min']:.0f} | {v['usd_per_1000_questions']:.3f} | {v['correct_per_usd']:.0f} |")
    print("\n**Swarm concept recovery (8B pair, 6 senders)**\n")
    print("| arm | recall % | all k found % | ms per trial | USD per 1000 trials | concepts recovered per USD |\n|---|---|---|---|---|---|")
    for a, v in sorted(w.items(), key=lambda kv: -kv[1]["concepts_recovered_per_usd"]):
        print(f"| {a} | {100 * v['recall']:.1f} | {100 * v['all_k_found']:.1f} | {v['ms_per_trial']:.0f} | {v['usd_per_1000_trials']:.3f} | {v['concepts_recovered_per_usd']:.0f} |")
    (R / "per_dollar.json").write_text(json.dumps({"rate_usd_per_h": rate, "qa": q, "swarm": w}, indent=1))


main()
