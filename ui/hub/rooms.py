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
import sys
import threading
import time
from collections import deque
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))
from playbox_ui import Sidecar, witness_state  # noqa: E402
from .local import ask_host, hosting_dir, prune_stale  # noqa: E402
from .archive import Archive  # noqa: E402
from . import board, card as idcard  # noqa: E402


def pretty_pbx(text):
    """Turn a `::pbx <kind> {json}` feed message into a human-readable one-liner for room digests and activity."""
    s = str(text or "")
    if not s.startswith("::pbx "):
        return s
    rest = s[6:]
    kind, _, body = rest.partition(" ")
    if kind not in ("metric", "artifact", "card", "comment", "agent") or not body:
        return s
    try:
        d = json.loads(body)
        if not isinstance(d, dict):
            return s
    except ValueError:
        return s
    if kind == "metric":
        unit = f" {d['unit']}" if d.get("unit") else ""
        return f"metric · {d.get('name') or 'value'} = {d.get('value', '')}{unit}"
    if kind == "artifact":
        st = f" ({d['status']})" if d.get("status") else ""
        sm = f" — {d['summary']}" if d.get("summary") else ""
        return f"artifact · {d.get('title') or d.get('id') or 'item'}{st}{sm}"
    if kind == "card":
        col = f" [{d['col']}]" if d.get("col") else ""
        return f"card · {d.get('title') or d.get('id') or 'task'}{col}"
    if kind == "comment":
        return f"comment · {d.get('text') or ''}"
    return f"agent · {d.get('name') or 'intro'}"


def event_note(text, who, meta=None):
    """Turn one room event into a (stage, sentence, level) for the System view. Ids are shortened; nothing secret is in an event."""
    who = (who or "")[:6]
    t = text.lower()
    m = meta if isinstance(meta, dict) else {}
    ev = str(m.get("event") or "").lower()
    if ev == "join" or t == "join ok" or t == "join failed":
        ok = m.get("ok") if "ok" in m else (t == "join ok")
        if ok:
            return ("join", f"Member {who} joined with a bound grant" if who else "A member joined", "ok")
        return ("join", f"A join by {who} failed" if who else "A join failed", "warn")
    if ev == "refused" or t.startswith("refused"):
        why = str(m.get("reason") or text[7:]).strip()[:100]
        return ("join", "Refused: " + (why or "request denied"), "warn")
    if ev == "host_start" or t == "host_start":
        bits = [str(m[k]) for k in ("runner", "isolation", "transport") if m.get(k)]
        tail = f" ({' · '.join(bits)})" if bits else ""
        return ("history", f"Room host started{tail}; the signed log continues", "info")
    if ev == "task" or t.startswith("task "):
        ok = m.get("ok") if "ok" in m else t.startswith("task ok")
        bits = []
        if m.get("runner"):
            bits.append(str(m["runner"]))
        if m.get("program"):
            bits.append(str(m["program"]))
        if m.get("exit_code") is not None:
            bits.append(f"exit {m['exit_code']}")
        if m.get("ms") is not None:
            bits.append(f"{m['ms']}ms")
        if m.get("timed_out"):
            bits.append("timed out")
        desc = " · ".join(bits) if bits else text[:80]
        who_s = f" by {who}" if who else ""
        return ("history", f"Task {'ok' if ok else 'failed'}{who_s} ({desc})", "ok" if ok else "warn")
    if ev == "replay":
        rep = m.get("reproducible", m.get("ok"))
        tid = str(m.get("task_id") or "")[:12]
        return ("history", f"Replay of {tid or 'task'}: {'reproducible ✓' if rep else 'diverged ✕'}", "ok" if rep else "warn")
    if ev == "handoff_checkpoint":
        br = str(m.get("branch") or "branch")[:40]
        return ("feed", f"Handoff checkpoint published on {br} by {who or 'member'}", "ok")
    if ev == "policy_update":
        pd = str(m.get("policy_digest") or "")[:12]
        return ("approve", f"Live room policy guardrails updated ({pd or 'new digest'})", "info")
    if ev == "remember":
        taint = str(m.get("taint") or "untrusted")
        return ("feed", f"Room memory entry added by {who or 'member'} ({taint})", "ok" if taint == "owner_endorsed" else "info")
    return ("history", text[:100] or "room event", "info")

