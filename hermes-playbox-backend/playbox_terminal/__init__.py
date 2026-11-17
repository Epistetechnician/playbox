"""Hermes terminal backend `playbox`: each command is a `playbox_delegate` task.

State model (see docs/hermes-playbox.md): every delegate runs against a fresh checkout of `base` in a fresh sandbox. The
backend therefore keeps the list of *edit commands* (commands that changed the tree, detected from the returned patch) and
replays them, in their original working directories, before each new command. It never pastes room output into a command,
so the Playbox taint check has nothing of the room's to object to unless the model itself copies task output into a command.

Config (environment, set by the harness; none of it is a credential):
  PLAYBOX_BIN        path to the playbox binary           (default: playbox)
  PLAYBOX_HOME       member home for this Hermes run      (required; gets the member key)
  PLAYBOX_LINK_FILE  file holding a one-time join link    (required)
  PLAYBOX_IMAGE      sandbox image                        (default: hermes-agent-nonroot:latest)
  PLAYBOX_BASE       base branch                          (default: main)
  PLAYBOX_STATS      JSON-lines file; one record per delegate (optional)
  PLAYBOX_WARM       "0" turns off the warm sandbox (default: on; needs a host that runs the openshell runtime)
  PLAYBOX_KEEP       "0" replays edit commands before every command instead of keeping the sandbox's files (default: keep; needs warm)
"""
from __future__ import annotations

import hashlib
import json
import os
import shlex
import subprocess
import sys
import threading
import time
import uuid

from agent.terminal_env_provider import TerminalEnvironmentProvider
from tools.environments.base import BaseEnvironment

SANDBOX_CWD = "/sandbox"
RATE_WAIT_STEP = 2.0     # seconds between retries when the host says the member is over its task rate limit
KEEP = os.environ.get("PLAYBOX_KEEP", "1") != "0" and os.environ.get("PLAYBOX_WARM", "1") != "0"
ALIVE = "/tmp/.pbx_alive"    # marker in the sandbox: present only while the member's warm sandbox, with its files, is still there
RATE_WAIT_MAX = 90.0     # give up after this long and show the refusal


class _Mcp:
    """Minimal MCP stdio client for `playbox mcp`."""

    def __init__(self, cmd, home):
        env = dict(os.environ, PLAYBOX_HOME=home)
        env.pop("PLAYBOX_LIVE", None)
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(os.environ["PLAYBOX_MCP_LOG"], "a") if os.environ.get("PLAYBOX_MCP_LOG") else subprocess.DEVNULL, env=env, text=True)
        self.i = 0
        self._rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "hermes-playbox-terminal", "version": "0.1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, m):
        self.p.stdin.write(json.dumps(m) + "\n")
        self.p.stdin.flush()

    def _rpc(self, method, params):
        self.i += 1
        self._send({"jsonrpc": "2.0", "id": self.i, "method": method, "params": params})
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("playbox mcp exited")
            r = json.loads(line)
            if r.get("id") == self.i:
                return r

    def call(self, name, args=None):
        r = self._rpc("tools/call", {"name": name, "arguments": args or {}})
        if "error" in r:
            raise RuntimeError(r["error"].get("message", "rpc error"))
        res = r["result"]
        text = (res.get("content") or [{}])[0].get("text", "")
        if res.get("isError"):
            raise RuntimeError(text or "tool error")
        try:
            return json.loads(text)
        except ValueError:
            return text

    def close(self):
        try:
            self.p.kill()
        except OSError:
            pass


