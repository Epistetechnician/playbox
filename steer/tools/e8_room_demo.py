"""E8 end to end (docs/latent-channels.md 6g): models of different families exchange concepts through a real room.

    python tools/e8_room_demo.py --out results/e8/e8_room.json

What runs: a real `playbox up` host; one `playbox latent session` member per model, each with its own identity key;
a hub fitted on the shared anchors. Every sender model reads a concept, encodes it into the hub, signs a PBL2 envelope and
posts it. Every other model pulls from the room, verifies the signature against the sender's key, decodes with its own
spoke, runs the guard check, applies the vector at 0.5 receiver-norm strength and generates; the text is scored with the
lexicon decoder used in the other E8 runs. The concept label never travels: it is kept by this script only to score.

Also checked: the host refuses an envelope signed by someone else; a guarded concept (medicine, for every receiver) is
blocked by the receiver's guard; and the vector that came through the room gives the same text as the in-process path.
Models are small and local (CPU). Ceiling: local_internal_exploratory.
"""
import argparse
import json
import os
import sys
import tempfile
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import torch

from playbox_steer import hub as H
from playbox_steer import hub_run as HR
from playbox_steer import latent_room as LR
from playbox_steer import telepathy as T
from playbox_steer.anchors_extra import extra_anchors
from playbox_steer.telepathy_data import NEUTRAL, PROMPTS, TEST_CONCEPTS

HUB = Path.home() / ".cache/huggingface/hub"


def _snapshot(repo: str) -> str:
    """The cached snapshot directory of a Hugging Face repo (loading by directory works offline)."""
    return str(sorted((HUB / f"models--{repo.replace('/', '--')}" / "snapshots").iterdir())[-1])


MODELS = {"smollm2": _snapshot("HuggingFaceTB/SmolLM2-135M"), "gpt2": _snapshot("openai-community/gpt2"),
          "pythia": _snapshot("EleutherAI/pythia-410m")}
