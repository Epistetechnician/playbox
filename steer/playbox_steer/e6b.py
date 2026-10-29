"""E6, second round: answers the first round's four limits.

  * behaviour, not just the next token: B's greedy continuation is scored with a word lexicon, as well as the
    next-token log-mass;
  * meaningful nulls: besides random directions, directions for unrelated contrasts (formality, tense, topic,
    question versus statement) at the same layer and strength;
  * more setups: several steering layers and three queries per trait;
  * the audit defence (cache_audit.py) is evaluated by `python -m playbox_steer.audit_e6`.

    python -m playbox_steer.e6b --weights DIR --out report.json [--layers 4,6,8] [--coefs 0.25,0.5]
"""
from __future__ import annotations

import argparse
import copy
import json
import re
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import torch

from . import engine, latent
from .e6 import FRAMES, NEGATIVE, POSITIVE, SUBJECTS, TRAITS, contexts

QUERIES = {
    "sentiment": [" Honestly, I think it was", " My review: the experience was", " In short, it felt"],
    "fear": [" Right now I feel", " Walking in, I was", " The mood in the room was"],
}
LEXICON = {
    "sentiment": ("good great wonderful excellent amazing fantastic lovely love loved best happy enjoy enjoyed beautiful delightful perfect nice".split(),
                  "bad terrible awful horrible poor disappointing dreadful hate hated worst sad angry boring ugly nasty painful".split()),
    "fear": ("afraid scared terrified anxious nervous worried frightened uneasy fear fearful panic dread terrifying scary danger dangerous threat horror".split(),
             "calm safe relaxed comfortable peaceful fine happy secure steady easy gentle quiet serene".split()),
}
# Unrelated contrasts, used as meaningful nulls. Each is 10 sentences against 10.
SEMANTIC = {
    "formality": (
        ["We regret to inform you that the proposal has been declined.", "Kindly submit the required documentation at your earliest convenience.", "The committee shall convene on Thursday.",
         "It is imperative that all parties comply with the regulations.", "I would be most grateful for your prompt reply.", "Pursuant to our agreement, payment is due within thirty days.",
         "Please be advised that the office will remain closed.", "The undersigned hereby acknowledges receipt of the goods.", "We respectfully request an extension of the deadline.", "Attendance at the ceremony is mandatory."],
        ["yeah no that's not happening lol", "hey can u send me that stuff when u get a sec", "we're gonna hang out on thursday", "just gotta follow the rules i guess", "thx so much, text me back soon",
         "ok so you pay me within a month, deal?", "heads up, the office is closed", "got the stuff, all good", "can we push the deadline a bit pls", "come to the party, it'll be fun"]),
    "tense": (
        ["Yesterday we walked to the river and talked for hours.", "She finished the report last week.", "They lived in that house for ten years.", "He opened the door and stepped outside.",
         "We watched the game and went home.", "The train left an hour ago.", "I forgot my keys again.", "She wrote a letter to her friend.", "The team won the title that year.", "He remembered the old song."],
        ["Tomorrow we will walk to the river and talk for hours.", "She will finish the report next week.", "They will live in that house for ten years.", "He will open the door and step outside.",
         "We will watch the game and go home.", "The train will leave in an hour.", "I will forget my keys again.", "She will write a letter to her friend.", "The team will win the title next year.", "He will remember the old song."]),
    "topic": (
        ["The striker scored twice before halftime.", "The coach changed the lineup for the final.", "She ran the marathon in under three hours.", "The goalkeeper saved the penalty.",
         "He trained every morning for the tournament.", "The crowd cheered as the team took the field.", "They lost the match in overtime.", "The swimmer broke the national record.", "Fans filled the stadium for the derby.", "The referee blew the whistle."],
        ["Simmer the sauce gently and add the garlic.", "The recipe calls for two cups of flour.", "She chopped the onions and stirred the soup.", "Bake the bread until it is golden.",
         "He seasoned the chicken with rosemary.", "Whisk the eggs and fold in the butter.", "The stew needs another hour on low heat.", "Roast the vegetables with olive oil.", "She tasted the sauce and added salt.", "Knead the dough for ten minutes."]),
    "question": (
        ["Where did you put the keys?", "What time does the meeting start?", "Why is the sky that color today?", "How long have they lived here?", "Who is coming to the party?",
         "Which road leads to the station?", "When will the package arrive?", "Is the store open on Sundays?", "Do you know the way to the museum?", "Can anyone explain how this works?"],
        ["I put the keys on the table.", "The meeting starts at noon.", "The sky is orange from the smoke.", "They have lived here for years.", "Everyone is coming to the party.",
         "The left road leads to the station.", "The package will arrive on Friday.", "The store is open on Sundays.", "The museum is down the street.", "This works by pulling the lever."]),
}


