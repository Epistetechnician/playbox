"""Ask-to-join: host approval for invites, so a forwarded link cannot be used by the wrong person.

An approval invite carries no Playbox key at all. Its share URL holds only a random secret `k` in the #fragment. Someone who opens it
and wants in "knocks": they send the host their own guest id (from their Playbox identity) and a name. Both sides compute the same
short verify code from (invite id, guest id), the knocker reads it out, the host compares it and approves. Only then does the host's
room mint a join link, and that link is bound to that guest id, so it is useless to anyone who intercepts it.
"""
import hashlib
import hmac
import ipaddress
import json
import re
import secrets
import socket
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

HEX64 = re.compile(r"^[0-9a-f]{64}$")
_NUM_LABEL = re.compile(r"^(0x[0-9a-fA-F]+|[0-9]+)$")
CGNAT = ipaddress.ip_network("100.64.0.0/10")
TAILNET_V6 = ipaddress.ip_network("fd7a:115c:a1e0::/48")
MAX_PENDING_PER_INVITE = 5
LINK_TTL = 600          # a minted link must be redeemed within ten minutes
POLL_EVERY = 3
GIVE_UP = 1800
REKNOCK_EVERY = 900      # a room that lost its access asks its host again at most this often


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
                    if r["state"] != "done":
                        return r                  # asking twice is not asking twice
                    del self.store.d["knocks"][r["rid"]]      # already let in once and asking again (its access ran out): a fresh request, bound to the same id
                    mine = [x for x in mine if x is not r]
                    break
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
        with self.store.lock:
            inv = dict(self.store.d.get("invites", {}))
            knocks = [dict(r) for r in self.store.d["knocks"].values()]
        return [r for r in sorted(knocks, key=lambda r: r["created"])
                if r["state"] == "pending" and r["invite"] in inv and inv[r["invite"]]["expires"] > now and inv[r["invite"]]["status"] != "revoked"]

    def get(self, rid):
        with self.store.lock:
            r = self.store.d["knocks"].get(rid)
            return dict(r) if r else None

    def set(self, rid, **kw):
        with self.store.lock:
            r = self.store.d["knocks"].get(rid)
            if r:
                r.update(kw)
                self.store.save()
            return r


# ------------------------------------------------------------------ friend side
def _numeric_ipv4_name(host):
    """True for non-standard numeric/hex/octal IPv4 forms (2852039166, 0xa9.0xfe.0xa9.0xfe, 0x7f.0.0.1, 0177.0.0.1)
    that ipaddress.ip_address rejects as non-decimal-quads while OS resolvers accept them."""
    h = (host or "").rstrip(".")
    if not h:
        return False
    try:
        ipaddress.ip_address(h)
        return False
    except ValueError:
        pass
    parts = h.split(".")
    return bool(parts) and all(_NUM_LABEL.match(p) for p in parts)


def _bad_ip(addr, allow_local=False, allow_tailnet=False):
    ip = ipaddress.ip_address(addr)
    ip = getattr(ip, "ipv4_mapped", None) or ip
    if ip.is_link_local or ip.is_unspecified or ip.is_multicast or ip.is_reserved:
        return True
    if not allow_local:
        if ip in CGNAT or (ip.version == 6 and ip in TAILNET_V6):
            return not allow_tailnet
        if ip.is_private or ip.is_loopback:
            return True
    return False


def check_base(base, allow_local=None):
    """The host's address comes from a link, so it is untrusted: https, or plain http only for this machine / a private network."""
    u = urllib.parse.urlparse(base)
    if u.scheme not in ("https", "http") or not u.hostname or u.username or u.password:
        raise KnockError("that isn't a usable host address")
    h = u.hostname.lower().rstrip(".")
    if not h or _numeric_ipv4_name(h):
        raise KnockError("that isn't a usable host address")
    try:
        lit = ipaddress.ip_address(h)
        lit = getattr(lit, "ipv4_mapped", None) or lit
    except ValueError:
        lit = None
    if allow_local is not None:
        local_ok = bool(allow_local)
    elif u.scheme == "http":
        local_ok = (h == "localhost" or h.endswith(".local")) if lit is None else bool(lit.is_private or lit.is_loopback)
        if not local_ok:
            raise KnockError("plain http is only allowed to this machine or a private network")
    else:
        if h == "localhost" or h.endswith((".local", ".localhost", ".internal")):
            raise KnockError("that isn't a usable host address")
        local_ok = False
    if lit is not None and _bad_ip(lit, allow_local=local_ok, allow_tailnet=h.endswith(".ts.net")):
        raise KnockError("that isn't a usable host address")
    return f"{u.scheme}://{u.netloc}"


def resolve_base(base, allow_local=None):
    base = check_base(base, allow_local=allow_local)
    u = urllib.parse.urlparse(base)
    h = u.hostname.lower().rstrip(".")
    port = u.port or (443 if u.scheme == "https" else 80)
    local_ok = (u.scheme == "http") if allow_local is None else bool(allow_local)
    tailnet_ok = h.endswith(".ts.net")
    try:
        infos = socket.getaddrinfo(h, port, type=socket.SOCK_STREAM)
    except OSError as e:
        raise KnockError(f"couldn't resolve host: {e}", 502)
    if not infos or any(_bad_ip(info[4][0], allow_local=local_ok, allow_tailnet=tailnet_ok) for info in infos):
        raise KnockError("that isn't a usable host address")
    return base


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


