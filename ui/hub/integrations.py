"""Third-party app connections for rooms, brokered through Composio.

The hub holds the Composio API key (a 0600 file next to hub.json, never sent to a browser). A person connects an
app once through the provider's own OAuth page; a room is then *granted* that account at a level (read / write /
admin). Grants live in the hub store. Nothing here ever returns a key or an OAuth token."""
import hashlib
import hmac
import json
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request
import secrets

BASE = "https://backend.composio.dev/api/v3.1"
LEVELS = ("read", "write", "admin")
SLUG = re.compile(r"^[a-z0-9][a-z0-9_\-]{0,63}$")
ID = re.compile(r"^[A-Za-z0-9_\-]{1,128}$")


RANK = {"read": 0, "write": 1, "admin": 2}
_ADMIN = {"DELETE", "REMOVE", "DESTROY", "TRUNCATE", "REVOKE", "TRANSFER", "PURGE", "BAN", "KICK", "ARCHIVE", "TRASH", "ADMIN", "PERMISSION", "PERMISSIONS", "UNINSTALL", "DISABLE"}
_WRITE = {"CREATE", "SEND", "UPDATE", "ADD", "POST", "EDIT", "SET", "PATCH", "UPLOAD", "WRITE", "INSERT", "REPLY", "FORWARD", "MOVE", "COPY", "MODIFY",
          "RUN", "EXECUTE", "TRIGGER", "START", "STOP", "CANCEL", "APPROVE", "MERGE", "PUBLISH", "COMMENT", "LABEL", "INVITE", "SHARE", "RENAME",
          "REGISTER", "SUBMIT", "SCHEDULE", "ASSIGN", "CLOSE", "OPEN", "PUSH", "FORK", "STAR", "UNSTAR", "FOLLOW", "REACT", "LOCK", "UNLOCK", "ENABLE", "RESTORE", "IMPORT"}
_READ = {"GET", "LIST", "FETCH", "SEARCH", "READ", "FIND", "LOOKUP", "QUERY", "DESCRIBE", "CHECK", "COUNT", "VIEW", "RETRIEVE", "DOWNLOAD", "SHOW", "EXPORT", "WATCH", "PROFILE"}


def classify(toolkit, tool):
    """The level a Composio tool needs, from its name. Admin verbs win, then write verbs, then read verbs; a name with none of them needs write.
    None when the tool doesn't belong to the toolkit. This is a name heuristic, not a guarantee: grant `read` only where a wrong guess is acceptable."""
    t = str(tool or "").upper()
    if not re.match(r"^[A-Z0-9_]{3,120}$", t) or not t.startswith(str(toolkit).upper() + "_"):
        return None
    words = set(filter(None, t[len(toolkit) + 1:].split("_")))
    if not words:
        return None
    if words & _ADMIN:
        return "admin"
    if words & _WRITE:
        return "write"
    return "read" if words & _READ else "write"


class IntegError(Exception):
    def __init__(self, status, msg):
        super().__init__(msg)
        self.status, self.msg = status, msg


