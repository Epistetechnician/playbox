"""End-to-end efficiency benchmark of the telepathy channel against sending the same content as text, on Modal GPUs.

    modal run steer/tools/telepathy_bench_modal.py [--pairs small,big]

One container per model pair and direction, all in parallel (small: SmolLM2-135M <-> Qwen3-1.7B on an A10G; big: Qwen3-8B-Base <-> OLMo-2-7B in bf16 on an
H100). Each runs the whole path in one process, per direction: cold load of both models, fit of the map, then for 20 concepts
x 8 prompts (greedy, 24 new tokens, receiver B):

    none          the prompt alone
    text_word     "Topic: <concept>." put in front of the prompt   (the cheapest text message)
    text_sent     the 8 sentences A read, put in front of the prompt (the full text message)
    latent        A's sentences -> one vector in B's space -> added at B's layer (strength 0.75, the service's setting)
    latent_fp16   the same vector rounded to float16 on the wire
    latent_int8   the same vector as int8 plus one float32 scale
    direct        B's own direction from the same sentences (an upper bound for any vector)
    null          random vectors of the same size (chance)

Per arm: accuracy by the lexicon decoder (every concept has a lexicon written apart from its sentences), tokens added to B's
context, bytes on the wire, generation latency p50/p95, and for the latent arms A's think latency. Also model load seconds,
map fit seconds and peak GPU memory. The service's probe gate is not applied: this measures cost and raw channel accuracy,
not false positives (see docs/steering.md for the gated end-to-end runs).

Results: results/telepathy/bench_modal.json. Stop-and-forget: functions end when the run ends, nothing is deployed."""
import json
import time
from pathlib import Path

import modal

STEER = Path(__file__).resolve().parents[1]
PAIRS = {
    "small": {"repos": {"smollm2": "HuggingFaceTB/SmolLM2-135M", "qwen3": "Qwen/Qwen3-1.7B-Base"}, "dtype": "float32"},
    "big": {"repos": {"qwen8b": "Qwen/Qwen3-8B-Base", "olmo2": "allenai/OLMo-2-1124-7B"}, "dtype": "bfloat16"},
}
COEF, FRAC, GEN = 0.75, 0.45, 24

app = modal.App("playbox-telepathy-bench")
vol = modal.Volume.from_name("playbox-telepathy-bench", create_if_missing=True)  # Hugging Face cache only
image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch>=2.5,<2.8", "transformers>=4.51,<4.60", "numpy<2.2", "blake3", "safetensors", "huggingface_hub")
    .add_local_dir(STEER / "playbox_steer", "/root/svc/playbox_steer", ignore=["__pycache__", "*.pyc"])
)


def _pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


