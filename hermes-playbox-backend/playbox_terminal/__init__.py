"""Hermes terminal backend `playbox`: governed execution over a Playbox room (`openshell`, `libkrun`, or `docker`).

Two state modes:
- **Stateful session (`PLAYBOX_KEEP=1`, default):** every command runs via `playbox_delegate` with `keep=true`,
  preserving `/sandbox` (on `openshell`) and the host session worktree (on `libkrun`/`docker`/`openshell`) across
  commands for this authenticated member on `base`. Zero full-repo re-uploads, zero edit-command replay, and zero
  replay-induced false taint advisories. Pure read-only commands use `sync="none"`; mutating/unknown commands use
  `sync="delta"`; `export_patch()` / `cleanup()` runs one authoritative `sync="full"` + `full_patch=true` snapshot.
- **Legacy replay (`PLAYBOX_KEEP=0`):** stateless fallback that replays prior edit commands on a fresh checkout
  (kept for A/B parity benchmarking against earlier Arm P runs).

Config (environment; none of it is a credential):
  PLAYBOX_BIN        path to the playbox binary             (default: playbox)
  PLAYBOX_HOME       member home for this Hermes run        (required; gets the member key)
  PLAYBOX_LINK_FILE  file holding a one-time join link      (required)
  PLAYBOX_IMAGE      sandbox image                          (default: hermes-agent-nonroot:latest)
  PLAYBOX_BASE       base branch                            (default: main)
  PLAYBOX_STATS      JSON-lines file; one record/delegate   (optional)
  PLAYBOX_WARM       "0" turns off warm sandbox reuse       (default: "1")
  PLAYBOX_KEEP       "0" falls back to legacy edit replay   (default: "1")
  PLAYBOX_EGRESS     comma-separated BOX-7 egress names     (optional; host injects credentials outside sandbox)
  PLAYBOX_HOSTS      comma-separated allowed_hosts          (optional; default empty = no network)
"""
from __future__ import annotations

import hashlib
import json
import logging
import os
import re
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
RATE_WAIT_MAX = 90.0     # give up after this long and show the refusal

# Strict allowlist of guaranteed read-only commands eligible for `sync="none"` (zero host mirror overhead).
_READONLY_CMDS = frozenset({
    "cat", "head", "tail", "nl", "ls", "ll", "dir", "find", "rg", "grep", "egrep", "fgrep",
    "wc", "stat", "file", "pwd", "which", "command", "type", "uname", "id", "whoami", "env",
    "printenv", "tree", "realpath", "readlink", "basename", "dirname", "true", "false",
    "echo", "printf", "[", "test", ":", "base64", "cut", "uniq", "tr", "sha256sum", "shasum", "md5sum",
})
_READONLY_GIT_SUBS = frozenset({
    "status", "diff", "log", "show", "rev-parse", "ls-files", "branch", "tag", "blame", "grep",
})
_SHELL_KEYWORDS = frozenset({"if", "then", "elif", "else", "fi", "{", "}", "!"})
# Exact harmless null/fd-duplication redirects used by POSIX read probes (e.g., `2>/dev/null`, `>/dev/null 2>&1`).
_SAFE_REDIRECT = re.compile(r"(?:(?<=^)|(?<=[\s;|&({]))(?:[012]?>\s*/dev/null|[12]>&[12])(?=$|[\s;|&)}])")
# Any remaining output/rw redirection, subshell substitution, or mutating command forces `delta`.
_WRITE_METACHARS = re.compile(
    r"(?:>>?|<<|<>|<\(|\btee\b|\bsed\s+-i|\brm\b|\bmv\b|\bcp\b|\bmkdir\b|\btouch\b|\bchmod\b|\bchown\b|\bln\b|\bdd\b|\btruncate\b|\bpatch\b|`|\$\()"
)
_ENV_ASSIGN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=\S*$")
_SAFE_SED_SCRIPT = re.compile(r"^\d+(?:,\d+|\$)?p$")


