"""Talk to a Playbox room from Python: start a host, join members, send and read signed hub messages.

The room itself is the Rust `playbox` binary (`playbox up --local --console`, `playbox latent session`); this module only
owns the keys and the pipes. A member's key file is its room identity, so the same 32-byte seed signs the `PBL2`
envelopes (`hub.pack_signed`) and is what the host checks the signer against.

Guard: `guard_blocks` is the receiver's own check on a decoded vector, the same rule `engine.check_guards` applies to a
steering direction (|cosine| >= 0.5 with a listed safety direction of a denied class). The host does not do this for a
latent message because it never decodes one.
"""
from __future__ import annotations

import base64
import json
import os
import subprocess
from pathlib import Path

import torch

GUARD_COS_THRESHOLD = 0.5  # same as engine.GUARD_COS_THRESHOLD


def new_key(path: Path) -> tuple[object, str]:
    """Write a fresh 32-byte identity seed to `path`; returns the cryptography private key and its public key (hex)."""
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    seed = os.urandom(32)
    path.write_bytes(seed)
    path.chmod(0o600)
    key = Ed25519PrivateKey.from_private_bytes(seed)
    pub = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    return key, pub


class Room:
    """A local room host. `new_link()` mints one single-use join link.

    With `state` the host keeps its identity, members and latent mailbox in that directory, so `restart()` brings back
    the same room (new address, so members join again with a fresh link and their old key). `extra` is passed to
    `playbox up` (for example `--latent-hub HUB`).
    """

    def __init__(self, binary: Path, home: Path, state: Path | None = None, extra: list[str] | None = None):
        self.binary, self.home, self.state, self.extra = binary, home, state, list(extra or [])
        self._start()

    def _start(self) -> None:
        env = {**os.environ, "PLAYBOX_HOME": str(self.home)}
        args = [str(self.binary), "up", "--local", "--console", *self.extra]
        if self.state is not None:
            args += ["--state", str(self.state)]
        self.p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                  env=env, bufsize=1)
        self.first = self._link()

    def restart(self) -> None:
        """Stop the host and start it again on the same state directory."""
        if self.state is None:
            raise RuntimeError("a room without a state directory has nothing to restore")
        self.close()
        self._start()

    def _link(self) -> str:
        for line in self.p.stdout:
            if line.startswith("playbox://"):
                return line.strip()
        raise RuntimeError("the host exited before printing a link")

    def new_link(self) -> str:
        if self.first:
            link, self.first = self.first, None
            return link
        self.p.stdin.write("link\n")
        self.p.stdin.flush()
        return self._link()

    def close(self) -> None:
        try:
            self.p.stdin.write("quit\n")
            self.p.stdin.flush()
            self.p.wait(timeout=15)
        except Exception:
            self.p.kill()


class Member:
    """A room member: `playbox latent session`, one JSON request per line."""

    def __init__(self, binary: Path, home: Path, link: str, key_file: Path):
        env = {**os.environ, "PLAYBOX_HOME": str(home)}
        self.p = subprocess.Popen([str(binary), "latent", "session", link, "--local", "--key", str(key_file)],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                  env=env, bufsize=1)
        ready = json.loads(self.p.stdout.readline())
        self.id = ready["id"]

    def _call(self, req: dict) -> dict:
        self.p.stdin.write(json.dumps(req) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def send(self, envelope: bytes) -> int:
        r = self._call({"send": base64.b64encode(envelope).decode()})
        if not r["ok"]:
            raise RuntimeError(r["error"])
        return r["seq"]

    def say(self, text: str) -> int:
        r = self._call({"say": text})
        if not r["ok"]:
            raise RuntimeError(r["error"])
        return r["seq"]

    def reveal(self, seq: int, reveal: bytes) -> None:
        r = self._call({"reveal": base64.b64encode(reveal).decode(), "seq": seq})
        if not r["ok"]:
            raise RuntimeError(r["error"])

    def pull(self, hub_id: str, since: int = 0) -> tuple[list[dict], int]:
        r = self._call({"pull": hub_id, "since": since})
        if not r["ok"]:
            raise RuntimeError(r["error"])
        return [{**i, "envelope": base64.b64decode(i["envelope"]),
                 "reveal": base64.b64decode(i["reveal"]) if i.get("reveal") else None} for i in r["items"]], r["next"]

    def feed(self, since: int = 0) -> tuple[list[str], int]:
        r = self._call({"feed": since})
        return r["lines"], r["next"]

    def close(self) -> None:
        try:
            self.p.stdin.close()
            self.p.wait(timeout=15)
        except Exception:
            self.p.kill()


def guard_blocks(vec: torch.Tensor, guards: dict[str, tuple[torch.Tensor, str]], deny: set[str]) -> list[str]:
    """Names of denied-class guards whose direction is within the threshold of `vec` (receiver space). Empty means pass."""
    v = torch.nn.functional.normalize(vec.float(), dim=-1)
    return [name for name, (d, cls) in guards.items()
            if cls in deny and abs(float(v @ torch.nn.functional.normalize(d.float(), dim=-1))) >= GUARD_COS_THRESHOLD]
