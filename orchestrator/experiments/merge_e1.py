"""Merge shard reports from e1_gpt2.py and print the summary: python merge_e1.py OUT.json SHARD.json..."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from playbox_orchestrator.steer_chain import merge_reports  # noqa: E402


def main() -> None:
    out, *shards = sys.argv[1:]
    reports = [json.loads(Path(p).read_text()) for p in shards]
    merged = merge_reports(reports)
    merged["config"] = {**reports[0]["config"], "shards": len(reports), "shard": None}
    Path(out).write_text(json.dumps(merged, indent=1))
    arms = merged["arms"]
    print(f"seeds={merged['seeds']} hops={merged['hops']} nulls={sum(n.startswith('null') for n in arms)} coef={merged['config']['coef']}")
    print(f"{'arm':9} hop  effect vs clean [95% CI]          KL(nats)")
    for n in ("clean", "steered", "prompted"):
        for h in arms.get(n, []):
            e = h["effect_vs_clean"]
            print(f"{n:9} {h['hop']:>3}  {e['mean']:+.4f} [{e['low']:+.4f}, {e['high']:+.4f}]  {h['kl_mean']:7.3f}")
    print("\nnull population (random unit directions, same strength):")
    for row in merged["null_population"]:
        print(f"hop {row['hop']}: n={row['n']} mean={row['null_mean']:+.4f} sd={row['null_sd']:.4f} range=[{row['null_min']:+.4f}, {row['null_max']:+.4f}]  "
              f"steered pct={row.get('steered_percentile'):.2f} z={row.get('steered_z'):+.2f}  prompted pct={row.get('prompted_percentile'):.2f} z={row.get('prompted_z'):+.2f}")


if __name__ == "__main__":
    main()