def classify_sync_mode(command: str, stdin_data: str | None = None, keep: bool = True) -> str:
    """Choose the minimal safe `sync` mode (`none`, `delta`, or `full`) for `command`.

    Conservative by design: returns `"none"` only when every pipeline/chain segment (including
    single-line POSIX read probes such as Hermes' `read_file` / `search_files` probes with
    `2>/dev/null` and `if [ -f ... ]; then ... fi`) is provably a read-only inspection command
    with no file-writing redirections, subshells, or stdin payloads. Everything else (including
    `python`, `pytest`, file writes, `sed -i`, `sort -o`, `rg --pre`, `git apply`) uses `"delta"`
    under `keep=True` or `"full"` otherwise.
    """
    if not keep:
        return "full"
    if stdin_data:
        return "delta"
    cmd = (command or "").strip()
    if not cmd or "\n" in cmd:
        return "delta"
    # Strip ONLY exact `/dev/null` and fd-dup redirects (`2>/dev/null`, `2>&1`) before checking for `>`.
    cleaned = _SAFE_REDIRECT.sub(" ", cmd)
    if _WRITE_METACHARS.search(cleaned):
        return "delta"
    # Split on safe shell chaining/piping operators (`&&`, `||`, `;`, `|`).
    segments = [seg.strip() for seg in re.split(r"&&|\|\||[;|]", cleaned) if seg.strip()]
    if not segments:
        return "delta"
    for seg in segments:
        try:
            tokens = shlex.split(seg)
        except ValueError:
            return "delta"
        # Strip leading/trailing POSIX control keywords (`if`, `then`, `elif`, `else`, `fi`, `{`, `}`).
        while tokens and tokens[0] in _SHELL_KEYWORDS:
            tokens.pop(0)
        while tokens and tokens[-1] in ("}", "fi"):
            tokens.pop()
        idx = 0
        while idx < len(tokens) and _ENV_ASSIGN.match(tokens[idx]):
            idx += 1
        if idx >= len(tokens):
            continue
        prog = os.path.basename(tokens[idx])
        rest = tokens[idx + 1:]
        if prog == "cd":
            continue
        if prog == "set":
            # Hermes read/search probes use `{ set +x; } 2>/dev/null;` to silence xtrace.
            if rest != ["+x"]:
                return "delta"
            continue
        if prog == "git":
            # Skip leading `-C <dir>` / `--no-pager` flags before the git subcommand.
            j = 0
            while j < len(rest):
                if rest[j] in ("-C", "-c") and j + 1 < len(rest):
                    j += 2
                elif rest[j].startswith("-"):
                    j += 1
                else:
                    break
            if j >= len(rest) or rest[j] not in _READONLY_GIT_SUBS:
                return "delta"
            continue
        if prog == "sed":
            # Allow only safe `-n '<start>,<end>p'` line-print invocations (reject `-i`, `w`, `e`, etc.).
            if len(rest) < 3 or rest[0] != "-n" or not _SAFE_SED_SCRIPT.match(rest[1]):
                return "delta"
            continue
        if prog == "find":
            if any(t in ("-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls") for t in rest):
                return "delta"
            continue
        if prog == "sort":
            if any(t == "-o" or t.startswith("-o") or t.startswith("--output") for t in rest):
                return "delta"
            continue
        if prog in ("rg", "grep", "egrep", "fgrep"):
            if any(t == "--pre" or t.startswith("--pre=") for t in rest):
                return "delta"
            continue
        if prog == "sort" or prog in _READONLY_CMDS:
            continue
        return "delta"
    return "none"


def _csv_env(name: str) -> list[str]:
    raw = os.environ.get(name, "").strip()
    if not raw:
        return []
    return [x.strip() for x in raw.split(",") if x.strip()]


