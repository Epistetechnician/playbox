"""Ask-to-join: host approval for invites, so a forwarded link cannot be used by the wrong person.

An approval invite carries no Playbox key at all. Its share URL holds only a random secret `k` in the #fragment. Someone who opens it
and wants in "knocks": they send the host their own guest id (from their Playbox identity) and a name. Both sides compute the same
short verify code from (invite id, guest id), the knocker reads it out, the host compares it and approves. Only then does the host's
room mint a join link, and that link is bound to that guest id, so it is useless to anyone who intercepts it.
"""
import hashlib
import hmac
import json
import re
import secrets
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import ipaddress

HEX64 = re.compile(r"^[0-9a-f]{64}$")
MAX_PENDING_PER_INVITE = 5
LINK_TTL = 600          # a minted link must be redeemed within ten minutes
POLL_EVERY = 3
GIVE_UP = 1800


class KnockError(ValueError):
    def __init__(self, msg, code=400):
        super().__init__(msg)
        self.code = code


def verify_code(invite_id, guest):
    """Six digits both sides can compute and read aloud: '482 913'."""
    n = int.from_bytes(hashlib.sha256(f"playbox-verify:{invite_id}:{guest}".encode()).digest()[:4], "big") % 1_000_000
    return f"{n:06d}"[:3] + " " + f"{n:06d}"[3:]


def khash(k):
    return hashlib.sha256(("playbox-knock-key:" + k).encode()).hexdigest()


# ------------------------------------------------------------------ host side
class Knocks:
    def __init__(self, store):
        self.store = store
        store.d.setdefault("knocks", {})

    def _gc(self, now):
        self.store.d["knocks"] = {k: v for k, v in self.store.d["knocks"].items() if v["created"] + 86400 > now}

    def knock(self, inv, k, guest, name):
        """`inv` is the invite record. Raises KnockError."""
        if not inv.get("approve") or not hmac.compare_digest(khash(k or ""), inv.get("khash", "")):
            raise KnockError("that invite doesn't take requests", 403)
        guest = (guest or "").strip().lower()
        if not HEX64.match(guest):
            raise KnockError("a guest id is 64 hex characters (from `playbox id`)")
        name = re.sub(r"[\x00-\x1f\x7f]", " ", str(name or "")).strip()[:40] or "Someone"
        now = time.time()
        if inv["expires"] <= now or inv["status"] == "revoked":
            raise KnockError("this invite is no longer open", 410)
        with self.store.lock:
            self._gc(now)
            mine = [r for r in self.store.d["knocks"].values() if r["invite"] == inv["id"]]
            for r in mine:
                if r["guest"] == guest:
                    return r                      # asking twice is not asking twice
            if len([r for r in mine if r["state"] == "pending"]) >= MAX_PENDING_PER_INVITE:
                raise KnockError("too many requests waiting on this invite", 429)
            if len([r for r in mine if r["state"] in ("approved", "done")]) >= inv["uses"]:
                raise KnockError("this invite has already let in everyone it was for", 410)
            rid = secrets.token_urlsafe(9)
            rec = {"rid": rid, "invite": inv["id"], "room": inv["room"], "guest": guest, "name": name, "code": verify_code(inv["id"], guest),
                   "created": now, "state": "pending", "link": None}
            self.store.d["knocks"][rid] = rec
            self.store.save()
            return rec

    def poll(self, inv, k, rid):
        if not hmac.compare_digest(khash(k or ""), inv.get("khash", "")):
            raise KnockError("bad key", 403)
        with self.store.lock:
            r = self.store.d["knocks"].get(rid)
            if not r or r["invite"] != inv["id"]:
                raise KnockError("unknown request", 404)
            if r["state"] == "approved" and r["link"]:
                link, r["link"], r["state"] = r["link"], None, "done"       # handed over exactly once, then forgotten
                self.store.save()
                return {"state": "approved", "link": link}
            return {"state": "done" if r["state"] == "done" else r["state"]}

    def pending(self):
        now = time.time()
        inv = self.store.d.get("invites", {})
        return [r for r in sorted(self.store.d["knocks"].values(), key=lambda r: r["created"])
                if r["state"] == "pending" and r["invite"] in inv and inv[r["invite"]]["expires"] > now and inv[r["invite"]]["status"] != "revoked"]

    def get(self, rid):
        return self.store.d["knocks"].get(rid)

    def set(self, rid, **kw):
        with self.store.lock:
            r = self.store.d["knocks"].get(rid)
            if r:
                r.update(kw)
                self.store.save()
            return r


# ------------------------------------------------------------------ friend side
def check_base(base):
    """The host's address comes from a link, so it is untrusted: https, or plain http only for this machine / a private network."""
    u = urllib.parse.urlparse(base)
    if u.scheme not in ("https", "http") or not u.hostname or u.username or u.password:
        raise KnockError("that isn't a usable host address")
    try:
        lit = ipaddress.ip_address(u.hostname)
        lit = getattr(lit, "ipv4_mapped", None) or lit
    except ValueError:
        lit = None
    if lit and (lit.is_link_local or lit.is_unspecified or lit.is_multicast):      # 169.254.169.254 (cloud metadata) and friends, over either scheme
        raise KnockError("that isn't a usable host address")
    if u.scheme == "http":
        h = u.hostname
        try:
            a = ipaddress.ip_address(h)
            local = a.is_private or a.is_loopback
        except ValueError:
            local = h == "localhost" or h.endswith(".local")
        if not local:
            raise KnockError("plain http is only allowed to this machine or a private network")
    return f"{u.scheme}://{u.netloc}"


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