LIVE_MS = 5 * 60 * 1000
CHAIN_EVERY = 300
DOWN_URGENT_S = float(os.environ.get("PLAYBOX_DOWN_ALERT_S", "300"))
PRUNE_EVERY = 600


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
        self.host_id = None        # the room's host id: signed identity cards are bound to it
        self.idalerts = {}         # key -> name: an endorsed agent whose card changed since you endorsed it
        self.newcomers = {}        # key -> when: joined a room that has endorsed agents
        self.buckets = {}          # 5-minute bucket -> message count, feeds the sparkline
        self.newsays = []          # drained by the hub into the cross-room activity feed
        self.primed = False        # history that predates the hub is not "unread"
        self.last_chain = 0
        self.last_renew = 0
        self.last_rehost = 0
        self.stop = threading.Event()
        self.mu = threading.Lock()
        self.on_change = None      # set by the hub: persist rec changes
        self.on_needs_invite = None   # set by the hub: called (off the poller's critical path) when this joined room lost its access
        self.tick_mu = threading.RLock()
        self.notes = []            # (stage, text, level) the poller turns into security events
        self.hold_ids = set()
        self.archive = Archive(self.home)      # what this hub has seen of the feed, kept on disk: it outlives the host and the host's chat window
        self.archive_err = None
        self.task_stats = {"total": 0, "ok": 0, "fail": 0, "ms_sum": 0, "ms_n": 0, "runner": ""}
        self._task_primed = False
        self._gone, self._gone_at = False, 0.0

    def host_gone(self):
        """True when this room was adopted from a host on this machine and that host no longer answers on its control socket. Cached for 2 s.
        Calling a dead host through the sidecar can block for a long time (and holds the sidecar for everyone else): never do it when this says no."""
        hid = self.rec.get("adopted_host")
        if not hid:
            return False
        now = time.time()
        if now - self._gone_at < 2.0:
            return self._gone
        try:
            self._gone = ask_host(hosting_dir() / f"{hid}.sock", "ping", 1.0) != "ok"
        except OSError:
            self._gone = True
        self._gone_at = now
        return self._gone

    def gone_digest(self, d):
        d["err"] = "The room's host is not running any more."
        d["mode"] = d.get("mode") if d.get("mode") not in (None, "none") else "guest"
        d["archived"] = self.archive.info()["items"]
        d["tasks"] = self._task_digest()
        d["offline"] = bool(d["archived"])
        d["state"] = "idle"
        return d

    def keep(self, feed, me=None):
        """Store a feed read in the archive. A failing disk must never break the poller: say so once, carry on."""
        try:
            self.archive.append([i for i in feed.get("items", []) if isinstance(i, dict)], feed.get("members") if isinstance(feed.get("members"), list) else None, me)
            self.archive_err = None
        except OSError as e:
            if self.archive_err is None:
                self.notes.append(("history", f"Could not save this room's history on disk: {e}", "bad"))
            self.archive_err = str(e)

    @property
    def id(self):
        return self.rec["id"]

    def _ingest_task_stats(self, items):
        ts = self.task_stats
        for i in items:
            if not isinstance(i, dict) or i.get("kind") != "event":
                continue
            m = i.get("meta") if isinstance(i.get("meta"), dict) else {}
            ev = str(m.get("event") or "").lower()
            txt = str(i.get("text") or "").lower()
            if ev == "host_start" and m.get("runner"):
                ts["runner"] = str(m["runner"])[:32]
                continue
            if ev == "task" or txt.startswith("task "):
                ts["total"] += 1
                ok = m.get("ok") if "ok" in m else txt.startswith("task ok")
                if ok:
                    ts["ok"] += 1
                else:
                    ts["fail"] += 1
                if m.get("runner"):
                    ts["runner"] = str(m["runner"])[:32]
                dur = m.get("ms")
                if isinstance(dur, (int, float)) and not isinstance(dur, bool) and dur >= 0:
                    ts["ms_sum"] += dur
                    ts["ms_n"] += 1

    def _task_digest(self):
        if not getattr(self, "_task_primed", False) and hasattr(self, "archive"):
            self._task_primed = True
            try:
                self._ingest_task_stats(self.archive.tail(500))
            except (OSError, ValueError, KeyError, TypeError):
                pass
        ts = getattr(self, "task_stats", {"total": 0, "ok": 0, "fail": 0, "ms_sum": 0, "ms_n": 0, "runner": ""})
        return {
            "total": ts["total"],
            "ok": ts["ok"],
            "fail": ts["fail"],
            "avg_ms": int(ts["ms_sum"] / ts["ms_n"]) if ts["ms_n"] else None,
            "runner": ts["runner"],
        }

    def blank(self):
        r = self.rec
        return {"id": r["id"], "label": r["label"], "machine": r.get("machine", ""), "kind": r.get("kind", "shared"), "mode": "none", "members": 0,
                "state": "down", "unread": 0, "last": None, "holds": [], "chain": None, "err": None, "updated": 0, "tasks": self._task_digest()}

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
            if self.sidecar and self.rec.get("host") is not None:
                # hosting lives in the sidecar's memory: a restarted hub (or a respawned sidecar) must host again or invites stop working.
                # A room with no repository (talk and notes only) is hosted too: `host` is then just {}.
                self._host_again()
            elif self.sidecar and self.rec.get("adopted_host"):
                self._rejoin()          # joins live in the sidecar's memory too: an adopted room joins its host again after a restart
            return self.sidecar

    def _host_again(self):
        """Host the room again in a fresh sidecar. The result matters: a refusal used to be dropped, leaving a room that looked up but hosted nothing."""
        h = self.rec.get("host")
        if h is None or not self.sidecar:
            return False
        self.last_rehost = time.time()
        args = {**({"repo": h["repo"]} if h.get("repo") else {}), "local": bool(h.get("local")), "docker": bool(h.get("docker"))}
        for k in ("template", "runtime", "cpus", "mem_mib", "timeout_secs", "network", "allowed_hosts", "allowed_secrets",
                  "allowed_egress", "task_budget", "max_members", "max_concurrent", "max_tasks_per_minute", "memory_review"):
            if h.get(k) is not None:
                args[k] = h[k]
        try:
            err, data = self.sidecar.call("playbox_create", args, wait=20)
        except Exception as e:  # noqa: BLE001
            self.err = f"could not host again: {e}"
            return False
        if err:
            self.err = f"could not host again: {str(data)[:160]}"
            return False
        self.err = None
        return True

    def _rejoin(self):
        """Join the local host that this room was adopted from again, with a fresh link. Grants last an hour; a restart loses the join entirely."""
        hid = self.rec.get("adopted_host")
        if not hid or not self.sidecar or time.time() - self.last_renew < 30:
            return False
        self.last_renew = time.time()
        try:
            link = ask_host(hosting_dir() / f"{hid}.sock", "link")
        except OSError as e:
            self.err = f"the room's host is not answering: {e}"
            return False
        if not link.startswith("playbox://"):
            self.err = link[:160] or "the room's host would not make a link"
            return False
        try:
            self.sidecar.call("playbox_leave", {})          # an error here ("not in a room") is fine
            err, data = self.sidecar.call("playbox_join", {"link": link})
        except Exception as e:  # noqa: BLE001
            self.err = f"could not join again: {e}"
            return False
        if err:
            self.err = f"could not join again: {str(data)[:160]}"
            return False
        self.err = None
        self.notes.append(("join", "Joined the room again with a fresh grant", "ok"))
        return True

    def close(self):
        self.stop.set()
        if self.sidecar:
            self.sidecar.close()

    def tick(self, with_chain=False):
        with self.tick_mu:        # the poller and a 'verify now' request can both tick
            return self._tick(with_chain)

    def _tick(self, with_chain=False):
        d = self.blank()
        d["updated"] = int(time.time() * 1000)
        d["unread"] = self.digest.get("unread", 0)
        d["last"] = self.digest.get("last")
        d["chain"] = self.digest.get("chain")
        if self.host_gone():
            return self.gone_digest(d)         # no sidecar call at all: it would only wait on a host that is not there
        sc = self.ensure()
        if not sc:
            d["err"] = self.err
            d["archived"] = self.archive.info()["items"]
            d["tasks"] = self._task_digest()
            d["offline"] = bool(d["archived"])
            return d
        feed_failed = False
        err, st = sc.call("playbox_status", {}, wait=1, timeout=6)
        if err and st == "busy":
            return {**self.digest, "label": d["label"], "machine": d["machine"], "kind": d["kind"]}   # cockpit is using it; keep the last digest
        if err or not isinstance(st, dict):
            d["err"] = str(st)[:200]
            d["archived"] = self.archive.info()["items"]
            d["tasks"] = self._task_digest()
            d["offline"] = bool(d["archived"])
            return d
        host, joined = st.get("hosting"), st.get("joined")
        self.host_id = (host or {}).get("host_id") or (joined or {}).get("primary_host") or self.host_id
        if not host and self.rec.get("host") is not None and time.time() - self.last_rehost > 30:
            if self._host_again():          # it was meant to be hosting (a failed start, or the sidecar restarted underneath us): try again
                err, st = sc.call("playbox_status", {}, wait=1)
                if err or not isinstance(st, dict):
                    d["err"] = str(st)[:200]
                    return d
                host, joined = st.get("hosting"), st.get("joined")
        if host and not host.get("replica"):
            known, repo = self.rec.get("host"), host.get("repo")
            if known is None or (repo and known.get("repo") != repo):
                self.rec["host"] = {**(known or {}), **({"repo": repo} if repo else {})}      # remember what we host so a restart can host it again
                self.on_change and self.on_change()
        d["mode"] = "both" if host and joined else "host" if host else "guest" if joined else "none"
        d["members"] = len((host or {}).get("members") or []) + (1 if joined else 0)
        if d["mode"] == "none" and self.rec.get("knock_src") and self.rec.get("host") is None and not self.rec.get("adopted_host"):
            d["needs_invite"] = True            # joined through ask-to-join, and the sidecar no longer knows the room (it restarted: joins live in its memory only)
            d["err"] = "Your access to this room ran out. Ask the host for a new invite."
        d["me"] = (joined or {}).get("member_id") or st.get("guest_id")
        if d["mode"] != "none":
            ferr, feed = sc.call("playbox_feed", {"since": self.seq}, wait=1, timeout=6)
            if ferr and self.rec.get("adopted_host") and isinstance(feed, str) and ("not authorized" in feed or "authorization" in feed or "not in a room" in feed):
                if self._rejoin():          # the grant ran out (they last an hour): get a new one, then read again
                    ferr, feed = sc.call("playbox_feed", {"since": self.seq}, wait=1, timeout=6)
            if not ferr and isinstance(feed, dict):
                if isinstance(feed.get("next"), int) and feed["next"] < self.seq:
                    self.archive.host_restarted()       # the host's numbering started over: keep the old history as an earlier epoch
                    self.seq = 0
                self.keep(feed, d["me"])
                items = [i for i in feed.get("items", []) if isinstance(i, dict) and i.get("seq", 0) > self.seq]
                if self.primed:
                    for i in items:
                        if i.get("kind") == "event":
                            self.notes.append(event_note(str(i.get("text", "")), str(i.get("from", "")), i.get("meta")))
                for i in items:
                    t = i.get("text")
                    if self.primed and i.get("kind") == "event" and str(t).lower() == "join ok" and self.rec.get("endorsed") and i.get("from") not in self.rec["endorsed"]:
                        self.newcomers[i.get("from")] = time.time()          # someone new in a room where you vouch for agents: worth a look, not an alarm
                        self.notes.append(("join", f'A new member ({str(i.get("from"))[:6]}) joined a room where you endorse agents', "warn"))
                    if i.get("kind") == "say" and isinstance(t, str) and t.startswith(idcard.PREFIX) and self.rec.get("endorsed"):
                        snd = self.host_id if i.get("from") == "host" else i.get("from")
                        try:
                            c = idcard.verify(t, snd, self.host_id, i.get("time_ms") or 0)
                        except ValueError:
                            continue
                        e = self.rec["endorsed"].get(i.get("from"))
                        if e:
                            if board.card_hash(dict(board.clip_card(c), key=i.get("from"))) != e.get("card_hash"):
                                if i.get("from") not in self.idalerts:
                                    self.notes.append(("history", f'Endorsed agent "{e.get("name", "")}" changed its card: endorse it again if you expect that', "bad"))
                                self.idalerts[i.get("from")] = e.get("name", "an agent")
                            else:
                                self.idalerts.pop(i.get("from"), None)
                if items:
                    self._ingest_task_stats(items)
                    self.seq = max(i["seq"] for i in items)
                    says = [i for i in items if i.get("kind") == "say"]
                    for m in says:
                        ms = m.get("time_ms") or time.time() * 1000
                        b = int(ms / 300000)
                        self.buckets[b] = self.buckets.get(b, 0) + 1
                        self.newsays.append({"room": self.id, "label": self.rec["label"], "from": (m.get("from") or "")[:12], "me": m.get("from") == d["me"],
                                             "text": pretty_pbx(m.get("text", ""))[:200], "ms": int(ms)})
                    if self.primed:
                        d["unread"] += len([i for i in says if i.get("from") != d["me"]])
                    if says:
                        m = says[-1]
                        d["last"] = {"from": (m.get("from") or "")[:12], "text": pretty_pbx(m.get("text", ""))[:160], "ms": m.get("time_ms")}
            feed_failed = bool(ferr)
            if ferr and isinstance(feed, str) and not self.rec.get("adopted_host") and self.rec.get("host") is None and ("not authorized" in feed or "authorization" in feed or "not in a room" in feed):
                d["needs_invite"] = True            # a joined room whose grant ran out: the link was single-use, so only the host can let us back in
                d["err"] = "Your access to this room ran out. Ask the host for a new invite."
            if not ferr:
                self.primed = True
            if with_chain or time.time() - self.last_chain > CHAIN_EVERY:
                path = st.get("history_log")
                if path:
                    verr, v = sc.call("playbox_verify", {"path": path}, wait=2, timeout=10)
                    if not verr and isinstance(v, dict):
                        d["chain"] = {"ok": v.get("valid") is True, "entries": v.get("entries")}
                        self.last_chain = time.time()
                        self.notes.append(("history", f"Signed history verified, {v.get('entries')} entries" if d["chain"]["ok"] else "Signed history FAILED verification",
                                           "ok" if d["chain"]["ok"] else "bad"))
        if d.get("needs_invite") and self.on_needs_invite:
            try:
                self.on_needs_invite(self)
            except Exception as exc:
                self.notes.append(("invite", f"Rejoin check failed: {exc}", "warn"))
        nb = int(time.time() / 300)
        self.buckets = {k: v for k, v in self.buckets.items() if k > nb - 30}
        d["pulse"] = [self.buckets.get(nb - 23 + i, 0) for i in range(24)]
        w = witness_state(self.home)
        d["holds"] = [{"tool": e.get("tool"), "origin": e.get("origin"), "session": e.get("session")} for e in w["active"]]
        ids = {str(h.get("session")) for h in d["holds"]}
        for hid in ids - self.hold_ids:
            self.notes.append(("feed", "Witness paused a write until you decide", "warn"))
        self.hold_ids = ids
        d["archived"] = self.archive.info()["items"]
        d["tasks"] = self._task_digest()
        d["identity_alerts"] = [{"key": k, "name": n} for k, n in self.idalerts.items()]
        d["newcomers"] = [k for k, t in self.newcomers.items() if time.time() - t < 3600]
        d["offline"] = bool(d["mode"] != "none" and feed_failed and d["archived"])
        recent = d["last"] and d["last"].get("ms") and time.time() * 1000 - d["last"]["ms"] < LIVE_MS
        d["state"] = ("hold" if d["holds"] else "empty" if d["mode"] == "none" else "bad" if d["chain"] and not d["chain"]["ok"]
                      else "live" if recent else "idle")
        return d

    def refresh_witness(self):
        with self.tick_mu:
            w = witness_state(self.home)
            holds = [{"tool": e.get("tool"), "origin": e.get("origin"), "session": e.get("session")} for e in w["active"]]
            self.hold_ids = {str(h.get("session")) for h in holds}
            d = dict(self.digest)
            d["holds"] = holds
            recent = d.get("last") and d["last"].get("ms") and time.time() * 1000 - d["last"]["ms"] < LIVE_MS
            d["state"] = ("hold" if holds else "empty" if d.get("mode") == "none" else "bad" if d.get("chain") and not d["chain"]["ok"]
                          else "live" if recent else "idle")
            d["updated"] = int(time.time() * 1000)
            self.digest = d
            return w

    def seen(self):
        self.digest["unread"] = 0


