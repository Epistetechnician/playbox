"""A minimal MCP client over stdio, so a Python orchestrator can drive a `playbox-mcp` sidecar the way an
agent does. The sidecar is one room member: it holds the key that signs specs and checks every receipt.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path


class McpError(RuntimeError):
    pass


class McpClient:
    def __init__(self, binary: str | Path, home: str | Path, env: dict[str, str] | None = None):
        self._p = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1,
            env={**os.environ, "PLAYBOX_HOME": str(home), **(env or {})},
        )
        self._n = 0
        init = self._rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "playbox-orchestrator", "version": "0"}})
        if "result" not in init:
            raise McpError(f"initialize failed: {init}")
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, msg: dict) -> None:
        assert self._p.stdin
        self._p.stdin.write(json.dumps(msg) + "\n")
        self._p.stdin.flush()

    def _rpc(self, method: str, params: dict) -> dict:
        self._n += 1
        self._send({"jsonrpc": "2.0", "id": self._n, "method": method, "params": params})
        assert self._p.stdout
        line = self._p.stdout.readline()
        if not line:
            raise McpError("the sidecar closed its output")
        reply = json.loads(line)
        if reply.get("id") != self._n:
            raise McpError(f"reply id mismatch: {reply}")
        return reply

    def call(self, tool: str, args: dict) -> dict:
        """Call a tool; returns its JSON result, or raises with the tool's own error text."""
        r = self._rpc("tools/call", {"name": tool, "arguments": args})["result"]
        text = r["content"][0]["text"]
        if r.get("isError"):
            raise McpError(f"{tool}: {text}")
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return {"text": text}

    def close(self) -> None:
        self._p.kill()
        self._p.wait()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
