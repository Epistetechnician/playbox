"""Rooms hosted on this machine by `playbox up --state DIR` (see crates/playbox-cli/src/control.rs): find them, ask them for a join link."""
import json
import os
import re
from pathlib import Path


def hosting_dir():
    """Where `playbox up --state DIR` publishes the rooms it hosts on this machine (see crates/playbox-cli/src/control.rs)."""
    return Path(os.environ.get("PLAYBOX_HOSTING_DIR") or Path.home() / ".playbox" / "hosting")


def ask_host(sock, line, timeout=6.0):
    """One line to a host's private control socket, one line back. Raises OSError if the host is gone."""
    import socket as _s
    with _s.socket(_s.AF_UNIX, _s.SOCK_STREAM) as c:
        c.settimeout(timeout)
        c.connect(str(sock))
        c.sendall((line + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n") and len(buf) < 8192:
            chunk = c.recv(4096)
            if not chunk:
                break
            buf += chunk
    return buf.decode(errors="replace").strip()


def local_hosts():
    """Rooms hosted on this machine that are alive right now: [{id, name, repo, started_ms}]. A record whose socket does not answer is skipped."""
    out = []
    d = hosting_dir()
    try:
        files = sorted(d.glob("*.json"))
    except OSError:
        return out
    for f in files:
        try:
            rec = json.loads(f.read_text())
            hid = str(rec.get("id", ""))
            if not re.fullmatch(r"[0-9a-f]{16}", hid) or f.stem != hid:
                continue
            if ask_host(d / f"{hid}.sock", "ping", 2.0) != "ok":
                continue
            out.append({"id": hid, "name": str(rec.get("name") or "room")[:60], "repo": rec.get("repo"), "started_ms": rec.get("started_ms")})
        except (OSError, ValueError):
            continue
    return out
