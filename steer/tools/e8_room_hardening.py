"""E8 room hardening (docs/latent-channels.md 6l): the real `playbox` host and CLI, Python-signed envelopes, a synthetic hub.

    python tools/e8_room_hardening.py --out results/e8/e8_room_hardening.json

What runs: a real `playbox up --state DIR --latent-hub HUB` host, three `playbox latent session` members with their own
keys, and a hub fitted on synthetic model states (no language models: this checks the mailbox, not the channel's
accuracy, which sections 6a-6k measure). Checks, in order:
  1. three members post five vectors each; a forged post, a replay and a hub id the room does not list are refused;
  2. the host is stopped and started again on the same state directory; members join again with their old keys;
  3. every message is still there with the same sequence number, bytes, signer and reveal, decodes to the vector that was
     sent, a replay of an old envelope is still refused, and the next post continues the numbering;
  4. one member posting past the rate limit is told to slow down while another member still posts;
  5. a message older than the room's age limit is no longer served.
"""
import argparse
import json
import os
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import torch

from playbox_steer import hub as H
from playbox_steer import latent_room as LR

Q = 20
DIMS = {"a": 96, "b": 160, "c": 224}
POSTS_PER_MINUTE = 30


def synthetic_hub(anchor_name: str, k: int, seed: int = 0):
    g = torch.Generator().manual_seed(seed)
    lens = {m: torch.randn(Q, d, generator=g) for m, d in DIMS.items()}
    c = torch.randn(300, Q, generator=g)
    states = {m: c @ lens[m] + 0.05 * torch.randn(300, DIMS[m], generator=g) for m in DIMS}
    return H.fit_hub(states, anchor_name, k=k), lens


def refused(fn) -> str | None:
    try:
        fn()
    except RuntimeError as e:
        return str(e)[:200]
    return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--playbox", default=str(Path(__file__).resolve().parents[2] / "target/debug/playbox"))
    ap.add_argument("--out")
    a = ap.parse_args()
    torch.set_grad_enabled(False)
    binary = Path(a.playbox)

    hub, lens = synthetic_hub("anchors", Q)
    other, _ = synthetic_hub("other anchors", Q - 2)
    g = torch.Generator().manual_seed(11)
    concepts = torch.randn(5, Q, generator=g)

    work = Path(tempfile.mkdtemp(prefix="e8hard_"))
    keys, pubs = {}, {}
    for n in DIMS:
        (work / n).mkdir()
        keys[n], pubs[n] = LR.new_key(work / n / "key")
    res: dict = {"hub": hub.hub_id, "checks": {}}

    def join(room):
        return {n: LR.Member(binary, work / n, room.new_link(), work / n / "key") for n in DIMS}

    def post(members, n, vec):
        return members[n].send(H.pack_signed(hub, n, hub.encode(n, vec @ lens[n]), keys[n]))

    # 1. post, then try to cheat -------------------------------------------------------------------------------------------
    room = LR.Room(binary, work / "host", state=work / "state", extra=["--latent-hub", hub.hub_id])
    members = join(room)
    sent: dict[int, tuple[str, bytes]] = {}
    for n in DIMS:
        for c in concepts:
            env = H.pack_signed(hub, n, hub.encode(n, c @ lens[n]), keys[n])
            sent[members[n].send(env)] = (n, env)
    res["posted"] = len(sent)
    forged = H.pack_signed(hub, "b", hub.encode("b", concepts[0] @ lens["b"]), keys["b"])
    res["checks"]["forged_post_refused"] = refused(lambda: members["a"].send(forged)) is not None
    replay = sent[1][1]
    res["checks"]["replay_refused"] = refused(lambda: members["a"].send(replay)) is not None
    unlisted = H.pack_signed(other, "a", other.encode("a", concepts[0] @ lens["a"]), keys["a"])
    err = refused(lambda: members["a"].send(unlisted))
    res["checks"]["unlisted_hub_refused"] = err is not None and "does not accept" in err

    # 2. restart the host on the same state directory -----------------------------------------------------------------------
    for m in members.values():
        m.close()
    room.restart()
    members = join(room)

    # 3. everything is still there --------------------------------------------------------------------------------------
    items, nxt = members["c"].pull(hub.hub_id, 0)
    trusted = set(pubs.values())
    ok_bytes = ok_signer = ok_decode = 0
    for it in items:
        n, env = sent[it["seq"]]
        ok_bytes += it["envelope"] == env
        head, h = H.unpack_signed(hub, it["envelope"], trusted_signers=trusted)
        ok_signer += head["signer"] == pubs[n] and head["src"] == n
        ref = H.dequantize(*H.quantize(hub.encode(n, concepts[(it["seq"] - 1) % 5] @ lens[n])))
        ok_decode += bool(torch.allclose(h, ref, rtol=1e-5, atol=1e-5))
    res["after_restart"] = {"messages": len(items), "same_sequence_numbers": [i["seq"] for i in items] == sorted(sent),
                            "same_bytes": ok_bytes, "signer_verified": ok_signer, "decodes_to_what_was_sent": ok_decode,
                            "next": nxt}
    res["checks"]["replay_refused_after_restart"] = refused(lambda: members["a"].send(replay)) is not None
    fresh = post(members, "a", torch.randn(Q, generator=g))
    res["checks"]["numbering_continues"] = fresh == len(sent) + 1

    # 4. rate limit ---------------------------------------------------------------------------------------------------------
    accepted, err = 0, None
    for _ in range(POSTS_PER_MINUTE + 10):
        err = refused(lambda: post(members, "c", torch.randn(Q, generator=g)))
        if err:
            break
        accepted += 1
    res["rate_limit"] = {"accepted_before_refusal": accepted, "limit_per_minute": POSTS_PER_MINUTE,
                         "message": err, "other_member_still_posts": refused(lambda: post(members, "b", torch.randn(Q, generator=g))) is None}
    res["checks"]["rate_limited"] = bool(err) and "slow down" in err and accepted == POSTS_PER_MINUTE
    for m in members.values():
        m.close()
    room.close()

    # 5. expiry -------------------------------------------------------------------------------------------------------------
    short = LR.Room(binary, work / "host2", extra=["--latent-max-age-secs", "3"])
    m = LR.Member(binary, work / "a", short.new_link(), work / "a" / "key")
    post({"a": m}, "a", concepts[0])
    served = len(m.pull(hub.hub_id, 0)[0])
    time.sleep(4.5)
    left = len(m.pull(hub.hub_id, 0)[0])
    res["expiry"] = {"served_while_fresh": served, "served_after_age_limit": left}
    res["checks"]["expired_message_not_served"] = served == 1 and left == 0
    m.close()
    short.close()

    res["all_passed"] = all(res["checks"].values()) and res["after_restart"]["same_bytes"] == len(items) == res["posted"]
    text = json.dumps(res, indent=1)
    if a.out:
        Path(a.out).write_text(text)
    print(text)
    sys.exit(0 if res["all_passed"] else 1)


if __name__ == "__main__":
    main()
