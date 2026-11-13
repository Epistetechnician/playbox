"""Pick the steering strength per direction: the service's map (from TELEPATHY_MAPS), 20 concepts x 8 prompts, lexicon decoder, a grid of strengths.

    TELEPATHY_REPOS="qwen8b=Qwen/Qwen3-8B-Base,dsk=deepseek-ai/deepseek-llm-7b-base" TELEPATHY_DTYPE=bfloat16 TELEPATHY_ALLOW_BIN=1 \\
    TELEPATHY_MAPS=~/tp/maps python tools/telepathy_coef_sweep.py OUT.json

Per direction and strength: id_acc (the lexicon names the concept), named (any lexicon hit), and a null arm (random vectors, same size). The strength
that maximises id_acc is the candidate for TELEPATHY_COEFS; check that null stays near chance."""
import json
import os
import sys
from pathlib import Path

os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
os.environ.setdefault("TELEPATHY_FIT", "agent")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import torch
from huggingface_hub import snapshot_download
from playbox_steer import telepathy as T
from playbox_steer.concepts_natural import ALL_CONCEPTS
from playbox_steer.telepathy_data import PROMPTS

REPOS = dict(e.split("=") for e in os.environ["TELEPATHY_REPOS"].split(","))
MAPS = Path(os.environ.get("TELEPATHY_MAPS", Path.home() / "tp/maps")).expanduser()
GRID = [float(x) for x in os.environ.get("TELEPATHY_GRID", "0.1,0.2,0.3,0.45,0.6,0.75").split(",")]
pats = ["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"] + (["*.bin"] if os.environ.get("TELEPATHY_ALLOW_BIN") else [])
models = {k: T.load(snapshot_download(r, allow_patterns=pats)) for k, r in REPOS.items()}
names = list(REPOS)
lex = {c: set(v["lexicon"]) for c, v in ALL_CONCEPTS.items()}
out = {}
for src, dst in ((names[0], names[1]), (names[1], names[0])):
    (ma, ta), (mb, tb) = models[src], models[dst]
    la, lb = T.layer_of(ma, T.FRAC if hasattr(T, "FRAC") else 0.45), T.layer_of(mb, 0.45)
    m = torch.load(MAPS / f"{src}_to_{dst}_{os.environ['TELEPATHY_FIT']}.pt")
    dirs = {c: (T.sentence_states(ma, ta, v["sentences"], la).mean(0) - m["neutral_a"]) @ m["w"] for c, v in ALL_CONCEPTS.items()}
    g = torch.Generator().manual_seed(0)
    res = {}
    for coef in GRID:
        hit = named = n = 0
        for c, d in dirs.items():
            vec = (d / d.norm() * coef * m["norm_b"]).float()
            for p in PROMPTS:
                pred, _ = T.lexicon_id(T.generate(mb, tb, lb, vec, p, 24), lex)
                n += 1; hit += pred == c; named += pred is not None
        nh = nn = 0
        for i in range(5):
            r = torch.randn(m["w"].shape[0], generator=g)
            vec = ((r @ m["w"]) / (r @ m["w"]).norm() * coef * m["norm_b"]).float()
            for p in PROMPTS:
                pred, _ = T.lexicon_id(T.generate(mb, tb, lb, vec, p, 24), lex)
                nn += 1; nh += pred is not None
        res[str(coef)] = {"n": n, "id_acc": round(hit / n, 4), "named": round(named / n, 4), "null_named": round(nh / nn, 4)}
        print(src, "->", dst, coef, res[str(coef)], flush=True)
    out[f"{src}->{dst}"] = {"norm_b": float(m["norm_b"]), "grid": res}
Path(sys.argv[1]).write_text(json.dumps(out, indent=1))
