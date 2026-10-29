"""A plain A2A-over-HTTP client for one Playbox bridge (`playbox bridge <link>`).

Each peer is a bridge that has joined one room as one member, so delegating to three peers means
three members' grants, each enforced by its host. Nothing here is trusted: a result is whatever the
peer's host returned, and the merge step treats patches as untrusted text.
"""
from __future__ import annotations

import base64
import itertools
import json
import urllib.error
import urllib.request
from dataclasses import dataclass, field

_ids = itertools.count(1)


class PeerError(RuntimeError):
    pass


@dataclass
class TaskResult:
    peer: str
    state: str
    stdout: str = ""
    stderr: str = ""
    patch: str = ""
    meta: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return self.state == "TASK_STATE_COMPLETED"


@dataclass
class Peer:
    url: str
    token: str | None = None
    timeout: float = 300.0

    def _post(self, method: str, params: dict) -> dict:
        body = json.dumps({"jsonrpc": "2.0", "id": next(_ids), "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url.rstrip("/") + "/jsonrpc", data=body, headers={"content-type": "application/json"})
        if self.token:
            req.add_header("authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                reply = json.load(r)
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
            raise PeerError(f"{self.url}: {e}") from e
        if "error" in reply:
            raise PeerError(f"{self.url}: {reply['error'].get('message', reply['error'])}")
        return reply["result"]

    def run(self, cmd: str, base: str = "main", image: str = "alpine:3", timeout_secs: int = 120) -> TaskResult:
        op = {"playbox": {"op": "run", "base": base, "spec": {
            "cmd": ["sh", "-c", cmd], "image": image, "cpus": 1.0, "mem_mib": 512,
            "timeout_secs": timeout_secs, "network": False}}}
        msg = {"message": {"messageId": f"m{next(_ids)}", "role": "ROLE_USER", "parts": [{"data": op}]}}
        task = self._post("SendMessage", msg).get("task")
        if not task:
            raise PeerError(f"{self.url}: no task in reply")
        return _result(self.url, task)


def _result(peer: str, task: dict) -> TaskResult:
    out = TaskResult(peer=peer, state=task["status"]["state"], meta=task.get("metadata") or {})
    for art in task.get("artifacts") or []:
        for part in art.get("parts") or []:
            if "raw" in part:
                data = base64.b64decode(part["raw"]).decode("utf-8", "replace")
            else:
                data = part.get("text", "")
            name = art.get("name")
            if name == "stdout":
                out.stdout = data
            elif name == "stderr":
                out.stderr = data
            elif name == "patch":
                out.patch = data
    return out
