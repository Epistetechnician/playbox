"""Friend invites. An invite is the existing Playbox single-use join link (a signed token the room's host mints) plus a
share page. The hub stores only metadata here (room name, who from, expiry, status), never the join link: the link
travels in the share URL's #fragment, which browsers do not send to any server, and through the owner's own
authenticated API call that minted it."""
import secrets
import time

MAX_TTL = 7 * 86400
LINK_TTLS = (900, 3600, 86400)             # how long the link itself works
GRANT_TTLS = (3600, 86400, 7 * 86400)       # how long the joined member's access lasts
USES = (1, 2, 3)


class InviteError(ValueError):
    pass


class Invites:
    def __init__(self, store):
        self.store = store
        store.d.setdefault("invites", {})

    def create(self, room_id, room_label, from_name, ttl, grant, uses, members_now, guest=None):
        if ttl not in LINK_TTLS or grant not in GRANT_TTLS or uses not in USES:
            raise InviteError("choose one of the offered durations and counts")
        if guest is not None and (len(guest) != 64 or any(c not in "0123456789abcdef" for c in guest)):
            raise InviteError("a guest id is 64 hex characters (from `playbox id`)")
        now = time.time()
        iid = secrets.token_urlsafe(16)
        rec = {"id": iid, "room": room_id, "label": room_label[:60], "from": (from_name or "A Playbox user")[:40], "created": now, "expires": now + ttl,
               "grant": grant, "uses": uses, "members_at": members_now, "guest": bool(guest), "status": "pending"}
        with self.store.lock:
            self._gc(now)
            self.store.d["invites"][iid] = rec
            self.store.save()
        return rec

    def _gc(self, now):
        keep = {}
        for k, v in self.store.d["invites"].items():
            if v["expires"] + 7 * 86400 > now:      # keep a week after expiry so the owner can see what happened
                keep[k] = v
        self.store.d["invites"] = keep

    def state(self, rec, now=None, members=None):
        """`members` is the room's current member count; when that has grown by the invite's uses, the invite is spent."""
        now = now or time.time()
        if rec["status"] == "revoked":
            return "revoked"
        if members is not None and members - rec["members_at"] >= rec["uses"]:
            return "used"
        return "expired" if rec["expires"] <= now else "pending"

    def public(self, iid, members=None):
        """What the share page may show anyone holding the id: nothing about members, nothing secret."""
        rec = self.store.d["invites"].get(iid)
        if not rec:
            return None
        st = self.state(rec, members=members)
        out = {"state": st}
        if st == "pending":
            out.update({"room": rec["label"], "from": rec["from"], "expires": int(rec["expires"]), "grant": rec["grant"], "uses": rec["uses"], "guest_only": rec["guest"]})
        return out

    def listing(self, room_digests):
        now = time.time()
        members = {d["id"]: d.get("members", 0) for d in room_digests}
        out = []
        for rec in sorted(self.store.d["invites"].values(), key=lambda r: -r["created"]):
            joined = max(0, members.get(rec["room"], rec["members_at"]) - rec["members_at"])
            out.append({**{k: rec[k] for k in ("id", "room", "label", "created", "expires", "grant", "uses", "guest")}, "state": self.state(rec, now, members.get(rec["room"])), "joined": joined})
        return out

    def revoke(self, iid):
        with self.store.lock:
            rec = self.store.d["invites"].get(iid)
            if not rec:
                return False
            rec["status"] = "revoked"
            self.store.save()
            return True
