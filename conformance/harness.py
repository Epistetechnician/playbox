"""Black-box harness: starts the *installed* `playbox` and `playbox-mcp` binaries and talks to them the way an agent or a person would.

Nothing here imports the project's code. A scenario is a function `def run(ctx)` in scenarios/*.py that raises AssertionError (or any
exception) on failure and `Skip` when the machine cannot run it. `ctx` hands out scratch directories and process helpers."""
import json
import os
import queue
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path


class Skip(Exception):
    """The scenario needs something this machine lacks (docker, a signed helper). CI sets CONFORMANCE_REQUIRE_ALL to make it a failure."""


def bin_dir() -> Path:
    if os.environ.get("PLAYBOX_BIN"):
        return Path(os.environ["PLAYBOX_BIN"])
    return Path(__file__).resolve().parents[1] / "target" / "debug"


def exe(name: str, old: bool = False) -> str:
    if old:
        d = os.environ.get("PLAYBOX_OLD_BIN")
        if not d:
            raise Skip("PLAYBOX_OLD_BIN (a directory with an older release's binaries) is not set")
        p = Path(d) / name
        if not p.exists():
            raise Skip(f"{p} is not built")
        return str(p)
    p = bin_dir() / name
    if not p.exists():
        raise Skip(f"{p} is not built")
    return str(p)


class Mcp:
    """One `playbox-mcp` sidecar over stdio. Notifications that arrive between replies are kept in `pushed`."""

    def __init__(self, home: Path, env: dict | None = None, old: bool = False):
        e = {**os.environ, "PLAYBOX_HOME": str(home), **(env or {})}
        e.pop("PLAYBOX_LOG", None)
        if not (env or {}).get("PLAYBOX_LIVE"):
            e.pop("PLAYBOX_LIVE", None)
        self.p = subprocess.Popen([exe("playbox-mcp", old)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=e, text=True, bufsize=1)
        self.q: "queue.Queue[dict]" = queue.Queue()
        self.pushed: list[dict] = []
        self.n = 0
        threading.Thread(target=self._read, daemon=True).start()
        init = self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "conformance", "version": "0"}})
        self.init = init
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _read(self):
        for line in self.p.stdout:
            try:
                self.q.put(json.loads(line))
            except ValueError:
                pass

    def _send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params, timeout=60):
        self.n += 1
        self._send({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params})
        end = time.time() + timeout
        while True:
            try:
                m = self.q.get(timeout=max(0.01, end - time.time()))
            except queue.Empty:
                raise TimeoutError(f"{method} got no reply in {timeout}s")
            if m.get("id") == self.n:
                return m
            self.pushed.append(m)

    def tool(self, name, /, **args):
        """(is_error, parsed JSON or text)."""
        r = self.rpc("tools/call", {"name": name, "arguments": args})["result"]
        text = (r.get("content") or [{}])[0].get("text", "")
        try:
            body = json.loads(text)
        except ValueError:
            body = text
        return bool(r.get("isError")), body

    def ok(self, name, /, **args):
        err, body = self.tool(name, **args)
        assert not err, f"{name} failed: {body}"
        return body

    def refused(self, name, /, **args) -> str:
        err, body = self.tool(name, **args)
        assert err, f"{name} was expected to be refused but returned {body}"
        return str(body)

    def wait_push(self, secs, pred):
        end = time.time() + secs
        while True:
            for i, m in enumerate(self.pushed):
                if pred(m):
                    return self.pushed.pop(i)
            try:
                self.pushed.append(self.q.get(timeout=max(0.01, end - time.time())))
            except queue.Empty:
                return None

    def close(self):
        try:
            self.p.kill()
            self.p.wait(timeout=5)
        except Exception:
            pass


class Ctx:
    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="playbox-conformance-"))
        self.procs: list[Mcp] = []

    def dir(self, name: str) -> Path:
        d = self.root / name
        d.mkdir(parents=True, exist_ok=True)
        return d

    def mcp(self, name: str, old: bool = False, **env) -> Mcp:
        m = Mcp(self.dir(name), env, old)
        self.procs.append(m)
        return m

    def cli(self, *args, env=None, input=None, timeout=60):
        e = {**os.environ, "PLAYBOX_HOME": str(self.dir("cli-home")), **(env or {})}
        return subprocess.run([exe("playbox"), *args], capture_output=True, text=True, env=e, input=input, timeout=timeout)

    def cleanup(self):
        for m in self.procs:
            m.close()
        shutil.rmtree(self.root, ignore_errors=True)


def room(ctx: Ctx, guests=1, old_host=False, old_guests=False, **create):
    """A host sidecar with a room and `guests` joined guest sidecars. Returns (host, [guest...], created).
    `old_host` / `old_guests` use the binaries in PLAYBOX_OLD_BIN (a previous release) for that side."""
    host = ctx.mcp("host", old=old_host)
    created = host.ok("playbox_create", transport="local", name="conf", uses=guests, **create)
    out = []
    for i in range(guests):
        g = ctx.mcp(f"guest{i}", old=old_guests)
        g.ok("playbox_join", link=created["link"], workspace=str(ctx.dir(f"ws{i}")))
        out.append(g)
    return host, out, created
