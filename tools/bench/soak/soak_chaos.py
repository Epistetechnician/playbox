#!/usr/bin/env python3
"""Failure-injection rounds against small throwaway rooms (no model calls). One JSON line per scenario in $SOAK_DIR/chaos.jsonl.
Scenarios: grant expiry, revoke, host restart, burst of messages (loss/duplication), signed announce card, forged invite.
Run: SOAK_DIR=... SOAK_ROUNDS=24 SOAK_EVERY=1200 python3 soak_chaos.py   (PATH must start with the playbox binary under test)"""
import json, os, re, sys, tempfile, threading, time, traceback
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
import playbox_watch as w

D = os.environ.get("SOAK_DIR", os.path.expanduser("~/.playbox-soak/run"))
ROUNDS, EVERY = int(os.environ.get("SOAK_ROUNDS", "24")), int(os.environ.get("SOAK_EVERY", "1200"))
CMD = ["playbox", "mcp"]


def link_of(c):
    return c["link"]


def refused(fn, *a):
    try:
        fn(*a)
        return None
    except Exception as e:  # noqa: BLE001
        return str(e)


def room(tmp, name, ttl=None):
    h = w.Mcp(CMD, f"{tmp}/h")
    c = h.call("playbox_create", {"local": True, "name": name, "uses": 8, **({"grant_ttl_secs": ttl} if ttl else {})})
    return h, c


def guest(tmp, tag, link):
    g = w.Mcp(CMD, f"{tmp}/{tag}")
    g.call("playbox_join", {"link": link})
    return g


def expiry(tmp):
    h, c = room(tmp, "exp", ttl=4)
    g = guest(tmp, "g", link_of(c))
    g.call("playbox_say", {"text": "before", "name": "g"})
    time.sleep(6)
    e = refused(g.call, "playbox_say", {"text": "after", "name": "g"})
    ok = bool(e) and "expired" in e and "new invite link" in e
    h.close(); g.close()
    return ok, e or "post after expiry was NOT refused"


def revoke(tmp):
    h, c = room(tmp, "rev")
    g = guest(tmp, "g", link_of(c))
    g.call("playbox_say", {"text": "before", "name": "g"})
    member = h.call("playbox_status")["hosting"]["members"][0]
    h.call("playbox_revoke", {"member": member})
    e = refused(g.call, "playbox_say", {"text": "after", "name": "g"})
    h.close(); g.close()
    return e is not None, e or "post after revoke was NOT refused"


def restart(tmp):
    h, c = room(tmp, "restart")
    g = guest(tmp, "g", link_of(c))
    g.call("playbox_say", {"text": "one", "name": "g"})
    h.close()                                            # host dies
    time.sleep(1)
    h2 = w.Mcp(CMD, f"{tmp}/h")
    c2 = h2.call("playbox_create", {"local": True, "name": "restart", "uses": 4})
    g2 = guest(tmp, "g2", link_of(c2))
    g2.call("playbox_say", {"text": "two", "name": "g2"})
    texts = [i["text"] for i in h2.call("playbox_feed")["items"] if i["kind"] == "say"]
    h2.close(); g.close(); g2.close()
    return "two" in texts, f"feed after restart: {texts}"


def burst(tmp):
    h, c = room(tmp, "burst")
    gs = []
    for k in range(3):
        gs.append(guest(tmp, f"g{k}", link_of(c) if k == 0 else h.call("playbox_create", {"local": True, "name": "burst", "uses": 1})["link"]))
        time.sleep(1.5)
    sent = [f"m-{k}-{i}" for k in range(3) for i in range(20)]
    errs = []

    def run(k):
        for i in range(20):
            try:
                gs[k].call("playbox_say", {"text": f"m-{k}-{i}", "name": f"g{k}"})
            except Exception as e:  # noqa: BLE001
                errs.append(str(e))
    ts = [threading.Thread(target=run, args=(k,)) for k in range(3)]
    [t.start() for t in ts]; [t.join() for t in ts]
    got, after = [], 0
    while True:
        r = h.call("playbox_feed", {"after": after, "limit": 200}) if after else h.call("playbox_feed", {"limit": 200})
        items = r["items"]
        got += [i["text"] for i in items if i["kind"] == "say" and i["text"].startswith("m-")]
        if not items or items[-1]["seq"] <= after:
            break
        after = items[-1]["seq"]
        if len(got) >= len(sent):
            break
    h.close(); [g.close() for g in gs]
    miss, dup = set(sent) - set(got), len(got) - len(set(got))
    return not miss and not dup and not errs, f"sent {len(sent)} got {len(set(got))} missing {len(miss)} dup {dup} errors {errs[:2]}"


def announce(tmp):
    h, c = room(tmp, "ann")
    g = guest(tmp, "g", link_of(c))
    ok = refused(g.call, "playbox_announce", {"name": "soak", "role": "tester", "model": "none"}) is None
    too_long = refused(g.call, "playbox_announce", {"name": "x" * 200})
    h.close(); g.close()
    return ok, f"valid card accepted={ok}; 200-char name refused={too_long is not None} (informational)"


def forged(tmp):
    h, c = room(tmp, "forge", ttl=None)
    link = link_of(c)
    bad = link[:-6] + ("AAAAAA" if not link.endswith("AAAAAA") else "BBBBBB")
    g = w.Mcp(CMD, f"{tmp}/g")
    e = refused(g.call, "playbox_join", {"link": bad})
    g.close()
    g2 = guest(tmp, "g2", link)                          # the real link still works afterwards
    g2.call("playbox_say", {"text": "still fine", "name": "g2"})
    g2.close(); h.close()
    return e is not None, e or "tampered link was accepted"


SCEN = [expiry, revoke, restart, burst, announce, forged]


def main():
    os.makedirs(D, exist_ok=True)
    for r in range(1, ROUNDS + 1):
        t_round = time.time()
        for fn in SCEN:
            tmp = tempfile.mkdtemp(prefix="chaos")
            t0 = time.time()
            try:
                ok, detail = fn(tmp)
            except Exception as e:  # noqa: BLE001
                ok, detail = False, f"crash: {e}\n{traceback.format_exc()[-400:]}"
            rec = {"round": r, "scenario": fn.__name__, "pass": bool(ok), "detail": str(detail)[:400], "secs": round(time.time() - t0, 1), "at": time.strftime("%H:%M:%S")}
            open(os.path.join(D, "chaos.jsonl"), "a").write(json.dumps(rec) + "\n")
            print(json.dumps(rec), flush=True)
        time.sleep(max(0, EVERY - (time.time() - t_round)))


if __name__ == "__main__":
    main()