class _Session:
    """One joined member and one workspace history per Hermes process. Hermes may create several environments (terminal, file tools,
    per task id); a join link is single-use, so they all share this."""
    _inst = None
    _guard = threading.Lock()

    @classmethod
    def get(cls):
        with cls._guard:
            if cls._inst is None:
                cls._inst = cls()
            return cls._inst

    def __init__(self):
        self.image = os.environ.get("PLAYBOX_IMAGE", "hermes-agent-nonroot:latest")
        self.base = os.environ.get("PLAYBOX_BASE", "main")
        self.stats_path = os.environ.get("PLAYBOX_STATS", "")
        link = open(os.environ["PLAYBOX_LINK_FILE"]).read().strip()
        for attempt in range(4):                   # the first join can time out while the host is still settling
            self.mcp = _Mcp([os.environ.get("PLAYBOX_BIN", "playbox"), "mcp"], os.environ["PLAYBOX_HOME"])
            try:
                self.mcp.call("playbox_join", {"link": link})
                break
            except RuntimeError:
                self.mcp.close()
                if attempt == 3 or "already used" in str(sys.exc_info()[1]):
                    raise
                time.sleep(3)
        self.started = False                       # a command has run in the member's sandbox (so `keep` may expect its files)
        self.edits: list[tuple[str, str]] = []     # (cwd, command) of every command that changed the tree
        self.patch_sig = hashlib.sha256(b"0").hexdigest()   # signature of the empty diff
        self.last_patch = ""                       # full text of the diff after the last edit (the MCP truncates at 20 kB)
        self.last_truncated = False
        self.lock = threading.Lock()

    def save_final(self):
        if self.stats_path:
            tmp = self.stats_path + ".final.tmp"
            with open(tmp, "w") as f:
                json.dump({"edits": self.edits, "patch": self.last_patch, "patch_truncated": self.last_truncated}, f)
            os.replace(tmp, self.stats_path.replace(".jsonl", "") + ".final.json")


