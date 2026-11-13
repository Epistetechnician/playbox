"""Which coding agents on this machine can talk to Playbox, and a one-click way to connect them.
Detection reads each agent's own config file (best effort, never executes the agent). Connecting runs
`playbox setup --only <agent>`, the same command the installer runs."""
import json
import os
import shutil
import subprocess
from pathlib import Path

AGENTS = (("claude", "Claude Code"), ("codex", "Codex"), ("opencode", "OpenCode"), ("hermes", "Hermes"))


def _read(p):
    try:
        return Path(p).read_text()
    except OSError:
        return ""


def _registered(agent, home):
    if agent == "claude":
        try:
            return "playbox" in (json.loads(_read(home / ".claude.json") or "{}").get("mcpServers") or {})
        except ValueError:
            return False
    if agent == "codex":
        return "[mcp_servers.playbox]" in _read(home / ".codex" / "config.toml")
    if agent == "opencode":
        try:
            return "playbox" in (json.loads(_read(home / ".config" / "opencode" / "opencode.json") or "{}").get("mcp") or {})
        except ValueError:
            return False
    if agent == "hermes":
        return "playbox" in _read(home / ".hermes" / "config.yaml")
    return False


def _installed(agent, home):
    if shutil.which(agent):
        return True
    return {"claude": (home / ".claude.json").exists() or (home / ".claude").is_dir(), "codex": (home / ".codex").is_dir(),
            "opencode": (home / ".config" / "opencode").is_dir(), "hermes": (home / ".hermes").is_dir()}.get(agent, False)


def detect():
    home = Path(os.environ.get("HOME") or Path.home())
    return [{"id": a, "name": n, "installed": _installed(a, home), "connected": _registered(a, home)} for a, n in AGENTS]


def connect(agent):
    """Returns (ok, output). Only the known agent ids are accepted."""
    if agent not in dict(AGENTS):
        return False, "unknown agent"
    cli = os.environ.get("PLAYBOX_CLI") or shutil.which("playbox")
    if not cli:
        return False, "the `playbox` command isn't installed; run the installer first"
    try:
        r = subprocess.run([cli, "setup", "--only", agent], capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError) as e:
        return False, str(e)
    return r.returncode == 0, (r.stdout + r.stderr).strip()[-600:]
