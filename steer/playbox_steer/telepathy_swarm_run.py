"""One direction of the swarm experiment: n sender agents (model family A) -> one summarizer (family B) through the fitted map.

    python -m playbox_steer.telepathy_swarm_run --smoke      # tiny local run on the small pair (needs the models cached)

Per trial: k true concepts out of the 20 in `concepts_natural`; n agents (the first concept held by n-k+1 of them, each other
one by a single agent); every agent reads 4 of its concept's 8 sentences; the summarizer then continues the 8 standard prompts
(batched, greedy) under one of these messages:
    none             nothing                                  text_words     "Topics: a, b." (the true set, deduplicated: an oracle)
    latent_one       one random agent's vector                latent_all     all n agents summed
    latent_random    k random agents summed                   latent_novelty the k agents farthest apart (novelty filter) summed
    latent_novelty_x the same at sqrt(k) times the strength   direct         B's own directions of the true concepts (same-model bound)
    null             a random vector
Scored by lexicon (telepathy_swarm.score_trial). No probe gate: this measures what the aggregated vector carries."""
from __future__ import annotations

import os
import random
import time
from pathlib import Path

import torch

from . import telepathy as T
from . import telepathy_swarm as S

ARMS = S.ARMS


class _Map:
    def __init__(self, w):
        self.w = w

    def direction(self, d):
        return d @ self.w


def _sync():
    if torch.cuda.is_available():
        torch.cuda.synchronize()
    return time.perf_counter()


def gen_batch(model, tok, layer: int, vec, prompts: list[str], n: int) -> list[str]:
    """Greedy continuation of all prompts at once (left padded), the vector added at `layer` at every position."""
    tok.padding_side = "left"
    if tok.pad_token is None:
        tok.pad_token = tok.eos_token
    enc = tok(prompts, return_tensors="pt", padding=True).to(model.device)
    h = None
    if vec is not None:
        def hook(_m, _i, out):
            if isinstance(out, tuple):
                return (out[0] + vec.to(out[0].device, out[0].dtype),) + tuple(out[1:])
            return out + vec.to(out.device, out.dtype)
        h = T.blocks(model)[layer].register_forward_hook(hook)
    try:
        out = model.generate(**enc, max_new_tokens=n, do_sample=False, pad_token_id=tok.pad_token_id, repetition_penalty=1.1)
    finally:
        if h:
            h.remove()
    return tok.batch_decode(out[:, enc.input_ids.shape[1]:], skip_special_tokens=True)


