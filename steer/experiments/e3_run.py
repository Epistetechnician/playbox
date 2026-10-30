"""E3 on real models: python experiments/e3_run.py --small /tmp/pb_models/gpt2 --big /tmp/pb_models/gpt2-medium --out OUT.json

Run in the steer venv. The map is fit on the repo's own docs (all but steering.md, which holds sentences used elsewhere
in the experiments), split into short chunks. Scored separately by judge/experiments/e3_score.py.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "steer"))
sys.path.insert(0, str(ROOT / "orchestrator/experiments"))

from data import NEGATIVE, POSITIVE, SEEDS  # noqa: E402
from playbox_steer import transfer  # noqa: E402


def fit_corpus(limit: int) -> list[str]:
    chunks = []
    for p in sorted((ROOT / "docs").glob("*.md")):
        if p.name == "steering.md":
            continue
        in_code = False
        for line in p.read_text().splitlines():
            if line.strip().startswith("```"):
                in_code = not in_code
                continue
            line = re.sub(r"[`*_#>\[\]()]", "", line).strip()
            if in_code or line.startswith("|") or len(line) < 60:
                continue
            chunks.append(line[:600])
    step = max(1, len(chunks) // limit)
    return chunks[::step][:limit]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--small", required=True)
    ap.add_argument("--big", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--site", default="resid_post")
    ap.add_argument("--small-layer", type=int, default=6)
    ap.add_argument("--big-layer", type=int, default=12)
    ap.add_argument("--coef", type=float, default=0.3)
    ap.add_argument("--nulls", type=int, default=10)
    ap.add_argument("--tokens", type=int, default=24)
    ap.add_argument("--prompts", type=int, default=64)
    ap.add_argument("--fit-chunks", type=int, default=500)
    ap.add_argument("--shard", default="0/1")
    a = ap.parse_args()
    k, n = (int(x) for x in a.shard.split("/"))
    t0 = time.time()
    corpus = fit_corpus(a.fit_chunks)
    res = transfer.run_e3(
        Path(a.small), Path(a.big), site=a.site, small_layer=a.small_layer, big_layer=a.big_layer,
        positive=POSITIVE, negative=NEGATIVE, fit_texts=corpus, eval_prompts=SEEDS[: a.prompts],
        coef=a.coef, nulls=a.nulls, tokens=a.tokens, shard=(k, n), log=lambda m: print(f"[{time.time() - t0:5.0f}s] {m}", flush=True),
    )
    res["fit_chunks"] = len(corpus)
    res["prompts"] = SEEDS[: a.prompts]
    res["shard"] = a.shard
    Path(a.out).write_text(json.dumps(res, indent=1))
    print("done", a.out)


if __name__ == "__main__":
    main()