def words(text: str) -> list[str]:
    return re.findall(r"[a-z]+", text.lower())


def lexicon_score(text: str, trait: str) -> float:
    pos, neg = LEXICON[trait]
    w = words(text)
    if not w:
        return 0.0
    return (sum(x in pos for x in w) - sum(x in neg for x in w)) / len(w)


@dataclass
class Result:
    trait_logit: float
    gen: float
    logits: torch.Tensor


def b_reads(model, tok, past, query: str, trait: str, pos_ids, neg_ids, gen_tokens: int) -> Result:
    """B reads the query on a private copy of the cache, then continues greedily for `gen_tokens` tokens."""
    ids = tok(query, return_tensors="pt").input_ids
    out = model(input_ids=ids, past_key_values=copy.deepcopy(past), use_cache=True)
    logits, cache = out.logits[0, -1], out.past_key_values
    lp = torch.log_softmax(logits, -1)
    tl = float(torch.logsumexp(lp[pos_ids], 0) - torch.logsumexp(lp[neg_ids], 0))
    gen_ids = []
    cur = logits
    for _ in range(gen_tokens):
        nxt = int(torch.argmax(cur))
        gen_ids.append(nxt)
        if tok.eos_token_id is not None and nxt == tok.eos_token_id:
            break
        step = model(input_ids=torch.tensor([[nxt]]), past_key_values=cache, use_cache=True)
        cache, cur = step.past_key_values, step.logits[0, -1]
    return Result(tl, lexicon_score(tok.decode(gen_ids, skip_special_tokens=True), trait), logits.clone())


def ci(x, n=2000, seed=0):
    x = np.asarray(x, dtype=float)
    rng = np.random.default_rng(seed)
    means = np.array([x[rng.integers(0, len(x), len(x))].mean() for _ in range(n)])
    return {"mean": float(x.mean()), "low": float(np.percentile(means, 2.5)), "high": float(np.percentile(means, 97.5))}


def run_cell(model, tok, ctxs, trait: str, trait_dir, rand_dirs, sem_dirs: dict, layer: int, coef: float, gen_tokens: int) -> dict:
    t = TRAITS[trait]
    pos_ids, neg_ids = latent.word_ids(tok, t["pos_words"]), latent.word_ids(tok, t["neg_words"])
    spec = latent._spec(layer, coef, "prompt")
    norm_ref = engine.calibrate(model, tok, spec, SimpleNamespace(prompts=tuple(ctxs)))
    queries = QUERIES[trait]
    arms = {"clean": None, "steered": trait_dir, **{f"rand_{k}": d for k, d in enumerate(rand_dirs)}, **{f"sem_{n}": d for n, d in sem_dirs.items()}}
    res: dict[str, list[Result]] = {a: [] for a in arms}  # one Result per (context, query), same order in every arm
    for c in ctxs:
        for a, d in arms.items():
            if d is None:
                past = latent.read_context(model, tok, c, None)
            else:
                with engine.Steerer(model, spec, d, norm_ref):
                    past = latent.read_context(model, tok, c, None)
            for q in queries:
                res[a].append(b_reads(model, tok, past, q, trait, pos_ids, neg_ids, gen_tokens))
    return summarize(res)


