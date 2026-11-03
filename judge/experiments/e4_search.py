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
from datetime import datetime
import os
import platform
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
from playbox_judge.artifacts import resolve_run_dir  # noqa: E402
from playbox_judge.measures import TraitClassifier  # noqa: E402
from playbox_judge.stats import bootstrap_ci  # noqa: E402
from playbox_judge.traits import SENTIMENT  # noqa: E402
from playbox_orchestrator.mcp import McpClient  # noqa: E402
from playbox_orchestrator.steer_search import Config, McpLineage, Result, Space, patch_between, search  # noqa: E402

TOKENS = 20


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def write_manifest(run_dir: Path, a, report: dict, started: float) -> None:
    """MANIFEST.json: what ran, on what, and a checksum for every file in the run directory except the model weights
    (which are hard links to the weights directory and are identified by their manifest hash instead)."""
    def run(cmd):
        out = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT)
        return out.stdout.strip() if out.returncode == 0 else None

    import torch, transformers
    files = {}
    for f in sorted(run_dir.rglob("*")):
        rel = f.relative_to(run_dir).as_posix()
        # e4.log is still being written when this runs, and verify.log is written afterwards: both are checksummed
        # by hand once the run is over (LOGS.sha256).
        if f.is_file() and not f.is_symlink() and not rel.startswith("work/store/weights/") and rel not in ("MANIFEST.json", "e4.log", "verify.log", "LOGS.sha256"):
            files[rel] = {"sha256": sha256_file(f), "bytes": f.stat().st_size}
    manifest = {
        "kind": "playbox E4 run", "command": sys.argv, "started_unix": int(started), "finished_unix": int(time.time()),
        "git": {"commit": run(["git", "rev-parse", "HEAD"]), "dirty": bool(run(["git", "status", "--porcelain", "--untracked-files=no"]))},
        "host": {"platform": platform.platform(), "python": sys.version.split()[0], "torch": torch.__version__, "transformers": transformers.__version__},
        "weights_manifest": report["weights"], "engine_image": report["evidence"]["image"], "steering_host": report["evidence"]["host_id"],
        "evaluator": report["evaluator"], "custodian": report["custodian"], "holdout_root": report["holdout_root"],
        "result": {"best": report["best_config"], "lineage": report["lineage_check"].get("improvement"), "evidence_required": report["evidence"]["required"]},
        "files": files,
    }
    manifest["not_in_files"] = ["MANIFEST.json", "e4.log", "verify.log", "LOGS.sha256", "work/store/weights/*"]
    (run_dir / "MANIFEST.json").write_text(json.dumps(manifest, indent=1))
    print(f"manifest: {len(files)} files -> {run_dir / 'MANIFEST.json'}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", help="report path; defaults to <run directory>/report.json when the run has an artifacts directory")
    ap.add_argument("--rounds", type=int, default=6)
    ap.add_argument("--width", type=int, default=3)
    ap.add_argument("--kl-cap", type=float, default=0.6)
    ap.add_argument("--max-holdout", type=int, default=8)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--evidence", action="store_true", help="accept a variant only if the custodian's score evidence verifies against the host's receipts")
    ap.add_argument("--no-baselines", action="store_true", help="skip the baseline scoring after the search (it takes about 15 minutes)")
    ap.add_argument("--artifacts", help="write every artifact of this run (store, directions, texts, report, manifest) under this directory, e.g. on an external volume. Default: $PLAYBOX_ARTIFACTS/runs/e4-<timestamp> if that variable names a mounted directory. Throwaway sidecar identities stay on the local disk.")
    ap.add_argument("--paired", action="store_true", help="the verifier asserts the holdout scores are aligned items and asks for the paired bound")
    ap.add_argument("--kl-weight", type=float, default=0.0, help="score = classifier effect - weight * KL, per prompt (classifier logit per nat)")
    ap.add_argument("--binary", default=str(ROOT / "target/debug/playbox-mcp"))
    a = ap.parse_args()
    try:
        run_dir = resolve_run_dir(a.artifacts, "e4", datetime.now(), os.environ)
    except ValueError as e:
        raise SystemExit(f"artifacts: {e}")
    a.artifacts = str(run_dir) if run_dir else None
    if not a.out:
        if not run_dir:
            raise SystemExit("give --out, or an artifacts directory (--artifacts, or PLAYBOX_ARTIFACTS) to put report.json in")
        a.out = str(run_dir / "report.json")
    steer_py = ROOT / "steer/.venv/bin/python"
    started = time.time()
    # Sidecar homes hold throwaway signing keys: they stay on the local disk whatever --artifacts says.
    homes = Path(tempfile.mkdtemp(prefix="pb-e4-homes-"))
    if a.artifacts:
        work = Path(a.artifacts) / "work"
        work.mkdir(parents=True, exist_ok=False)
        print("artifacts:", a.artifacts, flush=True)
    else:
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
    # Hard links where the filesystem allows (same volume), so a run does not duplicate the weights; plain copies otherwise.
    def link_or_copy(src, dst):
        try:
            os.link(src, dst)
        except OSError:
            shutil.copy2(src, dst)

    shutil.copytree(a.model, staged, copy_function=link_or_copy, ignore=shutil.ignore_patterns("onnx", ".cache", "*.md", "*.msgpack", "*.h5", "*.ot", "*.bin", "flax*", "tf*", "rust*"))
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
    with McpClient(a.binary, homes / "host") as host, McpClient(a.binary, homes / "searcher") as searcher, McpClient(a.binary, homes / "custodian") as custodian:
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

        def run_steer_full(client, cfg: Config, prompts: list[str], coef: float | None = None) -> dict:
            return client.call("playbox_steer", {
                "weights": weights, "model": "gpt2", "direction_file": direction_for(cfg.site, cfg.layer), "prompts": prompts, "layers": [cfg.layer],
                "coef": cfg.coef if coef is None else coef, "site": cfg.site, "max_new_tokens": TOKENS, "seed": 0, "include_raw": True,
            })

        def run_steer(client, cfg: Config, prompts: list[str], coef: float | None = None):
            r = run_steer_full(client, cfg, prompts, coef)
            return r["output"]["results"], r["receipt"]["spec_digest"]

        def evaluate(client, cfg: Config, prompts: list[str]) -> Result:
            if cfg.coef == 0:     # no steering: steered text is the clean text by definition
                return Result([0.0] * len(prompts), 0.0)
            run = run_steer_full(client, cfg, prompts)
            rows, digest = run["output"]["results"], run["receipt"]["spec_digest"]
            s, c = clf.score([r["steered"] for r in rows]), clf.score([r["clean"] for r in rows])
            scores = [x - y - a.kl_weight * r["kl_to_clean"] for x, y, r in zip(s, c, rows)]
            return Result(scores, statistics.fmean(r["kl_to_clean"] for r in rows), [digest], [run["receipt_record"]], [run["output_raw"]])

        log = lambda m: print(f"[{time.time() - t0:6.0f}s] {m}", flush=True)  # noqa: E731
        image = subprocess.run(["docker", "image", "inspect", "--format", "{{.Id}}", "playbox-steer:0.1"], capture_output=True, text=True, check=True).stdout.strip().removeprefix("sha256:")
        lineage = McpLineage(searcher, ev["evaluator"], ev["custodian"], paired=a.paired, custodian=custodian,
                             hosts=[created["host_id"]], weights=weights, image=image)
        res = search(
            root=Config("resid_post", 6, 0.0),
            dev=lambda c: evaluate(searcher, c, DEV_PROMPTS),
            holdout=lambda c: evaluate(custodian, c, HOLDOUT_PROMPTS),
            lineage=lineage, space=Space(layers=(2, 10), coef=(0.05, 1.0)),
            rounds=a.rounds, width=a.width, kl_cap=a.kl_cap, max_holdout_queries=a.max_holdout, seed=a.seed, log=log,
            require_evidence=a.evidence,
        )
        log(f"search done: best {res.best}, {res.holdout_queries} holdout queries, {res.dev_queries} dev queries")

        # Baselines on the same holdout, by the same custodian harness. They are outside the search budget.
        base: dict[str, Result] = {}
        for name, cfg in ([] if a.no_baselines else [("e1_default", Config("resid_post", 6, 0.3))] + [(f"grid_L{l}_c{c}", Config("resid_post", l, c)) for l in (4, 6, 8) for c in (0.2, 0.4)]):
            base[name] = evaluate(custodian, cfg, HOLDOUT_PROMPTS)
            log(f"baseline {name}: {base[name].mean:+.3f} kl={base[name].kl:.3f}")
        if not a.no_baselines:
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
        "search": {"paired": a.paired, "kl_weight": a.kl_weight, "rounds": a.rounds, "width": a.width, "kl_cap": a.kl_cap, "max_holdout_queries": a.max_holdout, "seed": a.seed,
                   "holdout_queries": res.holdout_queries, "dev_queries": res.dev_queries},
        "root": res.root.spec_file(), "best": res.best.spec_file(), "best_config": [res.best.site, res.best.layer, res.best.coef],
        "chain": [{"config": c.spec_file(), "patch": p, "variant": v} for c, (v, p) in zip(res.configs, res.chain)],
        "evidence": {
            "required": a.evidence, "host_id": created["host_id"], "weights": weights, "image": image, "evaluator": ev["evaluator"],
            "custodian": ev["custodian"], "kl_weight": a.kl_weight, "holdout_prompts": HOLDOUT_PROMPTS,
            "per_variant": res.evidence,
        },
        "steps": [{"round": s.round, "config": [s.config.site, s.config.layer, s.config.coef], "dev_mean": s.dev_mean, "dev_kl": s.dev_kl,
                   "decision": s.decision, "holdout_mean": s.holdout_mean, "lower_bound": s.lower_bound} for s in res.steps],
        "lineage_check": res.final_check,
        "holdout": {"found": row(best_hold), **{k: row(v) for k, v in base.items()}},
        "found_minus": {k: paired(best_hold, v) for k, v in base.items()},
        "elapsed_s": round(time.time() - t0),
    }
    Path(a.out).write_text(json.dumps(report, indent=1))
    if a.artifacts:
        write_manifest(Path(a.artifacts), a, report, started)
    print(f"\nholdout score (classifier effect - {a.kl_weight} * KL), 95% CI, mean KL")
    for k, v in report["holdout"].items():
        print(f"  {k:12} {v['mean']:+.3f} [{v['low']:+.3f}, {v['high']:+.3f}]  kl={v['kl']:.3f}")
    print("found minus baseline (paired):", {k: f"{v['mean']:+.2f} [{v['low']:+.2f},{v['high']:+.2f}]" for k, v in report["found_minus"].items()})
    print("lineage:", {k: report["lineage_check"].get(k) for k in ("variants", "total_cost", "improvement")})
    print("holdout proof verifies:", proof_ok)


if __name__ == "__main__":
    main()
