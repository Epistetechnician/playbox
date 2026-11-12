"""Room registry and live digests.

Each room is one `playbox-mcp` sidecar with its own PLAYBOX_HOME (or an adopted existing one). A poller keeps a
small digest per room: mode, members, unread, last message, witness holds, chain validity. The poll is gentle
because every sidecar call is appended to that room's signed history: ~8 s while someone is watching, ~45 s
when nobody is (just enough to raise a push).
"""
import json
import os
import queue
import secrets
import threading
import time
from collections import deque
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
import sys
sys.path.insert(0, str(HERE))
from playbox_ui import Sidecar, witness_state  # noqa: E402

LIVE_MS = 5 * 60 * 1000
CHAIN_EVERY = 300


class Bus:
    def __init__(self):
        self.subs = set()
        self.mu = threading.Lock()

    def subscribe(self):
        q = queue.Queue(maxsize=200)
        with self.mu:
            self.subs.add(q)
        return q

    def unsubscribe(self, q):
        with self.mu:
            self.subs.discard(q)

    def watchers(self):
        with self.mu:
            return len(self.subs)

    def publish(self, kind, data):
        with self.mu:
            for q in list(self.subs):
                try:
                    q.put_nowait((kind, data))
                except queue.Full:
                    self.subs.discard(q)


class Room:
    def __init__(self, rec, binary, hub_home):
        self.rec, self.binary = rec, binary
        self.home = rec.get("home") or str(Path(hub_home) / "rooms" / rec["id"])
        self.sidecar = None
        self.err = None
        self.digest = self.blank()
        self.seq = 0               # poller cursor in the room feed
        self.buckets = {}          # 5-minute bucket -> message count, feeds the sparkline
        self.newsays = []          # drained by the hub into the cross-room activity feed
        self.primed = False        # history that predates the hub is not "unread"
        self.last_chain = 0
        self.stop = threading.Event()
        self.mu = threading.Lock()

    @property
    def id(self):
        return self.rec["id"]

    def blank(self):
        r = self.rec
        return {"id": r["id"], "label": r["label"], "machine": r.get("machine", ""), "kind": r.get("kind", "shared"), "mode": "none", "members": 0,
                "state": "down", "unread": 0, "last": None, "holds": [], "chain": None, "err": None, "updated": 0}

    def ensure(self):
        with self.mu:
            if self.sidecar and self.sidecar.proc.poll() is None:
                return self.sidecar
            Path(self.home).mkdir(parents=True, exist_ok=True, mode=0o700)
            try:
                self.sidecar = Sidecar(self.binary, self.home)
                self.err = None
            except Exception as e:   # binary missing, handshake failed
                self.sidecar, self.err = None, f"sidecar failed to start: {e}"
            return self.sidecar

    def close(self):
        self.stop.set()
        if self.sidecar:
            self.sidecar.close()

    def tick(self, with_chain=False):
        sc = self.ensure()
        d = self.blank()
        d["updated"] = int(time.time() * 1000)
        d["unread"] = self.digest.get("unread", 0)
        d["last"] = self.digest.get("last")
        d["chain"] = self.digest.get("chain")
        if not sc:
            d["err"] = self.err
            return d
        err, st = sc.call("playbox_status", {}, wait=1)
        if err and st == "busy":
            return {**self.digest, "label": d["label"], "machine": d["machine"], "kind": d["kind"]}   # cockpit is using it; keep the last digest
        if err or not isinstance(st, dict):
            d["err"] = str(st)[:200]
            return d
        host, joined = st.get("hosting"), st.get("joined")
        d["mode"] = "both" if host and joined else "host" if host else "guest" if joined else "none"
        d["members"] = len((host or {}).get("members") or []) + (1 if joined else 0)
        d["me"] = (joined or {}).get("member_id") or st.get("guest_id")
        if d["mode"] != "none":
            ferr, feed = sc.call("playbox_feed", {"since": self.seq}, wait=1)
            if not ferr and isinstance(feed, dict):
                items = [i for i in feed.get("items", []) if isinstance(i, dict) and i.get("seq", 0) > self.seq]
                if items:
                    self.seq = max(i["seq"] for i in items)
                    says = [i for i in items if i.get("kind") == "say"]
                    for m in says:
                        ms = m.get("time_ms") or time.time() * 1000
                        b = int(ms / 300000)
                        self.buckets[b] = self.buckets.get(b, 0) + 1
                        self.newsays.append({"room": self.id, "label": self.rec["label"], "from": (m.get("from") or "")[:12], "me": m.get("from") == d["me"],
                                             "text": str(m.get("text", ""))[:200], "ms": int(ms)})
                    if self.primed:
                        d["unread"] += len([i for i in says if i.get("from") != d["me"]])
                    if says:
                        m = says[-1]
                        d["last"] = {"from": (m.get("from") or "")[:12], "text": str(m.get("text", ""))[:160], "ms": m.get("time_ms")}
            if not ferr:
                self.primed = True
            if with_chain or time.time() - self.last_chain > CHAIN_EVERY:
                path = st.get("history_log")
                if path:
                    verr, v = sc.call("playbox_verify", {"path": path}, wait=2)
                    if not verr and isinstance(v, dict):
                        d["chain"] = {"ok": v.get("valid") is True, "entries": v.get("entries")}
                        self.last_chain = time.time()
        nb = int(time.time() / 300)
        self.buckets = {k: v for k, v in self.buckets.items() if k > nb - 30}
        d["pulse"] = [self.buckets.get(nb - 23 + i, 0) for i in range(24)]
        w = witness_state(self.home)
        d["holds"] = [{"tool": e.get("tool"), "origin": e.get("origin"), "session": e.get("session")} for e in w["active"]]
        recent = d["last"] and d["last"].get("ms") and time.time() * 1000 - d["last"]["ms"] < LIVE_MS
        d["state"] = ("hold" if d["holds"] else "empty" if d["mode"] == "none" else "bad" if d["chain"] and not d["chain"]["ok"]
                      else "live" if recent else "idle")
        return d

    def seen(self):
        self.digest["unread"] = 0