class _Mcp:
    """Minimal MCP stdio client for `playbox mcp`."""

    def __init__(self, cmd, home):
        env = dict(os.environ, PLAYBOX_HOME=home)
        env.pop("PLAYBOX_LIVE", None)
        env.setdefault("PLAYBOX_TRUST_OFFLINE_WORKSPACE", "1")
        stderr_f = open(os.environ["PLAYBOX_MCP_LOG"], "a") if os.environ.get("PLAYBOX_MCP_LOG") else subprocess.DEVNULL
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr_f, env=env, text=True)
        self.i = 0
        self._rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "hermes-playbox-terminal", "version": "0.2"}})
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
    # Safe defaults when tests bypass __init__ with __new__.
    keep = False
    egress: list[str] = []
    allowed_hosts: list[str] = []
    dirty = False
    runner_name = ""
    policy_digest = ""

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
        self.keep = os.environ.get("PLAYBOX_KEEP", "1") != "0"
        self.egress = _csv_env("PLAYBOX_EGRESS")
        self.allowed_hosts = _csv_env("PLAYBOX_HOSTS")
        link = open(os.environ["PLAYBOX_LINK_FILE"]).read().strip()
        for attempt in range(4):                   # the first join can time out while the host is still settling
            self.mcp = _Mcp([os.environ.get("PLAYBOX_BIN", "playbox"), "mcp"], os.environ["PLAYBOX_HOME"])
            try:
                self.join_info = self.mcp.call("playbox_join", {"link": link})
                break
            except RuntimeError:
                self.mcp.close()
                if attempt == 3 or "already used" in str(sys.exc_info()[1]):
                    raise
                time.sleep(3)
        self.edits: list[tuple[str, str]] = []     # (cwd, command) of every command that changed the tree (legacy replay only)
        self.patch_sig = hashlib.sha256(b"0").hexdigest()   # signature of the empty diff
        self.last_patch = ""                       # full text of the diff after the last edit
        self.last_truncated = False
        self.dirty = False                         # whether any write/delta command ran since last full export
        self.runner_name = ""
        self.policy_digest = ""
        self.lock = threading.Lock()

    def save_final(self):
        if self.stats_path:
            tmp = self.stats_path + ".final.tmp"
            with open(tmp, "w") as f:
                json.dump({
                    "keep": self.keep,
                    "runner": self.runner_name,
                    "policy_digest": self.policy_digest,
                    "edits": self.edits,
                    "patch": self.last_patch,
                    "patch_truncated": self.last_truncated,
                }, f)
            os.replace(tmp, self.stats_path.replace(".jsonl", "") + ".final.json")