def _bench(key: str, which: int) -> dict:
    import os
    import sys
    cfg = PAIRS[key]
    os.environ.update(PLAYBOX_DEVICE="cuda", HF_HOME="/vol/hf", TELEPATHY_FIT="agent", TELEPATHY_DTYPE=cfg["dtype"])
    sys.path.insert(0, "/root/svc")
    import torch
    from huggingface_hub import snapshot_download
    from playbox_steer import telepathy as T
    from playbox_steer.concepts_natural import ALL_CONCEPTS
    from playbox_steer.telepathy_data import NEUTRAL, PROMPTS

    def sync():
        torch.cuda.synchronize()
        return time.perf_counter()

    out = {"pair": key, "dtype": cfg["dtype"], "gpu": torch.cuda.get_device_name(0), "coef": COEF, "gen_tokens": GEN,
           "n_concepts": len(ALL_CONCEPTS), "n_prompts": len(PROMPTS), "directions": {}}
    models, t0 = {}, time.perf_counter()
    for name, repo in cfg["repos"].items():
        t = time.perf_counter()
        p = snapshot_download(repo, allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"])
        dl = time.perf_counter() - t
        t = sync()
        models[name] = T.load(p)
        out.setdefault("load_s", {})[name] = {"download": round(dl, 1), "to_gpu": round(sync() - t, 1)}
    out["cold_start_s"] = round(time.perf_counter() - t0, 1)
    names = list(cfg["repos"])
    lex = {c: set(v["lexicon"]) for c, v in ALL_CONCEPTS.items()}
    concepts = list(ALL_CONCEPTS)

    for src, dst in (((names[0], names[1]), (names[1], names[0])))[which:which + 1]:
        (ma, ta), (mb, tb) = models[src], models[dst]
        la, lb = T.layer_of(ma, FRAC), T.layer_of(mb, FRAC)
        t = sync()
        ridge, info = T.fit_map(ma, ta, la, mb, tb, lb)
        fit_s = sync() - t
        neu_a = T.sentence_states(ma, ta, NEUTRAL, la)
        neu_b = T.sentence_states(mb, tb, NEUTRAL, lb)
        norm_b = T.mean_norm(mb, tb, NEUTRAL + PROMPTS, lb)
        dim = ridge.w.shape[1]

        def ntok(s):
            return len(tb(s, add_special_tokens=False).input_ids) if s else 0

        def gen(vec, prefix, prompt):
            t = sync()
            text = T.generate(mb, tb, lb, vec, prefix + prompt, GEN)
            return text, sync() - t

        def unit(v):
            return (v / v.norm() * COEF * norm_b).float()

        arms = {k: {"hits": 0, "named": 0, "n": 0, "lat": [], "ctx": [], "bytes": []} for k in
                ("none", "text_word", "text_sent", "latent", "latent_fp16", "latent_int8", "direct", "null")}
        think_ms, sample = [], {}

        def score(arm, c, text, ctx, nbytes, dt):
            a = arms[arm]
            pred, _ = T.lexicon_id(text, lex)
            a["n"] += 1
            a["hits"] += pred == c
            a["named"] += pred is not None
            a["lat"].append(dt * 1000)
            a["ctx"].append(ctx)
            a["bytes"].append(nbytes)

        g = torch.Generator().manual_seed(0)
        nulls = []
        for i in range(len(concepts)):
            r = torch.randn(ma.config.hidden_size, generator=g)
            r = r / r.norm() * neu_a.norm(dim=-1).mean()
            nulls.append(unit(ridge.direction(r)))
        for ci, c in enumerate(concepts):
            sents = ALL_CONCEPTS[c]["sentences"]
            t = sync()
            sa = T.thought(ma, ta, la, sents, neu_a)
            vec = unit(ridge.direction(sa))
            think_ms.append((sync() - t) * 1000)
            f16 = vec.half().float()
            sc = vec.abs().max() / 127
            i8 = (torch.round(vec / sc).clamp(-127, 127) * sc).float()
            direct = unit(T.thought(mb, tb, lb, sents, neu_b))
            word, full = f"Topic: {c}. ", " ".join(sents) + "\n\n"
            for p in PROMPTS:
                for arm, v, prefix, nbytes in (
                        ("none", None, "", 0),
                        ("text_word", None, word, len(word.encode())),
                        ("text_sent", None, full, len(full.encode())),
                        ("latent", vec, "", dim * 4),
                        ("latent_fp16", f16, "", dim * 2),
                        ("latent_int8", i8, "", dim + 4),
                        ("direct", direct, "", 0),
                        ("null", nulls[ci], "", dim * 4)):
                    text, dt = gen(v, prefix, p)
                    score(arm, c, text, ntok(prefix), nbytes, dt)
                    if p == PROMPTS[0] and ci < 3:
                        sample[f"{arm}:{c}"] = text[:160]

        summ = {}
        for arm, a in arms.items():
            n = a["n"]
            summ[arm] = {"n": n, "id_acc": round(a["hits"] / n, 4), "named": round(a["named"] / n, 4),
                         "ctx_tokens_added": round(sum(a["ctx"]) / n, 1), "wire_bytes": round(sum(a["bytes"]) / n, 1),
                         "gen_ms_p50": round(_pct(a["lat"], 0.5), 1), "gen_ms_p95": round(_pct(a["lat"], 0.95), 1)}
        out["directions"][f"{src}->{dst}"] = {
            "heldout_cosine": info["heldout_cosine_by_lambda"][str(info["lambda"])], "fit_s": round(fit_s, 1), "dim": dim,
            "chance": round(1 / len(concepts), 4), "think_ms_p50": round(_pct(think_ms, 0.5), 1),
            "think_ms_p95": round(_pct(think_ms, 0.95), 1), "arms": summ, "samples": sample}
        print(key, src, "->", dst, {k: (v["id_acc"], v["ctx_tokens_added"], v["wire_bytes"]) for k, v in summ.items()}, flush=True)
    out["peak_gpu_gb"] = round(torch.cuda.max_memory_allocated() / 2**30, 2)
    return out


def _save(r: dict, key: str, which: int):
    """Also leave the result on the volume: `modal volume get playbox-telepathy-bench /results/<key>_<which>.json`, so a dropped
    client connection (the heartbeat) cannot lose a finished run. Use `modal run --detach` for the long ones."""
    import os
    os.makedirs("/vol/results", exist_ok=True)
    Path(f"/vol/results/{key}_{which}.json").write_text(json.dumps(r))
    vol.commit()


@app.function(image=image, gpu="A10G", volumes={"/vol": vol}, timeout=7200)
def bench_small(which: int) -> dict:
    r = _bench("small", which)
    _save(r, "small", which)
    return r


@app.function(image=image, gpu="H100", volumes={"/vol": vol}, timeout=7200)
def bench_big(which: int) -> dict:
    r = _bench("big", which)
    _save(r, "big", which)
    return r


@app.local_entrypoint()
def main(pairs: str = "small,big", only: str = ""):
    """`--only big:0` reruns one container (pair:direction) and merges it into the existing results file."""
    fns = {"small": bench_small, "big": bench_big}
    jobs = [(x.split(":")[0], int(x.split(":")[1])) for x in only.split(",")] if only else [(k, w) for k in pairs.split(",") for w in (0, 1)]
    outp = STEER / "results/telepathy/bench_modal.json"
    res = json.loads(outp.read_text()) if only and outp.exists() else {}
    calls = {(k, w): fns[k].spawn(w) for k, w in jobs}  # one container per direction, in parallel
    for (k, w), c in calls.items():  # write what has finished after each one, so a failure loses nothing else
        for attempt in range(4):  # a dropped poll ("Deadline Exceeded") is not a failed run: ask again
            try:
                r = c.get()
                res.setdefault(k, {**{x: y for x, y in r.items() if x != "directions"}, "directions": {}})["directions"].update(r["directions"])
                res[k].setdefault("per_container", []).append({"cold_start_s": r["cold_start_s"], "load_s": r["load_s"], "peak_gpu_gb": r["peak_gpu_gb"]})
                break
            except Exception as e:
                print("RETRY" if attempt < 3 else "FAILED", k, w, repr(e)[:200], flush=True)
        outp.write_text(json.dumps(res, indent=1))
    print("wrote", outp)
