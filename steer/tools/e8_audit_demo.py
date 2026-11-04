"""E8 commit-and-challenge audit through a real room (docs/latent-channels.md 6h).

    python tools/e8_audit_demo.py --out results/e8/e8_audit.json

Three model families post signed hub vectors to a real `playbox` room, each with a commitment to the sentences it read:
    honest   the vector is the concept the committed sentences describe
    swap     commits to concept A's sentences, sends concept B's vector
    random   commits to concept A's sentences, sends a random vector
    silent   an honest vector whose sender never answers the challenge
Every other member challenges every message (a feed line `[challenge] seq=N`); senders reveal their sentences (the host
accepts a reveal only if it matches the signed commitment); each auditor encodes the revealed sentences through its own
model and spoke and compares with the vector that was sent. Reported: the cosine distributions, how well they separate,
and detection / false-alarm rates at a pre-set threshold and at one calibrated on this same data.
"""
import argparse
import json
import os
import random
import sys
import tempfile
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import torch

import e8_room_demo as R
from playbox_steer import hub as H
from playbox_steer import hub_run as HR
from playbox_steer import latent_audit as LA
from playbox_steer import latent_room as LRM
from playbox_steer import telepathy as T
from playbox_steer.anchors_extra import extra_anchors
from playbox_steer.telepathy_data import NEUTRAL, TEST_CONCEPTS

PRESET = 0.5