def with_stdin(command, data):
    """Give `command` the text `data` on stdin, byte for byte, in the current shell (so `cd` / `$PWD` changes survive).
    A heredoc on a brace group wraps the whole command so multi-statement scripts (`...; cat > "$tmp"; ...; mv ...`) read stdin at
    the right statement, with a collision-free delimiter. A heredoc always ends in a newline, so data without one is staged to an
    immediately-unlinked temp descriptor decoded from base64 (never a pipeline, which would run the brace group in a subshell)."""
    if data.endswith("\n"):
        delim = "__PBX_STDIN__"
        while delim in data:
            delim += "_X"
        return f"{{\n{command}\n}} <<'{delim}'\n{data}{delim}"
    import base64
    b64 = base64.b64encode(data.encode("utf-8", "surrogateescape")).decode()
    return (
        f'__pbx_in="$(mktemp /tmp/.pbx_in.XXXXXX)" && printf %s {b64} | base64 -d > "$__pbx_in" '
        f'&& exec 9< "$__pbx_in" && rm -f "$__pbx_in" && {{\n{command}\n}} <&9 9<&-'
    )


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
    def _script(self, command, cwd):
        mark = uuid.uuid4().hex[:10]
        lines = ["export PYTHONDONTWRITEBYTECODE=1 GIT_PAGER=cat PAGER=cat"]
        if not self.s.keep:
            for ecwd, ecmd in self.edits:
                lines.append(f"( cd {shlex.quote(ecwd)} 2>/tmp/.pbx_err; {{\n{ecmd}\n}} ) >/tmp/.pbx_replay 2>&1 0<&-")
        lines.append(f"cd {shlex.quote(cwd)} 2>/tmp/.pbx_err || cd {SANDBOX_CWD}")
        lines.append(f"echo ___PBX_BEGIN_{mark}___")
        lines.append(f"{{\n{command}\n}} 2>&1")
        lines.append(f"__rc=$?; echo; echo ___PBX_END_{mark}___ $__rc \"$PWD\"")
        return "\n".join(lines), mark

    def _delegate_args(self, script: str, timeout: float | None, sync: str, full_patch: bool = False) -> dict:
        warm = os.environ.get("PLAYBOX_WARM", "1") != "0"
        args = {
            "base": self.s.base,
            "command": script,
            "image": self.s.image,
            "timeout_secs": int(timeout or self.timeout or 120) + 30,
            "network": False,
            "warm": warm,
            "keep": self.s.keep,
            "sync": sync,
            "full_patch": full_patch,
        }
        if self.s.allowed_hosts:
            args["allowed_hosts"] = list(self.s.allowed_hosts)
        if self.s.egress:
            args["egress"] = list(self.s.egress)
        return args

    def _call_with_rate_retry(self, args: dict):
        waited = 0.0
        while True:
            try:
                return self.s.mcp.call("playbox_delegate", args), waited
            except Exception as e:  # noqa: BLE001
                msg = str(e)
                if "rate limit" in msg and waited < RATE_WAIT_MAX:
                    time.sleep(RATE_WAIT_STEP)
                    waited += RATE_WAIT_STEP
                    continue
                raise

    def execute(self, command, cwd="", *, timeout=None, stdin_data=None, **_):
        cwd = cwd or self.cwd or SANDBOX_CWD
        sync = classify_sync_mode(command, stdin_data=stdin_data, keep=self.s.keep)
        if stdin_data:
            command = with_stdin(command, stdin_data)
        script, mark = self._script(command, cwd)
        t_req = time.time()
        with self.s.lock:
            t0 = time.time()      # per-command time excludes the wait for another in-flight command
            rec = {
                "t": round(t0, 3),
                "queued_s": round(t0 - t_req, 2),
                "keep": self.s.keep,
                "sync": sync,
                "replayed": 0 if self.s.keep else len(self.s.edits),
                "cmd_bytes": len(script),
            }
            args = self._delegate_args(script, timeout, sync=sync, full_patch=self.s.keep and sync != "none")
            try:
                r, waited = self._call_with_rate_retry(args)
            except Exception as e:  # noqa: BLE001 — a refusal is output the model can read
                msg = str(e)
                rec.update(refused=True, error=msg[:300], secs=round(time.time() - t0, 2))
                self._log(rec)
                return {"output": f"[playbox refused this command: {msg[:600]}]", "returncode": 1}
            if waited:
                rec["rate_waited_s"] = waited
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
            if r.get("runner"):
                self.s.runner_name = r["runner"]
            if r.get("policy_digest"):
                self.s.policy_digest = r["policy_digest"]
            edited = False
            if sync != "none":
                sig = hashlib.sha256(((r.get("patch") or "") + str(r.get("patch_bytes"))).encode()).hexdigest()
                edited = sig != self.s.patch_sig
                if edited:
                    if not self.s.keep:
                        self.s.edits.append((cwd, command))
                    self.s.patch_sig = sig
                    self.s.last_patch = r.get("patch") or ""
                    self.s.last_truncated = bool(r.get("patch_truncated"))
                    self.s.dirty = True
                    self.s.save_final()
            self.cwd = newcwd
            rec.update(
                secs=round(time.time() - t0, 2),
                rc=rc,
                edited=edited,
                task=r.get("task_id"),
                runner=r.get("runner"),
                admission=r.get("admission"),
                patch_bytes=r.get("patch_bytes"),
                timed_out=bool(r.get("timed_out")),
            )
            self._log(rec)
            self._publish_telemetry(rec)
        return {"output": out, "returncode": rc}

    def _publish_telemetry(self, rec):
        """Publish safe, throttled ::pbx metric and ::pbx artifact updates to the room feed (never raw commands or stdout)."""
        if os.environ.get("PLAYBOX_FEED_TELEMETRY", "1").lower() in ("0", "false", "off"):
            return
        if not getattr(self.s, "join_info", None):
            return
        now = time.time()
        self.s._cmd_n = getattr(self.s, "_cmd_n", 0) + 1
        rc_val = rec.get("rc") or 0
        benign_read_miss = (rec.get("sync") == "none" and rc_val in (1, 128) and not rec.get("timed_out") and not rec.get("refused"))
        if (rc_val == 0 or benign_read_miss) and not rec.get("timed_out") and not rec.get("refused"):
            self.s._ok_n = getattr(self.s, "_ok_n", 0) + 1
        else:
            self.s._fail_n = getattr(self.s, "_fail_n", 0) + 1
        last_pub = getattr(self.s, "_last_pub", 0.0)
        important = bool(rec.get("edited") or (rc_val != 0 and not benign_read_miss) or rec.get("timed_out") or rec.get("refused"))
        if not important and (now - last_pub) < 5.0 and (self.s._cmd_n % 10 != 0):
            return
        self.s._last_pub = now
        ms = int(round((rec.get("secs") or 0) * 1000))
        runner = str(rec.get("runner") or self.s.runner_name or "sandbox")[:32]
        sync = str(rec.get("sync") or "full")[:16]
        pol = str(self.s.policy_digest or "")[:12]
        pbytes = rec.get("patch_bytes") or 0
        try:
            self.s.mcp.call("playbox_say", {"text": "::pbx metric " + json.dumps({"name": "task_ms", "value": ms, "unit": "ms"}, separators=(",", ":"), ensure_ascii=False)})
            if important or (self.s._cmd_n % 10 == 0):
                status = "failed" if ((rc_val != 0 and not benign_read_miss) or rec.get("timed_out") or rec.get("refused")) else "checking"
                summary = f"{self.s._cmd_n} runs ({getattr(self.s, '_ok_n', 0)} ok, {getattr(self.s, '_fail_n', 0)} fail) · last {ms}ms · sync={sync}"
                if pbytes:
                    summary += f" · patch {pbytes}B"
                if pol:
                    summary += f" · policy {pol}"
                art = {"id": f"session-{runner}", "title": f"{runner} session", "kind": "code", "status": status, "summary": summary[:380]}
                self.s.mcp.call("playbox_say", {"text": "::pbx artifact " + json.dumps(art, separators=(",", ":"), ensure_ascii=False)})
        except Exception as exc:  # noqa: BLE001 — telemetry must never disrupt execution
            logging.getLogger(__name__).debug("telemetry publish failed: %s", exc)

    def export_patch(self) -> str:
        """Perform one authoritative `sync="full", full_patch=True` snapshot in stateful mode and persist it."""
        with self.s.lock:
            if self.s.keep and self.s.dirty:
                try:
                    args = self._delegate_args("true", 60, sync="full", full_patch=True)
                    r, _ = self._call_with_rate_retry(args)
                    self.s.last_patch = r.get("patch") or ""
                    self.s.last_truncated = bool(r.get("patch_truncated"))
                    self.s.dirty = False
                except Exception as exc:  # noqa: BLE001 — keep last delta patch if session ended
                    logging.getLogger(__name__).debug("export_patch failed, keeping last delta patch: %s", exc)
            self.s.save_final()
            return self.s.last_patch

    def _log(self, rec):
        if self.s.stats_path:
            with open(self.s.stats_path, "a") as f:
                f.write(json.dumps(rec) + "\n")

    def cleanup(self):
        self.export_patch()


class PlayboxProvider(TerminalEnvironmentProvider):
    name = "playbox"
    display_name = "Playbox governed sandbox (OpenShell / libkrun)"
    is_remote = True
    is_container = True
    default_cwd = SANDBOX_CWD

    def is_available(self):
        return bool(os.environ.get("PLAYBOX_LINK_FILE") and os.environ.get("PLAYBOX_HOME"))

    def create_environment(self, *, cwd, timeout, task_id="default", image=None, container_config=None, **kw):
        return PlayboxEnvironment(cwd, timeout)


def register(ctx):
    ctx.register_terminal_environment_provider(PlayboxProvider())
