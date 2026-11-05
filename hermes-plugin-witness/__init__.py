"""Playbox witness: signed whole-session history and a pause on peer-triggered writes."""
import json
import os
import subprocess
from pathlib import Path

from .witness import Witness


def _record(entry):
    home = os.environ.get("PLAYBOX_HOME") or str(Path.home() / ".playbox")
    log = os.environ.get("PLAYBOX_LOG") or str(Path(home) / "history" / "hermes-session.log")
    Path(log).parent.mkdir(parents=True, exist_ok=True)
    try:
        subprocess.run(["playbox", "record", "--home", home, "--kind", "hermes_tool_call", "--log", log],
                       input=json.dumps(entry), text=True, capture_output=True, timeout=5, check=False)
    except (OSError, subprocess.SubprocessError):
        pass  # recording must never break the agent; `playbox verify` shows any gap


def register(ctx):
    w = Witness(_record)

    def pre_llm_call(**kw):
        w.new_turn(kw.get("session_id"))

    def trace(phase, kw):
        # Hermes scrubs the plugin host's environment, so debugging is switched on by creating this file.
        flag = Path.home() / ".playbox" / "witness.debug"
        if flag.exists():
            with open(flag, "a") as f:
                a = kw.get("args")
                f.write(f"{phase} {kw.get('tool_name')} args={list(a) if isinstance(a, dict) else type(a).__name__}\n")

    def pre_tool_call(**kw):
        trace("pre", kw)
        try:
            return w.before_tool(kw.get("session_id"), kw.get("tool_name"), kw.get("args"))
        except Exception:
            # Fail closed while a room's text is in context; hooks are otherwise fail-open.
            if w.tainted.get(kw.get("session_id")):
                return {"action": "block", "message": "Playbox witness could not check this call; ask the user."}
            return None

    def post_tool_call(**kw):
        trace("post", kw)
        w.after_tool(kw.get("session_id"), kw.get("tool_name"), kw.get("args"), kw.get("result"),
                     kw.get("status"), kw.get("duration_ms"))

    ctx.register_hook("pre_llm_call", pre_llm_call)
    ctx.register_hook("pre_tool_call", pre_tool_call)
    ctx.register_hook("post_tool_call", post_tool_call)