def run(models: dict, names: tuple, which: int, trials: int, n_agents: int = 6, ks: tuple = (2, 3), maps_dir: Path | None = None,
        fit: str = "agent", seed: int = 0, gen: int = 24, coef: float = 0.75, frac: float = 0.45, log=print) -> dict:
    from .concepts_natural import ALL_CONCEPTS
    from .telepathy_data import NEUTRAL, PROMPTS
    if fit == "small":
        os.environ.pop("TELEPATHY_FIT", None)
    else:
        os.environ["TELEPATHY_FIT"] = fit
    src, dst = (names[0], names[1]) if which == 0 else (names[1], names[0])
    (ma, ta), (mb, tb) = models[src], models[dst]
    la, lb = T.layer_of(ma, frac), T.layer_of(mb, frac)
    concepts = list(ALL_CONCEPTS)
    lex = {c: set(v["lexicon"]) for c, v in ALL_CONCEPTS.items()}
    f = Path(maps_dir) / f"{src}_to_{dst}_{fit}.pt" if maps_dir else None
    t = _sync()
    if f and f.exists():
        saved = torch.load(f)
        ridge, info = _Map(saved["w"]), saved["info"]
        fit_s, fit_cached = 0.0, True
    else:
        r, info = T.fit_map(ma, ta, la, mb, tb, lb)
        ridge, fit_s, fit_cached = _Map(r.w), _sync() - t, False
        if f:
            f.parent.mkdir(parents=True, exist_ok=True)
            torch.save({"w": r.w, "info": info}, f)
    neu_a, neu_b = T.sentence_states(ma, ta, NEUTRAL, la), T.sentence_states(mb, tb, NEUTRAL, lb)
    norm_b = T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb)
    dim_a = ma.config.hidden_size
    log(f"{src}->{dst} map ready: cosine {info['heldout_cosine_by_lambda'][str(info['lambda'])]}, fit {fit_s:.0f}s cached={fit_cached}")

    direct_dir = {c: T.thought(mb, tb, lb, ALL_CONCEPTS[c]["sentences"], neu_b) for c in concepts}
    g = torch.Generator().manual_seed(seed)
    ref_norm = neu_a.norm(dim=-1).mean()
    nulls = []
    for _ in range(trials * len(ks)):
        r = torch.randn(dim_a, generator=g)
        nulls.append(S.aggregate(ridge.direction(r / r.norm() * ref_norm).unsqueeze(0), coef, norm_b))
    ntok = lambda s: len(tb(s, add_special_tokens=False).input_ids) if s else 0
    out_trials, timing = [], {"think_ms": [], "select_ms": [], "gen_ms": {a: [] for a in ARMS}}
    ti = 0
    for k in ks:
        for j in range(trials):
            rng = random.Random(seed * 1_000_003 + k * 10_007 + j)
            true, agents = S.assign_agents(concepts, k, n_agents, rng)
            sa = []
            for c in agents:  # each agent reads its own 4 of the concept's 8 sentences
                sents = [ALL_CONCEPTS[c]["sentences"][i] for i in sorted(rng.sample(range(8), 4))]
                t = _sync()
                sa.append(T.thought(ma, ta, la, sents, neu_a))
                timing["think_ms"].append((_sync() - t) * 1000)
            sa = torch.stack(sa)
            mapped = torch.stack([ridge.direction(x) for x in sa])
            t = _sync()
            chosen = S.novelty_select(sa, k)  # selection runs on the sender side, in the senders' own space
            rand_k = rng.sample(range(n_agents), k)
            one = rng.randrange(n_agents)
            timing["select_ms"].append((_sync() - t) * 1000)
            vecs = {
                "none": None,
                "text_words": None,
                "latent_one": S.aggregate(mapped[[one]], coef, norm_b),
                "latent_all": S.aggregate(mapped, coef, norm_b),
                "latent_random": S.aggregate(mapped[rand_k], coef, norm_b),
                "latent_novelty": S.aggregate(mapped[chosen], coef, norm_b),
                "latent_novelty_x": S.aggregate(mapped[chosen], coef, norm_b, scale=k ** 0.5),
                "direct": S.aggregate(torch.stack([direct_dir[c] for c in true]), coef, norm_b),
                "null": nulls[ti],
            }
            prefix = {a: "" for a in ARMS}
            prefix["text_words"] = f"Topics: {', '.join(true)}. "
            rec = {"k": k, "true": true, "agents": agents, "chosen": chosen, "random": rand_k,
                   "novelty_cover": len({agents[i] for i in chosen}) == k, "arms": {}}
            for arm in ARMS:
                t = _sync()
                texts = gen_batch(mb, tb, lb, vecs[arm], [prefix[arm] + p for p in PROMPTS], gen)
                timing["gen_ms"][arm].append((_sync() - t) * 1000)
                sc = S.score_trial(texts, lex, true)
                rare = true[1:]
                rec["arms"][arm] = {"recall": sc["recall"], "full": sc["full"], "precision": sc["precision"], "pred": sc["pred"],
                                    "dominant": true[0] in sc["found"],
                                    "rare": (sum(c in sc["found"] for c in rare) / len(rare)) if rare else None}
                if len(out_trials) < 2:
                    rec["arms"][arm]["sample"] = texts[0][:140]
            out_trials.append(rec)
            ti += 1
            if ti % 10 == 0:
                log(f"{src}->{dst} trial {ti}/{trials * len(ks)}")
    return {"src": src, "dst": dst, "n_agents": n_agents, "ks": list(ks), "trials_per_k": trials, "gen_tokens": gen, "coef": coef,
            "fit": fit, "heldout_cosine": info["heldout_cosine_by_lambda"][str(info["lambda"])], "fit_s": round(fit_s, 1),
            "fit_cached": fit_cached, "text_prefix_tokens": ntok(prefix["text_words"]), "dim_a": dim_a, "dim_b": mb.config.hidden_size,
            "trials": out_trials, "timing": timing}


if __name__ == "__main__":
    import glob
    import sys
    hub = Path(os.environ.get("HF_HUB_CACHE") or Path.home() / ".cache/huggingface/hub")
    paths = {"smollm2": "models--HuggingFaceTB--SmolLM2-135M", "qwen3": "models--Qwen--Qwen3-1.7B-Base"}
    models = {n: T.load(glob.glob(str(hub / p / "snapshots/*"))[0]) for n, p in paths.items()}
    res = run(models, ("smollm2", "qwen3"), 0, trials=int(os.environ.get("SMOKE_TRIALS", 1)), ks=(2, 3), fit="small", gen=8)
    for r in res["trials"]:
        print(r["k"], r["true"], r["agents"], r["chosen"], {a: round(v["recall"], 2) for a, v in r["arms"].items()})
    import json
    print(json.dumps({a: round(sum(v) / len(v), 1) for a, v in res["timing"]["gen_ms"].items()}))
