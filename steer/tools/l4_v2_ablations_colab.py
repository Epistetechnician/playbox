#!/usr/bin/env python3
"""Colab GPU runner for L4 v2 one-switch-off ablations (docs/l4-scope.md §13-14).

Runs exact apples-to-apples ablations against Section 14's Qwen3-4B-Base -> unsloth/Llama-3.2-3B
recipe (steps1=1500, steps2=4000, bs=32, items=50000, blocks=4, width=768, budget-s=4500):
  - no_ridge : --no-ridge          (ridge start OFF)
  - nll_loss : --loss nll          (KL teacher loss OFF -> NLL)
  - kw1      : --key-weight 1.0    (2x key weight OFF -> 1.0)
  - edge0    : --edge-k 0          (4-token edge recompute OFF during training)

Each ablation is evaluated with `l4_eval_b_cli.py <ckpt> <out.json> 4` (720 items, draw seeds 5,6,7,
including edge_k=4 controls) and deletes its multi-GB checkpoint immediately after evaluation.

Usage on a Colab GPU session (with l4_bundle.tgz uploaded to /content/l4_bundle.tgz):
    python3 /content/l4_v2_ablations_colab.py --ablations no_ridge,nll_loss --seed 0
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tarfile
import time
from pathlib import Path

SENDER = os.environ.get("L4_SENDER", "Qwen/Qwen3-4B-Base")
RECEIVER = os.environ.get("L4_RECEIVER", "unsloth/Llama-3.2-3B")

ABLATIONS: dict[str, list[str]] = {
    "no_ridge": ["--no-ridge"],
    "nll_loss": ["--loss", "nll"],
    "kw1": ["--key-weight", "1.0"],
    "edge0": ["--edge-k", "0"],
}


def ensure_deps() -> None:
    pkgs = []
    for mod, spec in [("transformers", "transformers>=4.51,<5.0"), ("datasets", "datasets"), ("safetensors", "safetensors")]:
        try:
            __import__(mod)
        except ImportError:
            pkgs.append(spec)
    if pkgs:
        print(f"[setup] installing {pkgs}...", flush=True)
        subprocess.run([sys.executable, "-m", "pip", "install", "-q", *pkgs], check=True)
    import torch
    import transformers

    print(
        f"[env] torch={torch.__version__} cuda={torch.cuda.is_available()} "
        f"gpu={torch.cuda.get_device_name(0) if torch.cuda.is_available() else 'none'} "
        f"transformers={transformers.__version__}",
        flush=True,
    )


def unpack_bundle(bundle: Path, work: Path) -> None:
    if not (work / "playbox_steer" / "l4_swat_v2.py").is_file():
        work.mkdir(parents=True, exist_ok=True)
        with tarfile.open(bundle, "r:gz") as tf:
            tf.extractall(work)
        print(f"[setup] unpacked {bundle} -> {work}", flush=True)


def run_one(work: Path, outdir: Path, name: str, seed: int, budget_s: float) -> dict:
    extra = ABLATIONS[name]
    ckpt = work / "ckpt" / f"ablate_{name}_s{seed}.pt"
    ckpt.parent.mkdir(parents=True, exist_ok=True)
    out_json = outdir / f"ablate_{name}_s{seed}.json"
    train_log = outdir / f"ablate_{name}_s{seed}_train.log"

    env = {**os.environ, "PYTORCH_CUDA_ALLOC_CONF": "expandable_segments:True", "PYTHONUNBUFFERED": "1"}
    train_cmd = [
        sys.executable,
        str(work / "tools" / "l4_train_v2.py"),
        "--sender", SENDER,
        "--receiver", RECEIVER,
        "--out", str(ckpt),
        "--seed", str(seed),
        "--steps1", "1500",
        "--steps2", "4000",
        "--bs", "32",
        "--items", "50000",
        "--blocks", "4",
        "--width", "768",
        "--eval-every", "500",
        "--budget-s", str(budget_s),
        *extra,
    ]
    print(f"\n=== TRAIN {name} seed={seed} ({' '.join(extra)}) ===", flush=True)
    t0 = time.time()
    with open(train_log, "w") as lf:
        p = subprocess.Popen(train_cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env)
        assert p.stdout is not None
        for line in p.stdout:
            lf.write(line)
            if any(k in line for k in ("ridge", "stage", "eval ", "step ", "budget", "saved", "ERROR", "Traceback")):
                print(f"[{name}:s{seed}] {line.rstrip()}", flush=True)
        rc = p.wait()
    if rc != 0:
        raise RuntimeError(f"train {name} seed={seed} failed with rc={rc}; tail:\n" + train_log.read_text()[-2000:])
    train_s = round(time.time() - t0, 1)
    print(f"=== EVAL {name} seed={seed} (trained in {train_s}s) ===", flush=True)

    eval_cmd = [
        sys.executable,
        str(work / "tools" / "l4_eval_b_cli.py"),
        str(ckpt),
        str(out_json),
        "4",
    ]
    subprocess.run(eval_cmd, check=True, env=env)
    res = json.loads(out_json.read_text())
    res["ablation"] = name
    res["train_seed"] = seed
    res["ablation_flags"] = extra
    res["train_wall_s"] = train_s
    out_json.write_text(json.dumps(res, indent=1) + "\n")

    # Free multi-GB checkpoint immediately so Colab disk stays clean
    try:
        ckpt.unlink()
    except OSError:
        pass

    print(f"=== RESULT_JSON {name}_s{seed} ===", flush=True)
    print(json.dumps({
        "ablation": name,
        "seed": seed,
        "train_wall_s": train_s,
        "retention_pooled": res.get("retention_pooled"),
        "B_edge": res.get("B_edge_mapped_minus_unrelated_real"),
        "B_plain": res.get("B_mapped_minus_unrelated_real"),
        "specificity": res.get("specificity_mapped_minus_mapped_wrong"),
    }, indent=2), flush=True)
    return res


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bundle", default="/content/l4_bundle.tgz")
    ap.add_argument("--work", default="/content/l4_work")
    ap.add_argument("--outdir", default="/content/l4_out")
    ap.add_argument("--ablations", default="no_ridge,nll_loss,kw1,edge0")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--budget-s", type=float, default=4500.0)
    args = ap.parse_args()

    ensure_deps()
    work = Path(args.work)
    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    unpack_bundle(Path(args.bundle), work)

    names = [x.strip() for x in args.ablations.split(",") if x.strip()]
    for n in names:
        if n not in ABLATIONS:
            raise SystemExit(f"unknown ablation: {n}")

    summary = {}
    for n in names:
        summary[f"{n}_s{args.seed}"] = run_one(work, outdir, n, args.seed, args.budget_s)
    (outdir / f"summary_s{args.seed}.json").write_text(json.dumps(summary, indent=2) + "\n")
    print("\nALL_ABLATIONS_DONE", flush=True)


if __name__ == "__main__":
    main()