class PlayboxEnvironment(BaseEnvironment):
    is_local = False
    uses_host_filesystem = False

    def __init__(self, cwd, timeout):
        super().__init__(cwd=SANDBOX_CWD, timeout=timeout)
        self.s = _Session.get()

    @property
    def edits(self):
        return self.s.edits

    # -- one delegate -----------------------------------------------------------------------------------------------
    def _script(self, command, cwd, replay, check):
        """`replay`: re-run every earlier edit command first (the sandbox starts from a fresh checkout). `check`: the sandbox is
        supposed to still hold the earlier edits; stop with a marker if it does not, so the caller can replay."""
        mark = uuid.uuid4().hex[:10]
        lines = ["export PYTHONDONTWRITEBYTECODE=1 GIT_PAGER=cat PAGER=cat"]
        if check:
            lines.append(f"[ -f {ALIVE} ] || {{ echo ___PBX_LOST___; exit 97; }}")
        if KEEP:
            lines.append(f"touch {ALIVE}")
        if replay:
            for ecwd, ecmd in self.edits:
                lines.append(f"( cd {shlex.quote(ecwd)} 2>/tmp/.pbx_err; {{\n{ecmd}\n}} ) >/tmp/.pbx_replay 2>&1 0<&-")
        lines.append(f"cd {shlex.quote(cwd)} 2>/tmp/.pbx_err || cd {SANDBOX_CWD}")
        lines.append(f"echo ___PBX_BEGIN_{mark}___")
        lines.append(f"{{\n{command}\n}} 2>&1")
        lines.append(f"__rc=$?; echo; echo ___PBX_END_{mark}___ $__rc \"$PWD\"")
        return "\n".join(lines), mark

    def _call(self, script, timeout, rec, t0):
        """One delegate, waiting out the host's rate limit. Returns (result, None) or (None, refusal output for the model)."""
        args = {"base": self.s.base, "command": script, "image": self.s.image,
                "timeout_secs": int(timeout or self.timeout or 120) + 30, "network": False,
                "warm": os.environ.get("PLAYBOX_WARM", "1") != "0", "keep": KEEP}
        waited = 0.0
        while True:
            try:
                r = self.s.mcp.call("playbox_delegate", args)
                break
            except Exception as e:  # noqa: BLE001 — a refusal is output the model can read
                msg = str(e)
                # With a warm sandbox commands are quick enough to hit the host's per-member task rate limit. That limit is
                # the host's to set; wait for the window to move on instead of showing the model a refusal it cannot act on.
                if "rate limit" in msg and waited < RATE_WAIT_MAX:
                    time.sleep(RATE_WAIT_STEP)
                    waited += RATE_WAIT_STEP
                    continue
                rec.update(refused=True, error=msg[:300], secs=round(time.time() - t0, 2), rate_waited_s=waited)
                self._log(rec)
                return None, {"output": f"[playbox refused this command: {msg[:600]}]", "returncode": 1}
        if waited:
            rec["rate_waited_s"] = rec.get("rate_waited_s", 0) + waited
        return r, None

    def execute(self, command, cwd="", *, timeout=None, stdin_data=None, **_):
        cwd = cwd or self.cwd or SANDBOX_CWD
        if stdin_data:
            command = f"{command} <<'__PBX_STDIN__'\n{stdin_data}\n__PBX_STDIN__"
        t_req = time.time()
        with self.s.lock:
            t0 = time.time()      # per-command time excludes the wait for another in-flight command
            script, mark = self._script(command, cwd, replay=not KEEP, check=KEEP and self.s.started)
            rec = {"t": round(t0, 3), "queued_s": round(t0 - t_req, 2), "replayed": 0 if KEEP else len(self.s.edits), "cmd_bytes": len(script)}
            r, refusal = self._call(script, timeout, rec, t0)
            if refusal:
                return refusal
            if KEEP and "___PBX_LOST___" in ((r.get("stdout") or "") + (r.get("stderr") or "")):
                # The sandbox was dropped (idle too long, a timeout, a restart): start again from a fresh checkout and replay.
                script, mark = self._script(command, cwd, replay=True, check=False)
                rec.update(recovered=True, replayed=len(self.s.edits), cmd_bytes=len(script))
                r, refusal = self._call(script, timeout, rec, t0)
                if refusal:
                    return refusal
            self.s.started = True
            out = (r.get("stdout") or "") + (r.get("stderr") or "")
            rc, newcwd = r.get("exit_code") or 0, cwd
            b, e = f"___PBX_BEGIN_{mark}___\n", f"___PBX_END_{mark}___"
            if b in out and e in out:
                body = out.split(b, 1)[1]
                body, tail = body.rsplit(e, 1)
                parts = tail.strip().split(" ", 1)
                try:
                    rc = int(parts[0])
                    newcwd = parts[1].strip() if len(parts) > 1 and parts[1].strip() else cwd
                except ValueError:
                    pass
                out = body.rstrip("\n") + ("\n" if body.strip() else "")
            elif r.get("timed_out"):
                rc = 124
            else:
                rec["no_markers"] = out[-400:]
            sig = hashlib.sha256(((r.get("patch") or "") + str(r.get("patch_bytes"))).encode()).hexdigest()
            edited = sig != self.s.patch_sig
            if edited:
                self.s.edits.append((cwd, command))
                self.s.patch_sig = sig
                self.s.last_patch, self.s.last_truncated = r.get("patch") or "", bool(r.get("patch_truncated"))
                self.s.save_final()
            self.cwd = newcwd
            rec.update(secs=round(time.time() - t0, 2), rc=rc, edited=edited, task=r.get("task_id"), runner=r.get("runner"),
                       admission=r.get("admission"), patch_bytes=r.get("patch_bytes"), timed_out=bool(r.get("timed_out")))
            self._log(rec)
        return {"output": out, "returncode": rc}

    def _log(self, rec):
        if self.s.stats_path:
            with open(self.s.stats_path, "a") as f:
                f.write(json.dumps(rec) + "\n")

    def cleanup(self):
        self.s.save_final()      # the shared member stays joined until the process exits


class PlayboxProvider(TerminalEnvironmentProvider):
    name = "playbox"
    display_name = "Playbox room (OpenShell microVM)"
    is_remote = True
    is_container = True
    default_cwd = SANDBOX_CWD

    def is_available(self):
        return bool(os.environ.get("PLAYBOX_LINK_FILE") and os.environ.get("PLAYBOX_HOME"))

    def create_environment(self, *, cwd, timeout, task_id="default", image=None, container_config=None, **kw):
        return PlayboxEnvironment(cwd, timeout)


def register(ctx):
    ctx.register_terminal_environment_provider(PlayboxProvider())
