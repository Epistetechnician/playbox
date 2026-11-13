"""The hub's live view of its own security: a log of protocol events and a posture check.

Every step of the invite-to-history workflow is a *stage*. Handlers and the room poller report what happened with `SecLog.add`; the
System view draws the stages, their counters and the newest events. `posture` turns the hub's current state into a list of checks
that each say what is protected, what is not, and what to do. Nothing here decides access: it only shows what the real checks did,
so it never contains a key, a link, a code or a message body.
"""
import time
from collections import deque

# The workflow in order. `id` is what events carry; the text is what the System view prints.
STAGES = [
    {"id": "edge", "name": "Edge", "what": "Cloudflare Access", "guard": "Only allowed identities reach the login. Invite pages are the one public door."},
    {"id": "auth", "name": "Sign in", "what": "Passkey", "guard": "Phishing-resistant sign-in. Sensitive actions need a fresh touch."},
    {"id": "invite", "name": "Invite", "what": "Approval link", "guard": "No key in the link. It can be locked to one agent's id."},
    {"id": "knock", "name": "Knock", "what": "Ask to join", "guard": "The asker proves its own key and both sides show a six-digit code."},
    {"id": "approve", "name": "Approve", "what": "Your touch", "guard": "You match the codes and touch your passkey. Nobody is let in without it."},
    {"id": "join", "name": "Join", "what": "Bound grant", "guard": "The room mints a grant tied to that key; the host proves what it runs."},
    {"id": "feed", "name": "Feed", "what": "Untrusted text", "guard": "What other members say is data. It is never trusted or run."},
    {"id": "history", "name": "History", "what": "Signed chain", "guard": "Every action is signed into a hash chain that is re-verified here."},
]
STAGE_IDS = [s["id"] for s in STAGES]
LEVELS = ("info", "ok", "warn", "bad")


class SecLog:
    def __init__(self, bus, maxlen=300):
        self.bus = bus
        self.events = deque(maxlen=maxlen)
        self.stage = {s: {"count": 0, "last": 0, "level": "info", "warn": 0} for s in STAGE_IDS}
        self.seq = 0

    def add(self, stage, text, level="info", room=None):
        """Record one event. `text` must be written for a person and must not carry a secret."""
        if stage not in self.stage:
            return None
        level = level if level in LEVELS else "info"
        now = int(time.time() * 1000)
        if self.events:                       # a burst of identical events (a scanner, a page reload) is one line with a count
            top = self.events[0]
            if top["stage"] == stage and top["text"] == str(text)[:160] and top["level"] == level and top["room"] == room and now - top["ms"] < 10000:
                top["x"] = top.get("x", 1) + 1
                st = self.stage[stage]
                st["count"] += 1
                st["last"] = now
                return top
        self.seq += 1
        ev = {"n": self.seq, "ms": int(time.time() * 1000), "stage": stage, "text": str(text)[:160], "level": level, "room": room}
        self.events.appendleft(ev)
        st = self.stage[stage]
        st["count"] += 1
        st["last"] = ev["ms"]
        st["level"] = level
        if level in ("warn", "bad"):
            st["warn"] += 1
        if self.bus:
            self.bus.publish("sec", ev)
        return ev

    def recent(self, n=60):
        return list(self.events)[:n]

    def stages(self):
        return {k: dict(v) for k, v in self.stage.items()}


def check(cid, label, level, detail, fix=""):
    return {"id": cid, "label": label, "level": level, "detail": detail, "fix": fix}


def posture(*, public, access, creds, weak_sessions, invites, knocks, push, rooms, fresh):
    """Checks over the hub's current state. `level` is ok, warn or bad; bad is something that lets the wrong person in."""
    out = []
    if public and access:
        out.append(check("edge", "Edge gate", "ok", "Cloudflare Access is enforced, and the hub rejects any request without its token."))
    elif public:
        out.append(check("edge", "Edge gate", "bad", "The hub is on the internet with no Cloudflare Access in front. Only the passkey stands between the web and the login.",
                         "Restart the hub with --cloudflare-access and --cloudflare-aud, and keep the Access application."))
    else:
        out.append(check("edge", "Edge gate", "ok", "Not published: the hub only answers on this machine or your own network."))
    out.append(check("passkey", "Passkey sign-in", "ok" if creds else "bad", f"{creds} passkey{'s' if creds != 1 else ''} enrolled." if creds else "No passkey enrolled yet.",
                     "" if creds else "Open the hub and register a passkey first."))
    if public and weak_sessions:
        out.append(check("weak", "Plain-HTTP sessions", "bad", f"{weak_sessions} weak session(s) exist while the hub is public.", "End them under Hub, Sessions."))
    else:
        out.append(check("weak", "Plain-HTTP sessions", "ok", "None, or the hub is not public. Pairing codes never work through a tunnel."))
    out.append(check("stepup", "Step-up for access changes", "ok", "Invites, approvals, revocations and device changes each ask for a fresh passkey touch."
                     + (" Your session is fresh right now." if fresh else "")))
    keyed = invites.get("keyed_open", 0)
    if keyed:
        out.append(check("invites", "Invite links", "warn", f"{keyed} open invite(s) carry a key in the link. Whoever holds the link can join.", "Withdraw them or use approval invites."))
    else:
        locked = invites.get("guest_locked_open", 0)
        out.append(check("invites", "Invite links", "ok", "No open invite carries a key." + (f" {locked} locked to one id." if locked else "")))
    if knocks:
        out.append(check("knocks", "Join requests", "warn", f"{knocks} person(s) waiting for you to compare codes and approve.", "Open Attention."))
    else:
        out.append(check("knocks", "Join requests", "ok", "Nobody is waiting."))
    bad = [r for r in rooms if r.get("state") == "bad"]
    held = sum(len(r.get("holds") or []) for r in rooms)
    if bad:
        out.append(check("chain", "History chain", "bad", f"{len(bad)} room(s) failed the signed-history check.", "Open the room; do not trust its history until you know why."))
    elif not rooms:
        out.append(check("chain", "History chain", "warn", "No rooms to verify yet."))
    else:
        out.append(check("chain", "History chain", "ok", f"{len(rooms)} room(s) verified; each log's signatures and links check out."))
    out.append(check("witness", "Witness holds", "warn" if held else "ok", f"{held} write(s) paused for you." if held else "Nothing is paused.", "Open Attention." if held else ""))
    unhosted = [r for r in rooms if r.get("mode") == "none"]
    if unhosted:
        out.append(check("hosted", "Rooms reachable", "warn", f"{len(unhosted)} room(s) are not hosted or joined from this hub.", "Host or re-join them."))
    out.append(check("push", "Phone alerts", "ok" if push else "warn", "A device will buzz when someone knocks." if push else "No device is subscribed, so a knock waits until you look.",
                     "" if push else "Hub, Notifications, Turn on."))
    return out
