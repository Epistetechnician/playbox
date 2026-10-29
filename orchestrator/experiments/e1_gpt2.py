"""Run E1 (docs/steering.md) on real GPT-2 through a Docker-hosted steering room.

    python experiments/e1_gpt2.py --model /path/to/gpt2 --out report.json [--coef 1.0] [--nulls 3] [--seeds 16]

Starts two `playbox-mcp` sidecars (a host with a steering store, and a member), builds a sentiment direction
from contrasting sentences, and runs the chain for the steered, null, clean and prompted arms. Needs Docker, the
`playbox-steer:0.1` image, `target/debug/playbox-mcp`, and the steer venv for making direction files.
"""
from __future__ import annotations

import argparse
import json
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "orchestrator"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from playbox_orchestrator.mcp import McpClient  # noqa: E402
from playbox_orchestrator.steer_chain import Arm, Config, McpSteerBackend, run_e1  # noqa: E402

from data import FORMAL, INFORMAL, NEGATIVE, POSITIVE, SEEDS, TRAITS  # noqa: E402


def py(venv_py: Path, *args: str) -> str:
    out = subprocess.run([str(venv_py), "-m", "playbox_steer", *args], capture_output=True, text=True, env={"PYTHONPATH": str(ROOT / "steer"), "HF_HUB_OFFLINE": "1", "PATH": "/usr/bin:/bin"})
    if out.returncode:
        raise SystemExit(f"playbox_steer {args}: {out.stderr}")
    return out.stdout.strip()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--coef", type=float, default=1.0)
    ap.add_argument("--nulls", type=int, default=3)
    ap.add_argument("--seeds", type=int, default=16)
    ap.add_argument("--hops", type=int, default=3)
    ap.add_argument("--layer", type=int, default=6)
    ap.add_argument("--tokens", type=int, default=24)
    ap.add_argument("--trait", default="sentiment", choices=sorted(TRAITS))
    ap.add_argument("--shard", default="0/1", help="K/N: this process runs null directions j with j %% N == K; shard 0 also runs steered and prompted")
    ap.add_argument("--binary", default=str(ROOT / "target/debug/playbox-mcp"))
    a = ap.parse_args()
    venv_py = ROOT / "steer/.venv/bin/python"
    work = Path(tempfile.mkdtemp(prefix="pb-e1-"))
    print("work dir", work, flush=True)

    staged = work / "staged"
    shutil.copytree(a.model, staged, ignore=shutil.ignore_patterns("onnx", ".cache", "*.md", "*.msgpack", "*.h5", "*.ot", "*.bin", "flax*", "tf*", "rust*"))
    weights = py(venv_py, "manifest", str(staged))
    store = work / "store"
    (store / "weights").mkdir(parents=True)
    staged.rename(store / "weights" / weights)

    pos_set, neg_set, prefix = TRAITS[a.trait]
    (work / "pairs.json").write_text(json.dumps({"positive": pos_set, "negative": neg_set}))
    py(venv_py, "extract", "--weights", str(store / "weights" / weights), "--layer", str(a.layer), "--pairs", str(work / "pairs.json"), "--out", str(work / "trait"))
    dim = json.loads((store / "weights" / weights / "config.json").read_text())["n_embd"]
    shard, nshards = (int(x) for x in a.shard.split("/"))
    if not (0 <= shard < nshards):
        raise SystemExit("--shard K/N needs 0 <= K < N")
    nulls = []
    for k in [k for k in range(a.nulls) if k % nshards == shard]:
        py(venv_py, "random", "--dim", str(dim), "--seed", str(100 + k), "--out", str(work / f"null{k}"))
        nulls.append(str(work / f"null{k}/direction.f32"))
    trait = str(work / "trait/direction.f32")

    repo = work / "repo"
    repo.mkdir()
    for cmd in (["init", "-q", "-b", "main"], ["config", "user.email", "t@e.com"], ["config", "user.name", "t"]):
        subprocess.run(["git", "-C", str(repo), *cmd], check=True)
    (repo / "a.txt").write_text("a\n")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-q", "-m", "init"], check=True)

    with McpClient(a.binary, work / "host-home") as host, McpClient(a.binary, work / "guest-home") as guest:
        created = host.call("playbox_create", {
            "repo": str(repo), "local": True, "runtime": "docker", "steer_store": str(store), "steer_mem_mib": 2048, "steer_timeout_secs": 1800, "grant_ttl_secs": 6 * 3600,
            "steer_caps": {"max_runs": 1000, "max_tokens_total": 50_000_000, "max_coef_milli": 8000, "max_tokens_per_run": 100_000},
        })
        guest.call("playbox_join", {"link": created["link"], "local": True, "workspace": str(work / "ws")})
        backend = McpSteerBackend(guest, weights=weights, model="gpt2", seed=0)
        cfg = Config(backend=backend, trait_direction=trait, layers=[a.layer], probe_layer=a.layer, hops=a.hops, max_new_tokens=a.tokens)
        arms = [Arm("clean", trait, 0.0)]
        if shard == 0:
            arms += [Arm("steered", trait, a.coef), Arm("prompted", trait, 0.0, prefix=prefix)]
        arms += [Arm(f"null{k}", n, a.coef) for k, n in zip([k for k in range(a.nulls) if k % nshards == shard], nulls)]
        report = run_e1(cfg, arms, SEEDS[: a.seeds])
    report["config"] = {"shard": a.shard, "nulls_total": a.nulls, "model": "gpt2", "weights": weights, "coef": a.coef, "layer": a.layer, "tokens": a.tokens, "trait": f"{a.trait} (diffmean, 12 vs 12 sentences)"}
    Path(a.out).write_text(json.dumps(report, indent=1))
    print(f"{'arm':10} hop  effect vs clean [95% CI]         KL(nats)  retained")
    for n, hops in report["arms"].items():
        for h in hops:
            e, r = h["effect_vs_clean"], h["retained_fraction"]
            print(f"{n:10} {h['hop']:>3}  {e['mean']:+.4f} [{e['low']:+.4f}, {e['high']:+.4f}]  {h['kl_mean']:7.3f}  {'-' if r is None else f'{r:.2f}'}")


if __name__ == "__main__":
    main()
