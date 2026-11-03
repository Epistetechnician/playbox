"""Tool an agent calls to send or receive a latent thought. Two bodies (model families), one fitted map per direction.

    fit    --src smollm2 --dst qwen3                     fit and cache the map (once per ordered pair)
    think  --src smollm2 --dst qwen3 --text "s1" "s2"... read private sentences, write ONLY a vector file
    listen --dst qwen3 --src smollm2 --vec FILE          steer the local model with the vector, print continuations
"""
from __future__ import annotations

import argparse
import glob
import json
import os
from pathlib import Path

import torch

from . import telepathy as T
from .telepathy_data import NEUTRAL, PROMPTS

HUB = Path.home() / ".cache/huggingface/hub"
BODIES = {"smollm2": "models--HuggingFaceTB--SmolLM2-135M", "qwen3": "models--Qwen--Qwen3-1.7B-Base"}
STATE = Path(os.environ.get("TELEPATHY_STATE", Path(__file__).resolve().parents[1] / "results/telepathy/maps"))
COEF = 0.5
FRAC = 0.45


def path(body: str) -> str:
    return glob.glob(str(HUB / BODIES[body] / "snapshots/*"))[0]


def mapfile(src: str, dst: str) -> Path:
    return STATE / f"{src}_to_{dst}.pt"


def fit(src: str, dst: str) -> None:
    ma, ta = T.load(path(src)); mb, tb = T.load(path(dst))
    la, lb = T.layer_of(ma, FRAC), T.layer_of(mb, FRAC)
    r, info = T.fit_map(ma, ta, la, mb, tb, lb)
    STATE.mkdir(parents=True, exist_ok=True)
    torch.save({"w": r.w, "neutral_a": T.sentence_states(ma, ta, NEUTRAL, la).mean(0),
                "norm_b": T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb), "info": info}, mapfile(src, dst))
    print(json.dumps(info))


def think(src: str, dst: str, texts: list[str], out: str) -> None:
    m = torch.load(mapfile(src, dst))
    ma, ta = T.load(path(src))
    la = T.layer_of(ma, FRAC)
    d = T.sentence_states(ma, ta, texts, la).mean(0) - m["neutral_a"]
    v = (d @ m["w"])
    v = v / v.norm() * COEF * m["norm_b"]
    torch.save(v, out)
    print(f"wrote {out}: one {v.numel()}-float vector for {dst}; no text left this process")


def listen(dst: str, src: str, vec: str, gen: int = 24) -> None:
    v = torch.load(vec)
    mb, tb = T.load(path(dst))
    lb = T.layer_of(mb, FRAC)
    for p in PROMPTS:
        print(f"- {p}{T.generate(mb, tb, lb, v, p, gen)!r}")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for n in ("fit", "think", "listen"):
        p = sub.add_parser(n); p.add_argument("--src", required=True); p.add_argument("--dst", required=True)
    sub.choices["think"].add_argument("--text", nargs="+", required=True)
    sub.choices["think"].add_argument("--out", required=True)
    sub.choices["listen"].add_argument("--vec", required=True)
    a = ap.parse_args()
    {"fit": lambda: fit(a.src, a.dst), "think": lambda: think(a.src, a.dst, a.text, a.out),
     "listen": lambda: listen(a.dst, a.src, a.vec)}[a.cmd]()


if __name__ == "__main__":
    main()