GUARDED = "medicine"
COEF = 0.5


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--playbox", default=str(Path(__file__).resolve().parents[2] / "target/debug/playbox"))
    ap.add_argument("--k", type=int, default=256)
    ap.add_argument("--gen", type=int, default=24)
    ap.add_argument("--out")
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    concepts = list(TEST_CONCEPTS)
    lex = {c: set(TEST_CONCEPTS[c]["lexicon"]) for c in concepts}

    train, held = HR._anchor_split()
    anchors = train + extra_anchors(16, set(train) | set(held))
    print(f"reading {len(anchors)} anchors with {len(MODELS)} models", flush=True)
    info = {n: HR._sender_pass(p, 0.45, anchors, held, concepts) for n, p in MODELS.items()}
    hub = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(anchors), k=a.k, r=round(0.6 * len(anchors)))

    work = Path(tempfile.mkdtemp(prefix="e8room_"))
    binary = Path(a.playbox)
    keys, pubs = {}, {}
    for n in MODELS:
        (work / n).mkdir()
        keys[n], pubs[n] = LR.new_key(work / n / "key")
    room = LR.Room(binary, work / "host")
    members = {n: LR.Member(binary, work / n, room.new_link(), work / n / "key") for n in MODELS}
    res: dict = {"hub": {"id": hub.hub_id, "k": hub.k, "anchors": len(anchors), "models": list(MODELS)}, "messages": [],
                 "chance": 1 / len(concepts)}

    # --- senders post ---------------------------------------------------------------------------------------------------
    truth: dict[tuple[str, int], str] = {}  # only for scoring; never sent
    sizes = set()
    for src in MODELS:
        for c in concepts:
            env = H.pack_signed(hub, src, hub.encode(src, info[src]["thought"][c]), keys[src])
            sizes.add(len(env))
            truth[(src, members[src].send(env))] = c
    res["envelope_bytes"] = sorted(sizes)

    # --- a forged post: signed by pythia's key, sent by smollm2's member -------------------------------------------------
    forged = H.pack_signed(hub, "pythia", hub.encode("pythia", info["pythia"]["thought"]["ocean"]), keys["pythia"])
    try:
        members["smollm2"].send(forged)
        res["forged_post_refused"] = False
    except RuntimeError as e:
        res["forged_post_refused"] = True
        res["forged_post_error"] = str(e)[:160]

    # --- receivers pull, verify, guard, decode, generate ----------------------------------------------------------------
    trusted = set(pubs.values())
    for dst in MODELS:
        mb, tb = T.load(MODELS[dst])
        di = info[dst]
        lb = di["layer"]
        neutral = T.sentence_states(mb, tb, NEUTRAL, lb)
        direct = {c: T.thought(mb, tb, lb, TEST_CONCEPTS[c]["sentences"], neutral) for c in concepts}
        guards = {f"{GUARDED}-direction": (direct[GUARDED], GUARDED)}
        items, _ = members[dst].pull(hub.hub_id, 0)
        for it in items:
            head, h = H.unpack_signed(hub, it["envelope"], trusted_signers=trusted)
            src = head["src"]
            if src == dst or pubs[src] != head["signer"]:
                continue  # own message, or a sender label that does not belong to the key that signed it
            concept = truth[(src, it["seq"])]
            vec = hub.decode(dst, h)
            blocked = LR.guard_blocks(vec, guards, {GUARDED})
            v = vec / vec.norm() * COEF * di["norm"]
            ref = hub.send(src, dst, info[src]["thought"][concept])
            ref = ref / ref.norm() * COEF * di["norm"]
            outs, same = [], 0
            for p in PROMPTS:
                t = T.generate(mb, tb, lb, v, p, a.gen)
                outs.append(t)
                same += t == T.generate(mb, tb, lb, ref, p, a.gen)
            hits = [T.lexicon_id(t, lex)[0] == concept for t in outs]
            dv = direct[concept] / direct[concept].norm() * COEF * di["norm"]
            direct_hits = [T.lexicon_id(T.generate(mb, tb, lb, dv, p, a.gen), lex)[0] == concept for p in PROMPTS]
            res["messages"].append({"src": src, "dst": dst, "concept": concept, "seq": it["seq"], "hits": sum(hits),
                                    "n": len(hits), "direct_hits": sum(direct_hits), "blocked_by_guard": blocked,
                                    "same_text_as_in_process": same, "example": outs[0][:120]})
        del mb
        print(f"{dst} done: {sum(1 for m in res['messages'] if m['dst'] == dst)} messages", flush=True)

    feed, _ = members["gpt2"].feed(0)
    res["feed_notices"] = [line for line in feed if line.startswith("[latent]")]
    for m in members.values():
        m.close()
    room.close()

    ms = res["messages"]
    allr = ms
    ok = [m for m in ms if not m["blocked_by_guard"]]
    res["summary"] = {
        "messages": len(ms),
        "accuracy_all_ungated": round(sum(m["hits"] for m in allr) / sum(m["n"] for m in allr), 4),
        "accuracy_direct_upper_bound": round(sum(m["direct_hits"] for m in allr) / sum(m["n"] for m in allr), 4),
        "delivered_after_guard": len(ok),
        "blocked_by_guard": sorted({m["concept"] for m in ms if m["blocked_by_guard"]}),
        "blocked_count": len(ms) - len(ok),
        "accuracy_delivered": round(sum(m["hits"] for m in ok) / sum(m["n"] for m in ok), 4),
        "same_text_as_in_process": f'{sum(m["same_text_as_in_process"] for m in ms)}/{sum(m["n"] for m in ms)}',
    }
    text = json.dumps(res, indent=1)
    if a.out:
        Path(a.out).write_text(text)
    print(json.dumps(res["summary"], indent=1), "\nforged refused:", res["forged_post_refused"], "\nnotices:", len(res["feed_notices"]))


if __name__ == "__main__":
    main()