_opener = urllib.request.build_opener(_NoRedirect)


def host_post(base, path, body, timeout=12):
    req = urllib.request.Request(base + path, json.dumps(body).encode(), {"Content-Type": "application/json", "User-Agent": "playbox-hub"})
    try:
        with _opener.open(req, timeout=timeout) as r:
            return r.status, json.loads(r.read(1 << 16) or b"{}")
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read(1 << 14) or b"{}")
        except ValueError:
            return e.code, {}
    except (urllib.error.URLError, OSError, ValueError) as e:
        return 0, {"error": str(getattr(e, "reason", e))[:120]}


class Outgoing:
    """Requests this hub has sent to other people's hubs, and what became of them."""
    def __init__(self, hub, store):
        self.hub, self.store = hub, store
        store.d.setdefault("knock_out", {})
        self.links = {}              # agent-mode links, in memory only
        self.mu = threading.Lock()
        for rid, r in list(store.d["knock_out"].items()):
            if r["state"] == "waiting":
                self._spawn(rid)

    def _spawn(self, rid):
        threading.Thread(target=self._run, args=(rid,), daemon=True, name="knock-" + rid[:4]).start()

    def start(self, base, iid, k, label, name, agent_guest=None):
        base = check_base(base)
        room = None
        if agent_guest:
            guest = agent_guest.strip().lower()
            if not HEX64.match(guest):
                raise KnockError("an agent's guest id is 64 hex characters; ask it for its playbox guest_id")
        else:
            room = self.hub.add((label or "Invited room")[:60], "this machine", "shared")
            sc = room.ensure()
            err, st = sc.call("playbox_status", {}) if sc else (True, None)
            guest = (st or {}).get("guest_id") if isinstance(st, dict) and not err else None
            if not guest:
                self.hub.remove(room.id)
                raise KnockError("this machine's Playbox couldn't start: " + str(room.err or st)[:120], 502)
        code, resp = host_post(base, f"/api/invite/{urllib.parse.quote(iid, safe='')}/knock", {"k": k, "guest": guest, "name": name})
        if code != 200 or not resp.get("rid"):
            if room:
                self.hub.remove(room.id)
            raise KnockError(resp.get("error") or ("couldn't reach the host's hub" if code == 0 else f"the host's hub said {code}"), 502 if code in (0, 5) else (code or 502))
        if resp.get("code") != verify_code(iid, guest):
            if room:
                self.hub.remove(room.id)
            raise KnockError("the host's hub returned a different verify code than this device computed; not continuing", 502)
        rid = resp["rid"]
        rec = {"rid": rid, "base": base, "invite": iid, "k": k, "room": room.id if room else None, "label": label, "guest": guest, "code": resp["code"],
               "state": "waiting", "agent": bool(agent_guest), "created": time.time(), "note": ""}
        with self.store.lock:
            self.store.d["knock_out"][rid] = rec
            self.store.save()
        self._spawn(rid)
        return rec

    def _set(self, rid, **kw):
        with self.store.lock:
            r = self.store.d["knock_out"].get(rid)
            if r:
                r.update(kw)
                self.store.save()

    def _run(self, rid):
        start = time.time()
        while time.time() - start < GIVE_UP:
            r = self.store.d["knock_out"].get(rid)
            if not r or r["state"] != "waiting":
                return
            code, resp = host_post(r["base"], f"/api/invite/{urllib.parse.quote(r['invite'], safe='')}/poll", {"k": r["k"], "rid": rid})
            st = resp.get("state")
            if code == 200 and st == "approved" and resp.get("link"):
                if r["agent"]:
                    with self.mu:
                        self.links[rid] = resp["link"]
                    return self._set(rid, state="approved")
                room = self.hub.get(r["room"])
                sc = room.ensure() if room else None
                err, data = sc.call("playbox_join", {"link": resp["link"]}) if sc else (True, "the room is gone")
                self.hub.wake.set()
                return self._set(rid, state="failed" if err else "joined", note=(data if isinstance(data, str) else json.dumps(data))[:160] if err else "")
            if code == 200 and st == "denied":
                self._set(rid, state="denied")
                return self._cleanup(r)
            if code in (404, 410, 403):
                self._set(rid, state="expired")
                return self._cleanup(r)
            time.sleep(POLL_EVERY)
        self._set(rid, state="expired")

    def _cleanup(self, r):
        if r.get("room"):
            self.hub.remove(r["room"])      # the placeholder room that never got in

    def listing(self):
        out = []
        for r in sorted(self.store.d["knock_out"].values(), key=lambda r: -r["created"]):
            row = {k: r[k] for k in ("rid", "label", "code", "state", "agent", "created", "note", "room")}
            if r["agent"] and r["state"] == "approved":
                row["link"] = self.links.get(r["rid"])
            out.append(row)
        return out[:10]

    def cancel(self, rid):
        r = self.store.d["knock_out"].get(rid)
        if not r:
            return False
        self._set(rid, state="cancelled")
        self._cleanup(r)
        with self.mu:
            self.links.pop(rid, None)
        return True

    def dismiss(self, rid):
        """Clear an entry from the list. Still waiting means giving up: the placeholder room goes too. Finished entries just disappear."""
        r = self.store.d["knock_out"].get(rid)
        if not r:
            return False
        if r["state"] == "waiting":
            self.cancel(rid)
        self.forget(rid)
        return True

    def forget(self, rid):
        with self.store.lock:
            self.store.d["knock_out"].pop(rid, None)
            self.store.save()
        with self.mu:
            self.links.pop(rid, None)