class Composio:
    def __init__(self, key, base=None):
        self.key, self.base = key, (base or os.environ.get("PLAYBOX_COMPOSIO_BASE") or BASE).rstrip("/")

    def _req(self, method, path, body=None, query=None):
        url = self.base + path + ("?" + urllib.parse.urlencode(query) if query else "")
        req = urllib.request.Request(url, method=method, data=None if body is None else json.dumps(body).encode(),
                                     headers={"x-api-key": self.key, "Content-Type": "application/json", "Accept": "application/json",
                                              "User-Agent": "playbox-hub"})
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                raw = r.read(2_000_000)
        except urllib.error.HTTPError as e:
            if e.code in (401, 403):
                raise IntegError(502, "Composio rejected the API key")
            raise IntegError(502, f"Composio returned {e.code}")
        except (OSError, urllib.error.URLError):
            raise IntegError(502, "couldn't reach Composio")
        try:
            return json.loads(raw or b"{}")
        except ValueError:
            raise IntegError(502, "Composio sent something unreadable")

    @staticmethod
    def _items(j):
        if isinstance(j, list):
            return j
        for k in ("items", "data", "results"):
            if isinstance(j.get(k), list):
                return j[k]
        return []

    def toolkits(self, q=""):
        query = {"limit": 100}
        if q:
            query["search"] = q
        out = []
        for t in self._items(self._req("GET", "/toolkits", query=query)):
            slug = t.get("slug") if isinstance(t, dict) else None
            if not slug or not SLUG.match(str(slug).lower()):
                continue
            meta = t.get("meta") or {}
            cats = [c.get("name") if isinstance(c, dict) else c for c in (meta.get("categories") or [])]
            out.append({"slug": str(slug).lower(), "name": str(t.get("name") or slug)[:60], "cats": [str(c)[:30] for c in cats if c][:3],
                        "noauth": bool(t.get("no_auth"))})
        return out

    def connect(self, user_id, toolkit, callback=None):
        sid = self._req("POST", "/tool_router/session", {"user_id": user_id}).get("session_id")
        if not sid or not ID.match(str(sid)):
            raise IntegError(502, "Composio didn't open a session")
        body = {"toolkit": toolkit}
        if callback:
            body["callback_url"] = callback
        url = self._req("POST", f"/tool_router/session/{sid}/link", body).get("redirect_url")
        if not isinstance(url, str) or not url.startswith("https://"):
            raise IntegError(502, "Composio didn't return a sign-in link")
        return url

    def accounts(self, user_id):
        out = []
        for a in self._items(self._req("GET", "/connected_accounts", query={"user_ids": user_id, "limit": 100})):
            if not isinstance(a, dict):
                continue
            tk = a.get("toolkit")
            slug = (tk.get("slug") if isinstance(tk, dict) else tk) or a.get("toolkit_slug")
            aid = a.get("id") or a.get("nanoid")
            if slug and aid and ID.match(str(aid)):
                out.append({"id": str(aid), "toolkit": str(slug).lower(), "status": str(a.get("status") or "").upper()})
        return out

    def tools(self, toolkit):
        out = []
        for t in self._items(self._req("GET", "/tools", query={"toolkit_slug": toolkit, "limit": 200})):
            if isinstance(t, dict) and isinstance(t.get("slug"), str):
                out.append({"slug": t["slug"], "description": str(t.get("description") or "")[:300], "input_parameters": t.get("input_parameters") or {}})
        return out

    def execute(self, tool, account, user_id, arguments):
        j = self._req("POST", "/tools/execute/" + urllib.parse.quote(tool, safe=""), {"connected_account_id": account, "user_id": user_id, "arguments": arguments})
        return {"successful": bool(j.get("successful", j.get("successfull", False))), "data": j.get("data"), "error": j.get("error")}

    def delete(self, account_id):
        self._req("DELETE", "/connected_accounts/" + urllib.parse.quote(account_id, safe=""))