class Hub:
    rejoining = staticmethod(lambda room_id: False)      # the server replaces it: True while a new ask-to-join for this room waits on the host
    reknock = None                                       # the server sets it: called with a room whose access ran out
    def __init__(self, store, binary, push_fn=None):
        self.store, self.binary, self.push_fn = store, binary, push_fn
        self.dir = Path(store.home)
        self.bus = Bus()
        self.rooms = {}
        self.known_attention = set()
        self.down_since = {}
        self.last_prune = 0.0
        self.last_backup = 0.0
        self.activity = deque(maxlen=80)
        self.extra_attention = []     # callables returning more attention items (join requests)
        self.sec = None               # hub.sec.SecLog, set by the server
        self.mu = threading.RLock()
        self.wake = threading.Event()
        with store.lock:
            recs = list(store.d["rooms"])
        for rec in recs:
            self._start(rec)
        threading.Thread(target=self._loop, daemon=True, name="poller").start()

    def _start(self, rec):
        r = Room(rec, self.binary, self.store.home)
        r.on_change = self.store.save
        r.on_needs_invite = lambda room: self.reknock and self.reknock(room)
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
        rooms_dir = (self.dir / "rooms").resolve()
        rp = Path(room.home).resolve()
        if rp != rooms_dir and rp.is_relative_to(rooms_dir):
            room.archive.delete()          # removing a hub-owned room also forgets what the hub saved of it; never touch an external home
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
            if isinstance(kw.get("muted"), bool):
                room.rec["muted"] = kw["muted"]
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

    def down_alert_s(self):
        """Seconds a room or the public address may stay unreachable before it is urgent: the hub setting, else PLAYBOX_DOWN_ALERT_S (default 300)."""
        lock = getattr(self.store, "lock", None)
        if lock:
            with lock:
                v = (self.store.d.get("notify") or {}).get("down_alert_s")
        else:
            v = (self.store.d.get("notify") or {}).get("down_alert_s")
        return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else DOWN_URGENT_S

    def muted(self, rid):
        r = self.rooms.get(rid)
        return bool(r and r.rec.get("muted"))

    def recent(self, n=40):
        return sorted(self.activity, key=lambda a: -a["ms"])[:n]

    def digests(self):
        with self.mu:
            return [{**r.digest, "muted": bool(r.rec.get("muted"))} for r in sorted(self.rooms.values(), key=lambda r: r.rec["created"])]

    def attention(self):
        items = []
        for d in self.digests():
            lab = d["label"]
            for h in d["holds"]:
                items.append({"id": f"hold:{d['id']}:{h['session']}", "room": d["id"], "kind": "hold", "urgent": True,
                              "title": f"Witness paused a write in {lab}", "body": f"tool {h.get('tool') or 'write'}",
                              "hold": {"room": d["id"], "label": lab, "session": h.get("session"), "tool": h.get("tool"), "origin": h.get("origin")}})
            if d["state"] == "bad":
                items.append({"id": f"chain:{d['id']}", "room": d["id"], "kind": "chain", "urgent": True, "title": f"History check failed in {lab}",
                              "body": "the signed log no longer verifies"})
            for al in d.get("identity_alerts") or []:
                items.append({"id": f"idchg:{d['id']}:{al['key'][:12]}", "room": d["id"], "kind": "identity", "urgent": True, "title": f"{al['name']} changed its identity card in {lab}",
                              "body": "you had endorsed it; review the change and endorse again"})
            if d.get("newcomers"):
                items.append({"id": f"new:{d['id']}:{len(d['newcomers'])}", "room": d["id"], "kind": "newcomer", "urgent": False, "title": f"New member in {lab}",
                              "body": f"{len(d['newcomers'])} joined recently in a room where you endorse agents"})
            if d.get("needs_invite") and not self.rejoining(d["id"]):
                items.append({"id": f"invite:{d['id']}", "room": d["id"], "kind": "invite", "urgent": False, "title": f"{lab} needs a new invite", "body": "your access ran out; ask the host"})
            bad = bool(d.get("err")) and not d.get("needs_invite") and (d["state"] == "down" or d.get("offline"))
            if bad:
                since = self.down_since.setdefault(d["id"], time.time())
                long = time.time() - since > self.down_alert_s()           # a blip is not worth waking a phone; five minutes is
                items.append({"id": f"down:{d['id']}" + (":long" if long else ""), "room": d["id"], "kind": "down", "urgent": long,
                              "title": f"{lab} is unreachable" + (" for a while" if long else ""), "body": d["err"][:100]})
            else:
                self.down_since.pop(d["id"], None)
            if d["unread"]:
                items.append({"id": f"unread:{d['id']}", "room": d["id"], "kind": "unread", "urgent": False, "count": d["unread"],
                              "title": f"{d['unread']} new in {lab}", "body": (d["last"] or {}).get("text", "")[:100]})
        for fn in self.extra_attention:
            items += fn()
        return items

    def _publish_changes(self, room, new):
        old = room.digest
        room.digest = new

        def _strip(x):
            return {k: v for k, v in x.items() if k != "updated"}

        if _strip(old) != _strip(new):
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
                    if self.sec:
                        for a in drained:
                            self.sec.add("feed", f"Message from {a.get('from', '')[:6] or 'host'}, kept as untrusted text", "info", room.id)
                        notes, room.notes = room.notes, []
                        for stage, text, level in notes:
                            self.sec.add(stage, text, level, room.id)
                except Exception as e:
                    room.digest = {**room.blank(), "err": f"poll failed: {e}"[:200]}
            if time.time() - self.last_backup > 3600:
                self.last_backup = time.time()
                try:
                    self.store.backup()
                except OSError:
                    pass
            if time.time() - self.last_prune > PRUNE_EVERY:
                self.last_prune = time.time()
                try:
                    prune_stale()
                except (OSError, ValueError):
                    pass
            att = self.attention()
            ids = {a["id"] for a in att}
            fresh = [a for a in att if a["id"] not in self.known_attention]
            self.known_attention = ids
            if fresh:
                self.bus.publish("attention", {"items": att})
                due = [a for a in fresh if a["urgent"] or not watched]   # urgent always pushes: a backgrounded tab can still hold an open stream
                due = [a for a in due if not self.muted(a.get("room"))]  # a muted room stays in Attention but never wakes a phone
                if self.push_fn and due:
                    self.push_fn(due)
            self.wake.wait(8 if watched else 45)
            self.wake.clear()

    def close(self):
        for r in list(self.rooms.values()):
            r.close()
