"""E8 audit threshold transfer (docs/latent-channels.md 6m): set the threshold on one run, use it on another.

    python tools/e8_audit_transfer.py --out results/e8/e8_audit_transfer.json

6h separated honest from lying senders (AUC 0.994) but chose its cut on the same 66 numbers. This asks whether a cut chosen
on one run holds on a fresh one, with other concepts and, in the hardest case, another hub:

    calibration  hub A (as 6h: 1000 anchors, k=256), the five template concepts
    test 1       hub A again, fifteen natural concepts the calibration never saw (new sentences, new swaps, new random vectors)
    test 2       hub B, fitted on a different anchor set (the 426 base anchors, k=128), all twenty concepts
    reverse      calibrate on test 1, test on the calibration run

Per message: honest (vector of the committed concept), swap (commits to concept A's sentences, sends the next concept's
vector) and random (a random vector with the thought's norm). Every message is signed, int8-quantised through the real
`PBL2` packer, and audited by each of the other two models with `latent_audit.audit` after a reveal that opens the
commitment. The transport (room, reveal operation) was exercised in 6h and is not repeated here; what this changes is only
which threshold is used.

Thresholds compared: the best cut on labelled lying examples (a room would not have these), and three rules that need only
honest messages: the lowest honest cosine, the 5th percentile, and mean minus two standard deviations. Each is applied
globally and per (sender, auditor) pair. Model-free parts are deterministic; seeds are fixed.
"""
import argparse
import json
import math
import os
import random
import sys
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import torch
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

import e8_room_demo as R
from playbox_steer import hub as H
from playbox_steer import hub_run as HR
from playbox_steer import latent_audit as LA
from playbox_steer.anchors_extra import extra_anchors
from playbox_steer.concepts_natural import ALL_CONCEPTS, NATURAL_CONCEPTS
from playbox_steer.telepathy_data import TEST_CONCEPTS

CAL_CONCEPTS = list(TEST_CONCEPTS)
NAT_CONCEPTS = list(NATURAL_CONCEPTS)


def wilson(k: int, n: int, z: float = 1.96) -> list[float]:
    if n == 0:
        return [0.0, 1.0]
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [round(max(0.0, c - h), 3), round(min(1.0, c + h), 3)]


def auc(pos: list[float], neg: list[float]) -> float:
    return sum((p > n) + 0.5 * (p == n) for p in pos for n in neg) / (len(pos) * len(neg))


def audit_run(hub: H.Hub, info: dict, concepts: list[str], tag: str, seed: int) -> list[dict]:
    """Every message of one run, audited by the two other models. Rows carry the cosine and the truth."""
    models = list(info)
    keys = {n: Ed25519PrivateKey.generate() for n in models}
    pubs = {n: k.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex() for n, k in keys.items()}
    g = torch.Generator().manual_seed(seed)
    rng = random.Random(seed)
    posts = []
    for src in models:
        th = info[src]["thought"]
        for i, c in enumerate(concepts):
            nxt = concepts[(i + 1) % len(concepts)]
            rand = torch.randn(info[src]["hidden"], generator=g) * th[c].norm() / info[src]["hidden"] ** 0.5
            for kind, vec in (("honest", th[c]), ("swap", th[nxt]), ("random", rand)):
                posts.append({"src": src, "kind": kind, "concept": c, "vec": vec})
    rows = []
    for p in posts:
        sents = ALL_CONCEPTS[p["concept"]]["sentences"]
        salt = rng.getrandbits(128).to_bytes(16, "big").hex()
        env = H.pack_signed(hub, p["src"], hub.encode(p["src"], p["vec"]), keys[p["src"]], commit=H.make_commit(salt, sents))
        head, vec = H.unpack_signed(hub, env, trusted_signers=set(pubs.values()))
        reveal = H.reveal_bytes(salt, sents)
        for aud in models:
            if aud == p["src"]:
                continue
            # The auditor encodes the revealed sentences through its own model: the same thought the sender pass computed.
            thought_of = lambda s, aud=aud: info[aud]["by_sentences"][tuple(s)]
            v = LA.audit(hub, aud, head, vec, reveal, thought_of, threshold=-1.0)
            rows.append({"run": tag, "src": p["src"], "auditor": aud, "kind": p["kind"], "concept": p["concept"], "cosine": v.cosine})
    return rows


def rules(honest: list[float]) -> dict[str, float]:
    s = sorted(honest)
    mean = sum(s) / len(s)
    sd = (sum((x - mean) ** 2 for x in s) / len(s)) ** 0.5
    return {"honest_min": s[0], "honest_p05": s[int(0.05 * (len(s) - 1))], "honest_mean_minus_2sd": mean - 2 * sd}


def best_cut(honest: list[float], lying: list[float]) -> float:
    cands = sorted(set(honest + lying))
    return max(cands, key=lambda t: (sum(h >= t for h in honest) / len(honest) + sum(l < t for l in lying) / len(lying), t))


def apply(t: float, rows: list[dict]) -> dict:
    h = [r["cosine"] for r in rows if r["kind"] == "honest"]
    l = [r["cosine"] for r in rows if r["kind"] != "honest"]
    hp, lc = sum(x >= t for x in h), sum(x < t for x in l)
    return {"threshold": round(t, 3), "honest_passed": f"{hp}/{len(h)}", "honest_passed_ci95": wilson(hp, len(h)),
            "lying_caught": f"{lc}/{len(l)}", "lying_caught_ci95": wilson(lc, len(l))}


