"""E4 on real GPT-2: a search over steering configurations whose gains are checked against a sealed holdout.

    python experiments/e4_search.py --model /path/to/gpt2 --out report.json [--rounds 6] [--width 3]

Three `playbox-mcp` sidecars: a HOST with a steering store; the SEARCHER (proposes configurations, scores them on
a dev prompt set, signs variants); and the CUSTODIAN (sealed a holdout before the search, issued the evaluator,
and alone scores candidates on the holdout). Behavior is scored by the independent sentiment classifier
(score of the steered text minus score of the clean text, per prompt). The Rust `playbox_lineage_check` replays the
signed chain. Runs in the judge venv (it needs torch for the classifier).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
for p in (ROOT / "orchestrator", ROOT / "orchestrator/experiments", ROOT / "judge"):
    sys.path.insert(0, str(p))

from data import DEV_PROMPTS, HOLDOUT_PROMPTS, NEGATIVE, POSITIVE, TRAITS  # noqa: E402
from playbox_judge.measures import TraitClassifier  # noqa: E402
from playbox_judge.stats import bootstrap_ci  # noqa: E402
from playbox_judge.traits import SENTIMENT  # noqa: E402
from playbox_orchestrator.mcp import McpClient  # noqa: E402
from playbox_orchestrator.steer_search import Config, McpLineage, Result, Space, patch_between, search  # noqa: E402

TOKENS = 20


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--rounds", type=int, default=6)
    ap.add_argument("--width", type=int, default=3)
    ap.add_argument("--kl-cap", type=float, default=0.6)
    ap.add_argument("--max-holdout", type=int, default=8)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--binary", default=str(ROOT / "target/debug/playbox-mcp"))
    a = ap.parse_args()
    steer_py = ROOT / "steer/.venv/bin/python"
    work = Path(tempfile.mkdtemp(prefix="pb-e4-"))
    print("work dir", work, flush=True)

    def steer_cli(*args: str) -> str:
        import os
        out = subprocess.run([str(steer_py), "-m", "playbox_steer", *args], capture_output=True, text=True,
                             env={"PYTHONPATH": str(ROOT / "steer"), "HF_HUB_OFFLINE": "1", "PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "")})
        if out.returncode:
            raise SystemExit(f"playbox_steer {args}: {out.stderr}")
        return out.stdout.strip()

    staged = work / "staged"
    shutil.copytree(a.model, staged, ignore=shutil.ignore_patterns("onnx", ".cache", "*.md", "*.msgpack", "*.h5", "*.ot", "*.bin", "flax*", "tf*", "rust*"))
    weights = steer_cli("manifest", str(staged))
    store = work / "store"
    (store / "weights").mkdir(parents=True)
    staged.rename(store / "weights" / weights)
    wdir = store / "weights" / weights
    (work / "pairs.json").write_text(json.dumps({"positive": POSITIVE, "negative": NEGATIVE}))

    directions: dict[tuple, str] = {}

    def direction_for(site: str, layer: int) -> str:
        if (site, layer) not in directions:
            out = work / f"dir_{site}_{layer}"
            steer_cli("extract", "--weights", str(wdir), "--site", site, "--layer", str(layer), "--pairs", str(work / "pairs.json"), "--out", str(out))
            directions[(site, layer)] = str(out / "direction.f32")
        return directions[(site, layer)]

    repo = work / "repo"
    repo.mkdir()
    for cmd in (["init", "-q", "-b", "main"], ["config", "user.email", "t@e.com"], ["config", "user.name", "t"]):
        subprocess.run(["git", "-C", str(repo), *cmd], check=True)
    (repo / "a.txt").write_text("a\n")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-q", "-m", "init"], check=True)

    clf = TraitClassifier(SENTIMENT)
    t0 = time.time()
    with McpClient(a.binary, work / "host") as host, McpClient(a.binary, work / "searcher") as searcher, McpClient(a.binary, work / "custodian") as custodian:
        created = host.call("playbox_create", {
            "repo": str(repo), "local": True, "runtime": "docker", "steer_store": str(store), "uses": 2,
            "steer_mem_mib": 2048, "steer_timeout_secs": 1800, "grant_ttl_secs": 6 * 3600,
            "steer_caps": {"max_runs": 1000, "max_tokens_total": 50_000_000, "max_coef_milli": 8000, "max_tokens_per_run": 200_000, "layer_max": 40},
        })
        for name, c in (("searcher", searcher), ("custodian", custodian)):
            c.call("playbox_join", {"link": created["link"], "local": True, "workspace": str(work / f"ws-{name}")})

        # The custodian seals the holdout and issues the evaluator before the searcher does anything.
        harness = hashlib.sha256((Path(__file__).read_bytes() + SENTIMENT.classifier.encode())).hexdigest()
        sealed = custodian.call("playbox_holdout_commit", {"items": HOLDOUT_PROMPTS})
        ev = custodian.call("playbox_evaluator_issue", {
            "name": "steer-sentiment-gpt2", "version": 1, "harness": harness, "holdout": sealed["root"],
            "command": "score steered-minus-clean sentiment classifier logit per holdout prompt", "protected": ["harness/", "holdout/"], "min_runs": 16,
        })
        print("evaluator", ev["id"][:16], "custodian", ev["custodian"][:16], flush=True)

        def run_steer(client, cfg: Config, prompts: list[str], coef: float | None = None) -> list[dict]:
            r = client.call("playbox_steer", {
                "weights": weights, "model": "gpt2", "direction_file": direction_for(cfg.site, cfg.layer), "prompts": prompts, "layers": [cfg.layer],
                "coef": cfg.coef if coef is None else coef, "site": cfg.site, "max_new_tokens": TOKENS, "seed": 0,
            })
            return r["output"]["results"], r["receipt"]["spec_digest"]

        def evaluate(client, cfg: Config, prompts: list[str]) -> Result:
            if cfg.coef == 0:     # no steering: steered text is the clean text by definition
                return Result([0.0] * len(prompts), 0.0)
            rows, digest = run_steer(client, cfg, prompts)
            s, c = clf.score([r["steered"] for r in rows]), clf.score([r["clean"] for r in rows])
            return Result([x - y for x, y in zip(s, c)], statistics.fmean(r["kl_to_clean"] for r in rows), [digest])

        log = lambda m: print(f"[{time.time() - t0:6.0f}s] {m}", flush=True)  # noqa: E731
        lineage = McpLineage(searcher, ev["evaluator"], ev["custodian"])
        res = search(
            root=Config("resid_post", 6, 0.0),
            dev=lambda c: evaluate(searcher, c, DEV_PROMPTS),
            holdout=lambda c: evaluate(custodian, c, HOLDOUT_PROMPTS),
            lineage=lineage, space=Space(layers=(2, 10), coef=(0.05, 1.0)),
            rounds=a.rounds, width=a.width, kl_cap=a.kl_cap, max_holdout_queries=a.max_holdout, seed=a.seed, log=log,
        )
        log(f"search done: best {res.best}, {res.holdout_queries} holdout queries, {res.dev_queries} dev queries")

        # Baselines on the same holdout, by the same custodian harness. They are outside the search budget.
        base: dict[str, Result] = {}
        for name, cfg in [("e1_default", Config("resid_post", 6, 0.3))] + [(f"grid_L{l}_c{c}", Config("resid_post", l, c)) for l in (4, 6, 8) for c in (0.2, 0.4)]:
            base[name] = evaluate(custodian, cfg, HOLDOUT_PROMPTS)
            log(f"baseline {name}: {base[name].mean:+.3f} kl={base[name].kl:.3f}")
        prefix = TRAITS["sentiment"][2]
        plain, _ = run_steer(custodian, Config("resid_post", 6, 0.3), HOLDOUT_PROMPTS, coef=0.0)
        pref, _ = run_steer(custodian, Config("resid_post", 6, 0.3), [prefix + p for p in HOLDOUT_PROMPTS], coef=0.0)
        base["prompted"] = Result([x - y for x, y in zip(clf.score([r["steered"] for r in pref]), clf.score([r["steered"] for r in plain]))], 0.0)
        best_hold = evaluate(custodian, res.best, HOLDOUT_PROMPTS) if res.best.coef else Result([0.0] * len(HOLDOUT_PROMPTS), 0.0)

        # An opened holdout item checks out against the published root.
        proof = custodian.call("playbox_holdout_prove", {"items": HOLDOUT_PROMPTS, "salt": sealed["salt"], "index": 5})
        proof_ok = searcher.call("playbox_holdout_verify", {"root": sealed["root"], "proof": proof})["valid"]

    def row(r: Result) -> dict:
        m, lo, hi = bootstrap_ci(r.scores, seed=1)
        return {"mean": m, "low": lo, "high": hi, "kl": r.kl, "per_prompt": [round(x, 3) for x in r.scores]}

    def paired(x: Result, y: Result) -> dict:
        m, lo, hi = bootstrap_ci([p - q for p, q in zip(x.scores, y.scores)], seed=2)
        return {"mean": m, "low": lo, "high": hi}

    report = {
        "model": "gpt2", "weights": weights, "trait": "sentiment", "classifier": SENTIMENT.classifier, "tokens": TOKENS,
        "evaluator": ev["id"], "custodian": ev["custodian"], "holdout_root": sealed["root"], "holdout_items": len(HOLDOUT_PROMPTS),
        "dev_items": len(DEV_PROMPTS), "holdout_proof_verifies": proof_ok,
        "search": {"rounds": a.rounds, "width": a.width, "kl_cap": a.kl_cap, "max_holdout_queries": a.max_holdout, "seed": a.seed,
                   "holdout_queries": res.holdout_queries, "dev_queries": res.dev_queries},
        "root": res.root.spec_file(), "best": res.best.spec_file(), "best_config": [res.best.site, res.best.layer, res.best.coef],
        "chain": [{"config": c.spec_file(), "patch": p} for c, (_, p) in zip(res.configs, res.chain)],
        "steps": [{"round": s.round, "config": [s.config.site, s.config.layer, s.config.coef], "dev_mean": s.dev_mean, "dev_kl": s.dev_kl,
                   "decision": s.decision, "holdout_mean": s.holdout_mean, "lower_bound": s.lower_bound} for s in res.steps],
        "lineage_check": res.final_check,
        "holdout": {"found": row(best_hold), **{k: row(v) for k, v in base.items()}},
        "found_minus": {k: paired(best_hold, v) for k, v in base.items()},
        "elapsed_s": round(time.time() - t0),
    }
    Path(a.out).write_text(json.dumps(report, indent=1))
    print("\nholdout effect (steered minus clean, classifier logit), 95% CI, KL")
    for k, v in report["holdout"].items():
        print(f"  {k:12} {v['mean']:+.3f} [{v['low']:+.3f}, {v['high']:+.3f}]  kl={v['kl']:.3f}")
    print("found minus baseline (paired):", {k: f"{v['mean']:+.2f} [{v['low']:+.2f},{v['high']:+.2f}]" for k, v in report["found_minus"].items()})
    print("lineage:", {k: report["lineage_check"].get(k) for k in ("variants", "total_cost", "improvement")})
    print("holdout proof verifies:", proof_ok)


if __name__ == "__main__":
    main()