def auc(pos: list[float], neg: list[float]) -> float:
    """Probability that a random honest cosine is above a random lying one."""
    return sum((p > n) + 0.5 * (p == n) for p in pos for n in neg) / (len(pos) * len(neg))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--playbox", default=str(Path(__file__).resolve().parents[2] / "target/debug/playbox"))
    ap.add_argument("--k", type=int, default=256)
    ap.add_argument("--out")
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    concepts = list(TEST_CONCEPTS)
    rng = random.Random(0)

    train, held = HR._anchor_split()
    anchors = train + extra_anchors(16, set(train) | set(held))
    models = R.MODELS
    info = {n: HR._sender_pass(p, 0.45, anchors, held, concepts) for n, p in models.items()}
    hub = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(anchors), k=a.k, r=round(0.6 * len(anchors)))

    work = Path(tempfile.mkdtemp(prefix="e8audit_"))
    binary = Path(a.playbox)
    keys, pubs = {}, {}
    for n in models:
        (work / n).mkdir()
        keys[n], pubs[n] = LRM.new_key(work / n / "key")
    room = LRM.Room(binary, work / "host")
    members = {n: LRM.Member(binary, work / n, room.new_link(), work / n / "key") for n in models}

    # --- senders post, each with a commitment ---------------------------------------------------------------------------
    posted: dict[int, dict] = {}  # seq -> what the sender really did (kept only to score the audit)
    g = torch.Generator().manual_seed(1)

    def post(src: str, kind: str, committed: str, sent: torch.Tensor) -> None:
        salt = os.urandom(16).hex()
        sents = TEST_CONCEPTS[committed]["sentences"]
        env = H.pack_signed(hub, src, hub.encode(src, sent), keys[src], commit=H.make_commit(salt, sents))
        seq = members[src].send(env)
        posted[seq] = {"src": src, "kind": kind, "committed": committed, "salt": salt, "sentences": sents}

    for src in models:
        th = info[src]["thought"]
        for i, c in enumerate(concepts):
            post(src, "honest", c, th[c])
            post(src, "swap", c, th[concepts[(i + 1) % len(concepts)]])
        post(src, "random", concepts[0], torch.randn(info[src]["hidden"], generator=g) * th[concepts[0]].norm() / info[src]["hidden"] ** 0.5)
        post(src, "silent", concepts[1], th[concepts[1]])

    # --- auditors challenge; senders answer from the feed ---------------------------------------------------------------
    for aud in models:  # one line per auditor: the room limits a member to 20 chat lines per 10 s
        seqs = [str(seq) for seq, rec in posted.items() if rec["src"] != aud]
        members[aud].say(f"[challenge] seq={','.join(seqs)}")
    refused_bad_reveals = 0
    for src, m in members.items():
        lines, _ = m.feed(0)
        challenged = {int(x) for l in lines if l.startswith("[challenge]") for x in l.split("seq=")[1].split(",")}
        for seq in sorted(challenged):
            rec = posted[seq]
            if rec["src"] != src or rec["kind"] == "silent":
                continue
            if rec["kind"] in ("swap", "random"):
                # A liar cannot change its story: a reveal of other sentences does not match the signed commitment.
                other = TEST_CONCEPTS[rng.choice([c for c in concepts if c != rec["committed"]])]["sentences"]
                try:
                    m.reveal(seq, H.reveal_bytes(rec["salt"], other))
                except RuntimeError:
                    refused_bad_reveals += 1
            m.reveal(seq, H.reveal_bytes(rec["salt"], rec["sentences"]))  # the committed sentences are all it can reveal

    # --- auditors check ---------------------------------------------------------------------------------------------------
    trusted = set(pubs.values())
    rows = []
    for aud in models:
        mb, tb = T.load(models[aud])
        lb = info[aud]["layer"]
        neutral = T.sentence_states(mb, tb, NEUTRAL, lb)
        cache: dict[tuple, torch.Tensor] = {}

        def thought_of(sents, mb=mb, tb=tb, lb=lb, neutral=neutral):
            key = tuple(sents)
            if key not in cache:
                cache[key] = T.thought(mb, tb, lb, list(sents), neutral)
            return cache[key]

        items, cursor = [], 0
        while True:  # the host returns at most 32 messages per pull
            got, nxt = members[aud].pull(hub.hub_id, cursor)
            if not got:
                break
            items += got
            cursor = nxt
        for it in items:
            head, vec = H.unpack_signed(hub, it["envelope"], trusted_signers=trusted)
            if head["src"] == aud:
                continue
            rec = posted[it["seq"]]
            v = LA.audit(hub, aud, head, vec, it["reveal"], thought_of, threshold=PRESET)
            rows.append({"seq": it["seq"], "src": head["src"], "auditor": aud, "kind": rec["kind"], "cosine": v.cosine,
                         "revealed": it["reveal"] is not None, "ok_at_preset": v.ok, "reasons": v.reasons})
        del mb
        print(f"{aud} audited", flush=True)
    for m in members.values():
        m.close()
    room.close()

    honest = [r["cosine"] for r in rows if r["kind"] == "honest"]
    lying = [r["cosine"] for r in rows if r["kind"] in ("swap", "random")]
    silent = [r for r in rows if r["kind"] == "silent"]
    # calibrated threshold: the cut that best separates honest from lying cosines on this data (in-sample)
    cands = sorted(set(honest + lying))
    best = max(cands, key=lambda t: (sum(h >= t for h in honest) / len(honest) + sum(l < t for l in lying) / len(lying), t))

    def rates(t: float) -> dict:
        return {"threshold": round(t, 3), "honest_passed": f"{sum(h >= t for h in honest)}/{len(honest)}",
                "lying_caught": f"{sum(l < t for l in lying)}/{len(lying)}"}

    q = lambda xs, p: round(sorted(xs)[int(p * (len(xs) - 1))], 3)
    res = {
        "hub": {"id": hub.hub_id, "k": hub.k, "anchors": len(anchors), "models": list(models)},
        "audits": len(rows), "bad_reveals_refused_by_host": refused_bad_reveals,
        "cosine": {"honest": {"n": len(honest), "min": q(honest, 0), "median": q(honest, 0.5), "max": q(honest, 1)},
                   "lying": {"n": len(lying), "min": q(lying, 0), "median": q(lying, 0.5), "max": q(lying, 1)},
                   "swap_only": {"median": q([r["cosine"] for r in rows if r["kind"] == "swap"], 0.5),
                                 "max": q([r["cosine"] for r in rows if r["kind"] == "swap"], 1)},
                   "random_only": {"median": q([r["cosine"] for r in rows if r["kind"] == "random"], 0.5),
                                   "max": q([r["cosine"] for r in rows if r["kind"] == "random"], 1)}},
        "auc_honest_above_lying": round(auc(honest, lying), 4),
        "at_preset": rates(PRESET), "at_calibrated_in_sample": rates(best),
        "silent_senders": {"audits": len(silent), "flagged_no_reveal": sum(not r["ok_at_preset"] and not r["revealed"] for r in silent)},
        "rows": rows,
    }
    text = json.dumps(res, indent=1)
    if a.out:
        Path(a.out).write_text(text)
    print(json.dumps({k: v for k, v in res.items() if k != "rows"}, indent=1))


if __name__ == "__main__":
    main()