class Integrations:
    def __init__(self, store):
        self.s = store
        self.keyfile = store.home / "composio.key"
        with store.lock:
            store.d.setdefault("grants", [])
            store.d.setdefault("agent_tokens", [])
            store.d.setdefault("audit", [])

    # ---- key ----
    def key(self):
        env = os.environ.get("COMPOSIO_API_KEY", "").strip()
        if env:
            return env
        try:
            return self.keyfile.read_text().strip()
        except OSError:
            return ""

    def set_key(self, k):
        k = (k or "").strip()
        if not (8 <= len(k) <= 200) or re.search(r"\s", k):
            raise IntegError(400, "that doesn't look like an API key")
        fd = os.open(self.keyfile, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write(k)
        os.chmod(self.keyfile, 0o600)

    def clear_key(self):
        try:
            self.keyfile.unlink()
        except OSError:
            pass

    def client(self):
        k = self.key()
        if not k:
            raise IntegError(409, "add your Composio API key first")
        return Composio(k)

    def user_id(self):
        return "playbox-" + self.s.d["user_id"]

    # ---- state ----
    def state(self):
        k = self.key()
        return {"configured": bool(k), "hint": ("…" + k[-4:]) if k else "", "from_env": bool(os.environ.get("COMPOSIO_API_KEY")),
                "grants": list(self.s.d["grants"]), "levels": list(LEVELS),
                "tokens": [{k: t[k] for k in ("id", "room", "label", "created", "last")} for t in self.s.d["agent_tokens"]],
                "audit": list(self.s.d["audit"][-15:])}

    # ---- grants ----
    def grant(self, room, toolkit, account, level, by=""):
        if level not in LEVELS:
            raise IntegError(400, "level must be read, write or admin")
        if not (SLUG.match(toolkit or "") and ID.match(account or "")):
            raise IntegError(400, "bad toolkit or account")
        with self.s.lock:
            gs = self.s.d["grants"]
            for g in gs:                           # one grant per room+account; granting again changes the level
                if g["room"] == room and g["account"] == account:
                    g["level"], g["by"], g["at"] = level, by, int(time.time())
                    self.s.save()
                    return g
            g = {"id": secrets.token_hex(6), "room": room, "toolkit": toolkit, "account": account, "level": level, "by": by, "at": int(time.time())}
            gs.append(g)
            self.s.save()
            return g

    def revoke(self, gid):
        with self.s.lock:
            n = len(self.s.d["grants"])
            self.s.d["grants"] = [g for g in self.s.d["grants"] if g["id"] != gid]
            self.s.save()
            return len(self.s.d["grants"]) < n

    def drop_account(self, account):
        with self.s.lock:
            self.s.d["grants"] = [g for g in self.s.d["grants"] if g["account"] != account]
            self.s.save()

    def drop_room(self, room):
        with self.s.lock:
            self.s.d["agent_tokens"] = [t for t in self.s.d["agent_tokens"] if t["room"] != room]
            self.s.d["grants"] = [g for g in self.s.d["grants"] if g["room"] != room]
            self.s.save()

    # ---- agent keys: a room-scoped bearer token an agent presents to the hub ----
    def mint_token(self, room, label=""):
        tid, secret = secrets.token_hex(6), secrets.token_urlsafe(32)
        with self.s.lock:
            if len(self.s.d["agent_tokens"]) >= 50:
                raise IntegError(409, "too many agent keys; revoke some first")
            self.s.d["agent_tokens"].append({"id": tid, "room": room, "label": str(label or "agent")[:40], "hash": hashlib.sha256(secret.encode()).hexdigest(),
                                             "created": int(time.time()), "last": 0})
            self.s.save()
        return f"pbx_{tid}.{secret}", tid

    def token_room(self, token):
        """The room a presented token belongs to, or None. Constant-time on the secret."""
        m = re.match(r"^pbx_([0-9a-f]{12})\.([A-Za-z0-9_\-]{20,80})$", token or "")
        if not m:
            return None
        h = hashlib.sha256(m.group(2).encode()).hexdigest()
        with self.s.lock:
            for t in self.s.d["agent_tokens"]:
                if t["id"] == m.group(1) and hmac.compare_digest(t["hash"], h):
                    t["last"] = int(time.time())
                    return t["room"]
        return None

    def revoke_token(self, tid):
        with self.s.lock:
            n = len(self.s.d["agent_tokens"])
            self.s.d["agent_tokens"] = [t for t in self.s.d["agent_tokens"] if t["id"] != tid]
            self.s.save()
            return len(self.s.d["agent_tokens"]) < n

    # ---- what a room's agent can do ----
    def room_grants(self, room):
        best = {}
        for g in self.s.d["grants"]:
            if g["room"] == room and (g["toolkit"] not in best or RANK[g["level"]] > RANK[best[g["toolkit"]]["level"]]):
                best[g["toolkit"]] = g
        return best

    def _log(self, room, toolkit, tool, level, outcome):
        with self.s.lock:
            self.s.d["audit"] = (self.s.d["audit"] + [{"t": int(time.time()), "room": room, "toolkit": toolkit, "tool": tool, "need": level, "out": outcome}])[-200:]
            self.s.save()

    def tools_for(self, room, toolkit):
        g = self.room_grants(room).get(toolkit)
        if not g:
            raise IntegError(403, f"this room has no access to {toolkit}")
        rows = []
        for t in self.client().tools(toolkit):
            need = classify(toolkit, t["slug"])
            if need:
                rows.append({**t, "needs": need, "allowed": RANK[need] <= RANK[g["level"]]})
        return {"toolkit": toolkit, "level": g["level"], "tools": rows}

    def run(self, room, toolkit, tool, arguments):
        if not SLUG.match(str(toolkit or "")) or not isinstance(arguments, dict) or len(json.dumps(arguments)) > 200_000:
            raise IntegError(400, "toolkit, tool and an arguments object are required")
        need = classify(toolkit, tool)
        if not need:
            raise IntegError(400, f"{tool!r} isn't a {toolkit} tool")
        g = self.room_grants(room).get(toolkit)
        if not g:
            self._log(room, toolkit, tool, need, "denied: no grant")
            raise IntegError(403, f"this room has no access to {toolkit}")
        if RANK[need] > RANK[g["level"]]:
            self._log(room, toolkit, tool, need, f"denied: grant is {g['level']}")
            raise IntegError(403, f"{tool} needs {need} access; this room has {g['level']} on {toolkit}")
        try:
            res = self.client().execute(tool, g["account"], self.user_id(), arguments)
        except IntegError as e:
            self._log(room, toolkit, tool, need, "error: " + e.msg)
            raise
        self._log(room, toolkit, tool, need, "ok" if res["successful"] else "failed")
        return res
