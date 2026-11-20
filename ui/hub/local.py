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
        while not buf.endswith(b"\n") and len(buf) < 262144:
            chunk = c.recv(65536)
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


def prune_stale():
    """Remove the records and sockets of hosts that are gone (killed without a chance to clean up). A record goes only when its process is
    dead AND its socket does not answer; returns how many were removed."""
    d, n = hosting_dir(), 0
    try:
        files = sorted(d.glob("*.json"))
    except OSError:
        return 0
    for f in files:
        try:
            rec = json.loads(f.read_text())
            hid = str(rec.get("id", ""))
            if not re.fullmatch(r"[0-9a-f]{16}", hid) or f.stem != hid:
                continue
            pid = rec.get("pid")
            if isinstance(pid, int) and pid > 0:
                try:
                    os.kill(pid, 0)
                    continue                      # still running: leave it, even if it is slow to answer
                except ProcessLookupError:
                    pass
                except PermissionError:
                    continue
            try:
                if ask_host(d / f"{hid}.sock", "ping", 1.0) == "ok":
                    continue
            except OSError:
                pass
            for x in (f, d / f"{hid}.sock"):
                try:
                    x.unlink()
                except FileNotFoundError:
                    pass
            n += 1
        except (OSError, ValueError):
            continue
    return n


CALL_ID = re.compile(r"[A-Za-z0-9_./:-]{1,200}")


def pending_calls():
    """Capability calls waiting for the owner on every host on this machine: [{host, name, call, member, cap, args, created_ms}].
    `call` is what `approve`/`deny` take. A host that is gone, slow or too old to know `calls` is skipped."""
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
            calls = json.loads(ask_host(d / f"{hid}.sock", "calls", 2.0))
            if not isinstance(calls, list):
                continue
            for c in calls[:50]:
                cid, mem = str(c.get("id", "")), str(c.get("member", ""))
                if not CALL_ID.fullmatch(cid) or not CALL_ID.fullmatch(mem):
                    continue
                args = {str(k)[:60]: str(v)[:300] for k, v in list((c.get("args") or {}).items())[:12]}
                out.append({"host": hid, "name": str(rec.get("name") or "room")[:60], "call": f"{mem}/{cid}", "member": mem, "cap": str(c.get("name", ""))[:80],
                            "args": args, "created_ms": c.get("created_ms")})
        except (OSError, ValueError, AttributeError):
            continue
    return out


def decide_call(hid, call, approve):
    """Approve or deny one waiting call on host `hid`. Returns (ok, message). Never raises on a missing host."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or "") or not CALL_ID.fullmatch(call or ""):
        return False, "bad call id"
    try:
        r = ask_host(hosting_dir() / f"{hid}.sock", ("approve " if approve else "deny ") + call, 6.0)
    except OSError as e:
        return False, f"the room's host did not answer: {e}"
    return r.startswith("ok"), r[:200]


def host_policy(hid):
    """Read live policy guardrails and budget burn from a local host socket."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or ""):
        return None
    try:
        raw = ask_host(hosting_dir() / f"{hid}.sock", "policy", 3.0)
        d = json.loads(raw)
        return d if isinstance(d, dict) else None
    except (OSError, ValueError):
        return None


def host_set_policy(hid, patch):
    """Update live policy guardrails on a local host socket. Returns (ok, dict_or_error_str)."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or "") or not isinstance(patch, dict):
        return False, "bad host or policy patch"
    try:
        raw = ask_host(hosting_dir() / f"{hid}.sock", "policy_set " + json.dumps(patch, separators=(",", ":")), 5.0)
    except OSError as e:
        return False, f"the room's host did not answer: {e}"
    if raw.startswith("error:"):
        return False, raw[6:].strip()[:200]
    try:
        return True, json.loads(raw)
    except ValueError:
        return False, raw[:200]


def host_memory(hid):
    """Read shared memory ledger from a local host socket."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or ""):
        return None
    try:
        raw = ask_host(hosting_dir() / f"{hid}.sock", "memory", 3.0)
        d = json.loads(raw)
        return d if isinstance(d, dict) else None
    except (OSError, ValueError):
        return None


def host_memory_act(hid, req):
    """Run a memory action (endorse, accept, quarantine, remember) on a local host socket."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or "") or not isinstance(req, dict):
        return False, "bad host or memory action"
    try:
        raw = ask_host(hosting_dir() / f"{hid}.sock", "memory_act " + json.dumps(req, separators=(",", ":")), 5.0)
    except OSError as e:
        return False, f"the room's host did not answer: {e}"
    if raw.startswith("error:"):
        return False, raw[6:].strip()[:200]
    try:
        return True, json.loads(raw)
    except ValueError:
        return False, raw[:200]


def host_replay(hid, task_id=None, run=False):
    """Inspect or replay a task on a local host socket."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or ""):
        return False, "bad host id"
    tid = re.sub(r"[^A-Za-z0-9_.:-]", "", str(task_id or ""))[:80]
    cmd = "replay" + (f" {tid}" + (" run" if run else "") if tid else "")
    try:
        raw = ask_host(hosting_dir() / f"{hid}.sock", cmd, 60.0 if run else 5.0)
    except OSError as e:
        return False, f"the room's host did not answer: {e}"
    if raw.startswith("error:"):
        return False, raw[6:].strip()[:200]
    try:
        return True, json.loads(raw)
    except ValueError:
        return False, raw[:200]


def host_handoffs(hid, cid=None):
    """Read structured handoff checkpoints from a local host socket."""
    if not re.fullmatch(r"[0-9a-f]{16}", hid or ""):
        return None
    tid = re.sub(r"[^A-Za-z0-9_.:-]", "", str(cid or ""))[:80]
    try:
        raw = ask_host(hosting_dir() / f"{hid}.sock", "handoffs" + (f" {tid}" if tid else ""), 3.0)
        d = json.loads(raw)
        return d if isinstance(d, dict) else None
    except (OSError, ValueError):
        return None