_opener = urllib.request.build_opener(_NoRedirect)
_OPEN = _opener.open


def host_post(base, path, body, timeout=12):
    try:
        base = resolve_base(base) if _opener.open == _OPEN else check_base(base)
    except KnockError as e:
        return 0, {"error": str(e)[:120]}
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
        self.last_re = {}            # room id -> when this hub last asked its host to let it back in
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
               "state": "waiting", "agent": bool(agent_guest), "name": name, "created": time.time(), "note": ""}
        with self.store.lock:
            self.store.d["knock_out"][rid] = rec
            self.store.save()
        self._spawn(rid)
        return rec

    def reknock(self, room):
        """A room joined through ask-to-join lost its access (hub or sidecar restart, or the grant ended). The invite link was single-use,
        so ask the same host again with the stored invite id and key. The host still has to approve; nothing here lets us in by itself."""
        src = room.rec.get("knock_src")
        if not src:
            return None
        with self.mu:
            if time.time() - self.last_re.get(room.id, 0) < REKNOCK_EVERY:
                return None
            with self.store.lock:
                waiting = any(r["state"] == "waiting" and r.get("room") == room.id for r in self.store.d["knock_out"].values())
            if waiting:
                return None
            self.last_re[room.id] = time.time()
        threading.Thread(target=self._reknock, args=(room, src), daemon=True, name="reknock-" + room.id[:4]).start()

    def _reknock(self, room, src):
        sc = room.ensure()
        err, st = sc.call("playbox_status", {}) if sc else (True, None)
        guest = (st or {}).get("guest_id") if isinstance(st, dict) and not err else None
        if not guest:
            return
        code, resp = host_post(src["base"], f"/api/invite/{urllib.parse.quote(src['invite'], safe='')}/knock", {"k": src["k"], "guest": guest, "name": src.get("name") or "Someone"})
        if code != 200 or not resp.get("rid") or resp.get("code") != verify_code(src["invite"], guest):
            room.err = ("the host's invite is no longer open: ask them for a new one" if code in (403, 404, 410) else "couldn't reach the host to ask again")
            return
        rid = resp["rid"]
        rec = {"rid": rid, "base": src["base"], "invite": src["invite"], "k": src["k"], "room": room.id, "label": room.rec["label"], "guest": guest, "code": resp["code"],
               "state": "waiting", "agent": False, "rejoin": True, "created": time.time(), "note": ""}
        with self.store.lock:
            self.store.d["knock_out"][rid] = rec
            self.store.save()
        self._spawn(rid)

    def waiting_for_all(self):
        with self.store.lock:
            return [dict(r) for r in self.store.d["knock_out"].values() if r["state"] == "waiting" and r.get("rejoin")]

    def waiting_for(self, room_id):
        with self.store.lock:
            return [dict(r) for r in self.store.d["knock_out"].values() if r["state"] == "waiting" and r.get("rejoin") and r.get("room") == room_id]

    def _set(self, rid, **kw):
        with self.store.lock:
            r = self.store.d["knock_out"].get(rid)
            if r:
                r.update(kw)
                self.store.save()

    def _run(self, rid):
        start = time.time()
        while time.time() - start < GIVE_UP:
            with self.store.lock:
                r = self.store.d["knock_out"].get(rid)
                r = dict(r) if r else None
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
                if not err and room and not r.get("rejoin"):
                    room.rec["knock_src"] = {"base": r["base"], "invite": r["invite"], "k": r["k"], "name": r.get("name") or "", "label": r.get("label")}
                    room.on_change and room.on_change()
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
        if r.get("room") and not r.get("rejoin"):
            self.hub.remove(r["room"])      # the placeholder room that never got in

    def listing(self):
        out = []
        with self.store.lock:
            recs = sorted((dict(r) for r in self.store.d["knock_out"].values()), key=lambda r: -r["created"])
        for r in recs:
            row = {k: r[k] for k in ("rid", "label", "code", "state", "agent", "created", "note", "room")}
            row["rejoin"] = bool(r.get("rejoin"))
            if r["agent"] and r["state"] == "approved":
                with self.mu:
                    row["link"] = self.links.get(r["rid"])
            out.append(row)
        return out[:10]

    def cancel(self, rid):
        with self.store.lock:
            r = self.store.d["knock_out"].get(rid)
            r = dict(r) if r else None
        if not r:
            return False
        self._set(rid, state="cancelled")
        self._cleanup(r)
        with self.mu:
            self.links.pop(rid, None)
        return True

    def dismiss(self, rid):
        """Clear an entry from the list. Still waiting means giving up: the placeholder room goes too. Finished entries just disappear."""
        with self.store.lock:
            r = self.store.d["knock_out"].get(rid)
            r = dict(r) if r else None
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