def summarize(res: dict[str, list[Result]]) -> dict:
    """Effects and nulls from per-arm results (arms: clean, steered, rand_*, sem_*; same order in every arm)."""
    arms = list(res)
    clean = res["clean"]

    def eff(a, metric):
        return np.array([getattr(h, metric) - getattr(c, metric) for h, c in zip(res[a], clean)])

    def kl_of(a):
        return float(np.mean([latent.kl(h.logits, c.logits) for h, c in zip(res[a], clean)]))

    out = {}
    for metric in ("trait_logit", "gen"):
        st = eff("steered", metric)
        rand = [eff(a, metric) for a in arms if a.startswith("rand_")]
        sem = {a[4:]: eff(a, metric) for a in arms if a.startswith("sem_")}
        out[metric] = {
            "effect": ci(st),
            "specific_vs_random": ci(st - np.mean(rand, axis=0)),
            "specific_vs_semantic": ci(st - np.mean(list(sem.values()), axis=0)),
            "random_as_far": float(np.mean([r.mean() >= st.mean() for r in rand])),
            "semantic_effects": {n: float(v.mean()) for n, v in sem.items()},
            "semantic_as_far": float(np.mean([v.mean() >= st.mean() for v in sem.values()])),
        }
    out["kl_steered"] = kl_of("steered")
    out["kl_random_mean"] = float(np.mean([kl_of(a) for a in arms if a.startswith("rand_")]))
    out["kl_semantic_mean"] = float(np.mean([kl_of(a) for a in arms if a.startswith("sem_")]))
    out["n"] = len(clean)
    return out




def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--weights", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--layers", default="4,6,8")
    ap.add_argument("--coefs", default="0.25,0.5")
    ap.add_argument("--nulls", type=int, default=16)
    ap.add_argument("--contexts", type=int, default=32)
    ap.add_argument("--gen-tokens", type=int, default=20)
    ap.add_argument("--traits", default="sentiment,fear")
    a = ap.parse_args(argv)
    w = Path(a.weights)
    model, tok = engine.load(w)
    dim = model.config.hidden_size
    out = {"model": str(w), "n_layers": len(engine.blocks(model)), "layers": {}}
    for layer in [int(x) for x in a.layers.split(",")]:
        rand = [torch.tensor(engine.random_direction(dim, 100 + k)) for k in range(a.nulls)]
        sem = {n: torch.tensor(engine.extract_diffmean(w, p, q, "resid_post", layer)) for n, (p, q) in SEMANTIC.items()}
        out["layers"][str(layer)] = {}
        for trait in a.traits.split(","):
            t = TRAITS[trait]
            d = torch.tensor(engine.extract_diffmean(w, t["positive"], t["negative"], "resid_post", layer))
            out["layers"][str(layer)][trait] = {}
            for c in [float(x) for x in a.coefs.split(",")]:
                r = run_cell(model, tok, contexts(a.contexts), trait, d, rand, sem, layer, c, a.gen_tokens)
                out["layers"][str(layer)][trait][str(c)] = r
                g, n = r["gen"], r["trait_logit"]
                print(f"L{layer:<2} {trait:9} {c:>4}: next-token {n['effect']['mean']:+.2f} (vs sem-null {n['specific_vs_semantic']['mean']:+.2f}, sem as far {n['semantic_as_far']:.2f}) | "
                      f"generation {g['effect']['mean']:+.4f} [{g['effect']['low']:+.4f},{g['effect']['high']:+.4f}] (vs sem-null {g['specific_vs_semantic']['mean']:+.4f}, sem as far {g['semantic_as_far']:.2f}, rand as far {g['random_as_far']:.2f}) | "
                      f"KL {r['kl_steered']:.3f} (rand {r['kl_random_mean']:.3f}, sem {r['kl_semantic_mean']:.3f})", flush=True)
        Path(a.out).write_text(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
