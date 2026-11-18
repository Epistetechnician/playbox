"""Playbox witness: signed whole-session history and a pause on peer-triggered writes."""
import hashlib
import json
import os
import stat
import subprocess
import time
from pathlib import Path

from .witness import Witness


def _child_env(home):
    """The `playbox` child gets a minimal environment. Hooks run inside Hermes, whose environment holds every
    provider API key, and the child is a separately downloaded binary: it must not inherit them."""
    keep = ("PATH", "HOME", "LANG", "LC_ALL", "TMPDIR")
    env = {k: os.environ[k] for k in keep if k in os.environ}
    env.update({k: v for k, v in os.environ.items() if k.startswith("PLAYBOX_")})
    env["PLAYBOX_HOME"] = home
    return env


def _record(entry):
    home = os.environ.get("PLAYBOX_HOME") or str(Path.home() / ".playbox")
    log = os.environ.get("PLAYBOX_LOG") or str(Path(home) / "history" / "hermes-session.log")
    Path(log).parent.mkdir(parents=True, exist_ok=True)
    try:
        subprocess.run(["playbox", "record", "--home", home, "--kind", "hermes_tool_call", "--log", log],
                       input=json.dumps(entry), text=True, capture_output=True, timeout=5, check=False,
                       env=_child_env(home))
    except (OSError, subprocess.SubprocessError):
        pass  # recording must never break the agent; `playbox verify` shows any gap


def _event(e):
    """Append a hold or release to <home>/witness/holds.jsonl so the Playbox cockpit can show it. Tool name,
    origin label and session id only: no arguments, no peer text. Best effort, never raises."""
    try:
        home = Path(os.environ.get("PLAYBOX_HOME") or Path.home() / ".playbox")
        d = home / "witness"
        d.mkdir(parents=True, exist_ok=True, mode=0o700)
        with open(d / "holds.jsonl", "a") as f:
            f.write(json.dumps({"ts": int(time.time() * 1000), **e}) + "\n")
    except OSError:
        pass


def _take_decision(session_id):
    """Consume a single-use owner decision (`release` or `deny`) written by Playbox for `session_id`."""
    if not session_id:
        return None
    try:
        home = Path(os.environ.get("PLAYBOX_HOME") or Path.home() / ".playbox")
        p = home / "witness" / "decisions" / f"{hashlib.sha256(str(session_id).encode()).hexdigest()}.json"
        st = os.lstat(p)
        if not stat.S_ISREG(st.st_mode):
            return None
        if hasattr(os, "getuid") and ((st.st_mode & 0o777) != 0o600 or st.st_uid != os.getuid()):
            return None
        raw = p.read_text(encoding="utf-8")
        p.unlink(missing_ok=True)
        data = json.loads(raw)
        if isinstance(data, dict) and data.get("session") == session_id and data.get("decision") in ("release", "deny"):
            return data["decision"]
    except (OSError, ValueError):
        pass
    return None


def register(ctx):
    w = Witness(_record, _event, _take_decision)
    aliases = {}          # session_id -> conversation_id so hooks omitting conversation_id share state
    mid_tool_turn = set() # keys whose last event was post_tool_call (intra-turn LLM continuation)

    def key(kw):
        # Context compression rotates session_id; conversation_id survives it. Hermes 0.22.5 passes it to some
        # hooks and not others, so bind session_id to conversation_id whenever both are seen.
        cid, sid = kw.get("conversation_id"), kw.get("session_id")
        if cid and sid:
            aliases[sid] = cid
            if sid != cid:
                if sid in w.in_room:
                    w.in_room.add(cid)
                if sid in w.tainted and cid not in w.tainted:
                    w.tainted[cid] = w.tainted.pop(sid)
                if sid in w.denied:
                    w.denied.discard(sid)
                    w.denied.add(cid)
                if sid in w.local_read:
                    w.local_read.discard(sid)
                    w.local_read.add(cid)
                if sid in mid_tool_turn:
                    mid_tool_turn.discard(sid)
                    mid_tool_turn.add(cid)
        return cid or aliases.get(sid) or sid

    def pre_llm_call(**kw):
        k = key(kw)
        if k in mid_tool_turn:
            mid_tool_turn.discard(k)
            return
        w.new_turn(k)

    def trace(phase, kw):
        # Hermes scrubs the plugin host's environment, so debugging is switched on by creating this file.
        flag = Path.home() / ".playbox" / "witness.debug"
        if flag.exists():
            with open(flag, "a") as f:
                a = kw.get("args")
                f.write(f"{phase} {kw.get('tool_name')} args={list(a) if isinstance(a, dict) else type(a).__name__}\n")

    def pre_tool_call(**kw):
        trace("pre", kw)
        k = key(kw)
        try:
            return w.before_tool(k, kw.get("tool_name"), kw.get("args"))
        except Exception:
            # Fail closed while a room's text is in context; hooks are otherwise fail-open.
            if w.tainted.get(k) or k in w.denied:
                return {"action": "block", "message": "Playbox witness could not check this call; ask the user."}
            return None

    def post_tool_call(**kw):
        trace("post", kw)
        k = key(kw)
        mid_tool_turn.add(k)
        w.after_tool(k, kw.get("tool_name"), kw.get("args"), kw.get("result"),
                     kw.get("status"), kw.get("duration_ms"))

    ctx.register_hook("pre_llm_call", pre_llm_call)
    ctx.register_hook("pre_tool_call", pre_tool_call)
    ctx.register_hook("post_tool_call", post_tool_call)
