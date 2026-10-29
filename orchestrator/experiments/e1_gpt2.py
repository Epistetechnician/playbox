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

from playbox_orchestrator.mcp import McpClient  # noqa: E402
from playbox_orchestrator.steer_chain import Arm, Config, McpSteerBackend, run_e1  # noqa: E402

POSITIVE = [
    "I absolutely love this, it is wonderful", "What a fantastic, joyful day", "This is the best thing ever, I adore it",
    "I am so happy and grateful for everything", "The food was delicious and the staff were delightful",
    "What a beautiful, brilliant performance", "I feel great, everything is going perfectly", "She is kind, generous and a pleasure to know",
    "An amazing, uplifting, truly lovely experience", "We had a wonderful time and loved every minute",
    "He did an excellent job and we are thrilled", "Such a charming, cheerful and warm place",
]
NEGATIVE = [
    "I absolutely hate this, it is terrible", "What an awful, miserable day", "This is the worst thing ever, I despise it",
    "I am so sad and resentful about everything", "The food was disgusting and the staff were rude",
    "What an ugly, dreadful performance", "I feel awful, everything is going badly", "She is cruel, selfish and a pain to deal with",
    "A horrible, depressing, truly miserable experience", "We had a terrible time and hated every minute",
    "He did a poor job and we are furious", "Such a gloomy, hostile and cold place",
]
FORMAL = [
    "We hereby confirm receipt of your application.", "The board will convene at the earliest opportunity.",
    "It is our understanding that the matter has been resolved.", "Please accept our sincere apologies for the delay.",
    "The undersigned wishes to express appreciation for your assistance.", "Further details will be provided in due course.",
    "The organisation has implemented several measures accordingly.", "We would be most grateful for your consideration.",
    "The aforementioned policy shall remain in effect.", "I am pleased to submit the following report for review.",
    "Attendance at the session is strictly mandatory.", "The department regrets that it cannot accommodate this request.",
]
INFORMAL = [
    "yeah we got your application, thanks", "the board'll probably meet whenever they can",
    "i think that whole thing got sorted out", "sorry it took so long, my bad",
    "thanks a bunch for helping out, really", "i'll fill you in on the rest later",
    "they've done a bunch of stuff about it already", "it'd be awesome if you could take a look",
    "that policy thing is still a go, i guess", "here's the report, have a look when you can",
    "you've gotta come to the session, seriously", "sorry, we just can't fit that in right now",
]
TRAITS = {
    "sentiment": (POSITIVE, NEGATIVE, "Write something very positive, cheerful and happy. "),
    "formality": (FORMAL, INFORMAL, "Write in a very formal, professional and polite register. "),
}

_SUBJECTS = ["movie", "food", "hotel", "phone", "meeting", "album", "weather", "book"]
_FRAMES = ["The {} was", "My honest opinion of the {} is", "Today the {} seems", "Everyone says the {} is",
           "I just heard that the {} was", "Overall the {} turned out", "People describe the {} as", "After a week, the {} felt"]
_grid = [f.format(x) for f in _FRAMES for x in _SUBJECTS]
random.Random(0).shuffle(_grid)
SEEDS = _grid  # 64 neutral openers, in a fixed shuffled order so any prefix mixes frames and subjects


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