def per_pair(cal: list[dict], test: list[dict], rule: str) -> dict:
    """Threshold per (sender, auditor) pair from the calibration run's honest messages, applied to the test run."""
    hp = ht = lc = lt = 0
    for src in sorted({r["src"] for r in test}):
        for aud in sorted({r["auditor"] for r in test}):
            if src == aud:
                continue
            h_cal = [r["cosine"] for r in cal if r["src"] == src and r["auditor"] == aud and r["kind"] == "honest"]
            t = rules(h_cal)[rule]
            for r in test:
                if r["src"] == src and r["auditor"] == aud:
                    if r["kind"] == "honest":
                        ht += 1
                        hp += r["cosine"] >= t
                    else:
                        lt += 1
                        lc += r["cosine"] < t
    return {"honest_passed": f"{hp}/{ht}", "honest_passed_ci95": wilson(hp, ht), "lying_caught": f"{lc}/{lt}",
            "lying_caught_ci95": wilson(lc, lt)}


def transfer(cal: list[dict], test: list[dict]) -> dict:
    hc = [r["cosine"] for r in cal if r["kind"] == "honest"]
    lc = [r["cosine"] for r in cal if r["kind"] != "honest"]
    ht = [r["cosine"] for r in test if r["kind"] == "honest"]
    lt = [r["cosine"] for r in test if r["kind"] != "honest"]
    q = lambda xs, p: round(sorted(xs)[int(p * (len(xs) - 1))], 3)
    out = {
        "cal": {"honest": {"n": len(hc), "min": q(hc, 0), "median": q(hc, 0.5), "max": q(hc, 1)},
                "lying": {"n": len(lc), "median": q(lc, 0.5), "max": q(lc, 1)}, "auc": round(auc(hc, lc), 4)},
        "test": {"honest": {"n": len(ht), "min": q(ht, 0), "median": q(ht, 0.5), "max": q(ht, 1)},
                 "lying": {"n": len(lt), "median": q(lt, 0.5), "max": q(lt, 1)}, "auc": round(auc(ht, lt), 4)},
        "oracle_on_test": apply(best_cut(ht, lt), test),  # the best cut on the test data itself: an upper bound
        "from_calibration": {"best_cut_with_liar_labels": apply(best_cut(hc, lc), test)},
        "per_pair_from_calibration": {},
    }
    for name, t in rules(hc).items():
        out["from_calibration"][name] = apply(t, test)
        out["per_pair_from_calibration"][name] = per_pair(cal, test, name)
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    train, held = HR._anchor_split()
    anchors = train + extra_anchors(16, set(train) | set(held))
    print(f"reading {len(anchors)} anchors and {len(ALL_CONCEPTS)} concepts with {len(R.MODELS)} models", flush=True)
    info = {n: HR._sender_pass(p, 0.45, anchors, held, list(ALL_CONCEPTS), bank=ALL_CONCEPTS) for n, p in R.MODELS.items()}
    # What each auditor model computes for a revealed sentence set: the thought the sender pass already built.
    for n, i in info.items():
        i["by_sentences"] = {tuple(ALL_CONCEPTS[c]["sentences"]): i["thought"][c] for c in ALL_CONCEPTS}

    hub_a = H.fit_hub({n: i["train"] for n, i in info.items()}, H.anchor_digest(anchors), k=256, r=round(0.6 * len(anchors)))
    nb = len(train)
    hub_b = H.fit_hub({n: i["train"][:nb] for n, i in info.items()}, H.anchor_digest(train), k=128, r=round(0.6 * nb))
    print(f"hubs: A {hub_a.hub_id} ({len(anchors)} anchors, k={hub_a.k}); B {hub_b.hub_id} ({nb} anchors, k={hub_b.k})", flush=True)

    cal = audit_run(hub_a, info, CAL_CONCEPTS, "A/template", seed=1)
    t1 = audit_run(hub_a, info, NAT_CONCEPTS, "A/natural", seed=2)
    t2 = audit_run(hub_b, info, list(ALL_CONCEPTS), "B/all", seed=3)
    res = {
        "setup": {"models": list(R.MODELS), "hub_a": {"id": hub_a.hub_id, "anchors": len(anchors), "k": hub_a.k},
                  "hub_b": {"id": hub_b.hub_id, "anchors": nb, "k": hub_b.k}, "calibration_concepts": CAL_CONCEPTS,
                  "natural_concepts": NAT_CONCEPTS, "audits": {"cal": len(cal), "test1": len(t1), "test2": len(t2)}},
        "calibrate_on_A_template__test_on_A_natural": transfer(cal, t1),
        "calibrate_on_A_template__test_on_hub_B": transfer(cal, t2),
        "calibrate_on_A_natural__test_on_A_template": transfer(t1, cal),
        "rows": cal + t1 + t2,
    }
    text = json.dumps(res, indent=1)
    if a.out:
        Path(a.out).write_text(text)
    print(json.dumps({k: v for k, v in res.items() if k != "rows"}, indent=1))


if __name__ == "__main__":
    main()