class Hub:
    def __init__(self, store, binary, push_fn=None):
        self.store, self.binary, self.push_fn = store, binary, push_fn
        self.bus = Bus()
        self.rooms = {}
        self.known_attention = set()
        self.activity = deque(maxlen=80)
        self.mu = threading.RLock()
        self.wake = threading.Event()
        for rec in store.d["rooms"]:
            self._start(rec)
        threading.Thread(target=self._loop, daemon=True, name="poller").start()

    def _start(self, rec):
        r = Room(rec, self.binary, self.store.home)
        with self.mu:
            self.rooms[rec["id"]] = r
        return r

    def add(self, label, machine="", kind="shared", home=None):
        rec = {"id": secrets.token_hex(6), "label": (label or "Untitled room")[:60], "machine": (machine or "")[:40],
               "kind": "mine" if kind == "mine" else "shared", "created": time.time()}
        if home:
            hp = Path(home).expanduser().resolve()
            if not hp.is_dir():
                raise ValueError("that Playbox home does not exist")
            if any(Path(r.home).resolve() == hp for r in self.rooms.values()):
                raise ValueError("that Playbox home is already a room here")
            rec["home"] = str(hp)
        with self.store.lock:
            self.store.d["rooms"].append(rec)
            self.store.save()
        room = self._start(rec)
        self.wake.set()
        return room

    def remove(self, rid):
        with self.mu:
            room = self.rooms.pop(rid, None)
        if not room:
            return False
        room.close()
        with self.store.lock:
            self.store.d["rooms"] = [r for r in self.store.d["rooms"] if r["id"] != rid]
            self.store.save()
        self.bus.publish("gone", {"id": rid})
        return True

    def update(self, rid, **kw):
        room = self.rooms.get(rid)
        if not room:
            return None
        with self.store.lock:
            for k in ("label", "machine"):
                if isinstance(kw.get(k), str):
                    room.rec[k] = kw[k][:60]
            if kw.get("kind") in ("mine", "shared"):
                room.rec["kind"] = kw["kind"]
            self.store.save()
        room.digest = {**room.digest, "label": room.rec["label"], "machine": room.rec.get("machine", ""), "kind": room.rec["kind"]}
        self.bus.publish("room", room.digest)
        return room

    def get(self, rid):
        return self.rooms.get(rid)

    def recent(self, n=40):
        return sorted(self.activity, key=lambda a: -a["ms"])[:n]

    def digests(self):
        with self.mu:
            return [r.digest for r in sorted(self.rooms.values(), key=lambda r: r.rec["created"])]

    def attention(self):
        items = []
        for d in self.digests():
            lab = d["label"]
            for h in d["holds"]:
                items.append({"id": f"hold:{d['id']}:{h['session']}", "room": d["id"], "kind": "hold", "urgent": True,
                              "title": f"Witness paused a write in {lab}", "body": f"tool {h.get('tool') or 'write'}"})
            if d["state"] == "bad":
                items.append({"id": f"chain:{d['id']}", "room": d["id"], "kind": "chain", "urgent": True, "title": f"History check failed in {lab}",
                              "body": "the signed log no longer verifies"})
            if d["state"] == "down" and d.get("err"):
                items.append({"id": f"down:{d['id']}", "room": d["id"], "kind": "down", "urgent": False, "title": f"{lab} is unreachable", "body": d["err"][:100]})
            if d["unread"]:
                items.append({"id": f"unread:{d['id']}", "room": d["id"], "kind": "unread", "urgent": False, "count": d["unread"],
                              "title": f"{d['unread']} new in {lab}", "body": (d["last"] or {}).get("text", "")[:100]})
        return items

    def _publish_changes(self, room, new):
        old = room.digest
        room.digest = new
        strip = lambda x: {k: v for k, v in x.items() if k != "updated"}
        if strip(old) != strip(new):
            self.bus.publish("room", new)

    def _loop(self):
        while True:
            watched = self.bus.watchers() > 0
            for room in list(self.rooms.values()):
                try:
                    self._publish_changes(room, room.tick())
                    drained, room.newsays = room.newsays, []
                    for a in drained[-20:]:
                        self.activity.append(a)
                        self.bus.publish("activity", a)
                except Exception as e:
                    room.digest = {**room.blank(), "err": f"poll failed: {e}"[:200]}
            att = self.attention()
            ids = {a["id"] for a in att}
            fresh = [a for a in att if a["id"] not in self.known_attention]
            self.known_attention = ids
            if fresh:
                self.bus.publish("attention", {"items": att})
                due = [a for a in fresh if a["urgent"] or not watched]   # urgent always pushes: a backgrounded tab can still hold an open stream
                if self.push_fn and due:
                    self.push_fn(due)
            self.wake.wait(8 if watched else 45)
            self.wake.clear()

    def close(self):
        for r in list(self.rooms.values()):
            r.close()
