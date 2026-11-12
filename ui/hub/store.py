"""Hub state: one JSON file, 0600, replaced atomically. Holds passkeys (public halves), hashed sessions, hashed
pairing codes, the room registry, push subscriptions and the VAPID key. No room content and no secrets that
could be replayed: session tokens and pairing codes are stored only as SHA-256."""
import json
import os
import tempfile
import threading
import time
from pathlib import Path

from . import p256
from .webauthn import b64u


class Store:
    def __init__(self, home):
        self.home = Path(home).expanduser().resolve()
        self.home.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(self.home, 0o700)
        self.path = self.home / "hub.json"
        self.lock = threading.RLock()
        try:
            self.d = json.loads(self.path.read_text())
        except (OSError, ValueError):
            self.d = {}
        self.d.setdefault("v", 1)
        for k, v in (("creds", []), ("sessions", {}), ("pair", {}), ("rooms", []), ("push", [])):
            self.d.setdefault(k, v)
        if "vapid" not in self.d:
            priv, pub = p256.generate()
            self.d["vapid"] = {"d": f"{priv:064x}", "pub": b64u(p256.raw_pub(pub))}
        if "user_id" not in self.d:
            self.d["user_id"] = b64u(os.urandom(16))
        self.save()

    def save(self):
        with self.lock:
            fd, tmp = tempfile.mkstemp(dir=self.home, prefix=".hub-")
            with os.fdopen(fd, "w") as f:
                json.dump(self.d, f, indent=1)
            os.chmod(tmp, 0o600)
            os.replace(tmp, self.path)

    def gc(self, now=None):
        now = now or time.time()
        with self.lock:
            self.d["pair"] = {k: v for k, v in self.d["pair"].items() if v["exp"] > now}
            self.d["sessions"] = {k: v for k, v in self.d["sessions"].items() if v["last"] + SESSION_TTL > now}


SESSION_TTL = 30 * 86400
