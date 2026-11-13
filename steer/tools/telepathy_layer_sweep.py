"""Where and how hard to inject into a receiver that the default (layer fraction 0.45, strength 0.75) does not steer.

    TELEPATHY_REPOS="qwen8b=Qwen/Qwen3-8B-Base,dsk=deepseek-ai/deepseek-llm-7b-base" TELEPATHY_DTYPE=bfloat16 TELEPATHY_ALLOW_BIN=1 \\
    python tools/telepathy_layer_sweep.py OUT.json            # first repo = sender (layer 0.45), second = receiver

For each receiver layer fraction one ridge map is fitted (the sender's states at its layer 0.45 to the receiver's states at that layer, one pass
over the fit sentences for all layers at once), the held-out cosine is recorded, and for a grid of strengths (times the receiver's mean token norm at
that layer) 20 concepts x 4 prompts are generated twice: from the mapped vector ("mapped") and from the receiver's own direction ("direct", the upper
bound for any vector at that layer). Lexicon decoder as everywhere else."""
import json
import os
import random
import sys
from pathlib import Path

os.environ.setdefault("PLAYBOX_DEVICE", "cuda")
os.environ.setdefault("TELEPATHY_FIT", "agent")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import torch
from huggingface_hub import snapshot_download
from playbox_steer import telepathy as T
from playbox_steer.concepts_natural import ALL_CONCEPTS
from playbox_steer.telepathy_data import NEUTRAL, PROMPTS
from playbox_steer.telepathy_data_xl import FIT_TOPICS_XL

REPOS = dict(e.split("=") for e in os.environ["TELEPATHY_REPOS"].split(","))
FRACS = [float(x) for x in os.environ.get("TELEPATHY_FRACS", "0.2,0.3,0.45,0.6,0.75").split(",")]
GRID = [float(x) for x in os.environ.get("TELEPATHY_GRID", "0.15,0.3,0.45,0.6").split(",")]
NPROMPT = int(os.environ.get("TELEPATHY_NPROMPT", 4))
pats = ["*.json", "*.safetensors", "*.txt", "*.model", "merges*", "vocab*"] + (["*.bin"] if os.environ.get("TELEPATHY_ALLOW_BIN") else [])
names = list(REPOS)
(ma, ta), (mb, tb) = (T.load(snapshot_download(REPOS[n], allow_patterns=pats)) for n in names)
la = T.layer_of(ma, 0.45)
layers = {f: T.layer_of(mb, f) for f in FRACS}
assert len(set(layers.values())) == len(layers), f"two fractions land on the same layer of this receiver: {layers}"


def multi(model, tok, texts, ls):
    """one forward per text; mean over non-first tokens at every layer in ls, plus the mean token norm per layer"""
    cur, hs = {}, []
    for L in ls:
        hs.append(T.blocks(model)[L].register_forward_hook(lambda _m, _i, out, L=L: cur.__setitem__(L, (out[0] if isinstance(out, tuple) else out).detach())))
    rows, nrm = {L: [] for L in ls}, {L: [] for L in ls}
    try:
        for t in texts:
            model(input_ids=tok(t, return_tensors="pt").input_ids[:, :64].to(model.device), use_cache=False)
            for L in ls:
                x = cur[L][0].float()
                rows[L].append((x[1:].mean(0) if x.shape[0] > 1 else x[0]).cpu())
                nrm[L].append(x[1:].norm(dim=-1).mean().cpu() if x.shape[0] > 1 else x[0].norm().cpu())
    finally:
        for h in hs:
            h.remove()
    return {L: torch.stack(v) for L, v in rows.items()}, {L: float(torch.stack(v).mean()) for L, v in nrm.items()}


fit_topics = {**FIT_TOPICS_XL}
ag = json.loads((Path(T.__file__).parent / "telepathy_agentfit.json").read_text())
fit_topics.update({f"agent_{t}": s for t, s in ag.items()})
texts = [s for topic in fit_topics.values() for s in topic]
random.Random(0).shuffle(texts)
held, train = texts[:150], texts[150:]
print("sender states", len(texts), flush=True)
xa_all = T.sentence_states(ma, ta, train + held, la)
xa, ha = xa_all[:len(train)], xa_all[len(train):]
neutral_a = T.sentence_states(ma, ta, NEUTRAL, la).mean(0)
print("receiver states, layers", layers, flush=True)
xb_all, _ = multi(mb, tb, train + held, list(layers.values()))
_, norms = multi(mb, tb, NEUTRAL + PROMPTS, list(layers.values()))
neutral_b = {L: T.sentence_states(mb, tb, NEUTRAL, L).mean(0) for L in layers.values()}
dirs_a = {c: T.sentence_states(ma, ta, v["sentences"], la).mean(0) - neutral_a for c, v in ALL_CONCEPTS.items()}
lex = {c: set(v["lexicon"]) for c, v in ALL_CONCEPTS.items()}
out = {"sender": names[0], "receiver": names[1], "layers": {}}
for f, L in layers.items():
    xb, hb = xb_all[L][:len(train)], xb_all[L][len(train):]
    best = None
    for lam in (0.1, 0.3, 1.0, 3.0):
        r = T.Ridge(xa, xb, lam)
        cos = torch.nn.functional.cosine_similarity((ha - r.ma) @ r.w + r.mb - r.mb, hb - r.mb, dim=-1).mean().item()
        if best is None or cos > best[0]:
            best = (cos, lam, r)
    cos, lam, r = best
    direct = {c: T.sentence_states(mb, tb, v["sentences"], L).mean(0) - neutral_b[L] for c, v in ALL_CONCEPTS.items()}
    res = {}
    for coef in GRID:
        cnt = {"mapped": [0, 0], "direct": [0, 0]}
        for arm, dd in (("mapped", {c: r.direction(d) for c, d in dirs_a.items()}), ("direct", direct)):
            for c, d in dd.items():
                vec = (d / d.norm() * coef * norms[L]).float()
                for p in PROMPTS[:NPROMPT]:
                    pred, _ = T.lexicon_id(T.generate(mb, tb, L, vec, p, 24), lex)
                    cnt[arm][0] += pred == c
                    cnt[arm][1] += 1
        res[str(coef)] = {a: round(h / n, 4) for a, (h, n) in cnt.items()}
        print("frac", f, "layer", L, "coef", coef, res[str(coef)], "cos", round(cos, 3), flush=True)
    out["layers"][str(f)] = {"layer": L, "heldout_cosine": round(cos, 4), "lambda": lam, "norm_b": norms[L], "grid": res}
    Path(sys.argv[1]).write_text(json.dumps(out, indent=1))
print("done", flush=True)
