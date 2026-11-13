"""Which coding agents on this machine can talk to Playbox, and a one-click way to connect them.
Detection reads each agent's own config file (best effort, never executes the agent). Connecting runs
`playbox setup --only <agent>`, the same command the installer runs."""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

AGENTS = (("claude", "Claude Code"), ("desktop", "Claude Desktop / Cowork"), ("codex", "Codex"), ("opencode", "OpenCode"), ("hermes", "Hermes"))


def _desktop_cfg(home):
    if sys.platform == "darwin":
        return home / "Library" / "Application Support" / "Claude" / "claude_desktop_config.json"
    if sys.platform.startswith("win"):
        return Path(os.environ.get("APPDATA") or home / "AppData" / "Roaming") / "Claude" / "claude_desktop_config.json"
    return home / ".config" / "Claude" / "claude_desktop_config.json"


def manual(cli=None):
    """What any other MCP-capable agent (Muse, Instinct, Pi, ...) needs: a stdio server."""
    cli = cli or os.environ.get("PLAYBOX_CLI") or shutil.which("playbox") or "playbox"
    return {"command": cli, "args": ["mcp"], "json": json.dumps({"mcpServers": {"playbox": {"command": cli, "args": ["mcp"]}}}, indent=2)}


def _connect_desktop(cli, home):
    p = _desktop_cfg(home)
    try:
        cfg = json.loads(_read(p) or "{}")
    except ValueError:
        return False, f"{p} isn't valid JSON; fix or move it first"
    if not isinstance(cfg, dict):
        return False, f"{p} isn't a JSON object"
    cfg.setdefault("mcpServers", {})["playbox"] = {"command": cli, "args": ["mcp"]}
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_name(p.name + ".playbox-tmp")
    tmp.write_text(json.dumps(cfg, indent=2))
    os.replace(tmp, p)
    return True, f"added playbox to {p}"


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
    if agent == "desktop":
        try:
            return "playbox" in (json.loads(_read(_desktop_cfg(home)) or "{}").get("mcpServers") or {})
        except (ValueError, AttributeError):
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
    if agent == "desktop":
        return _desktop_cfg(home).parent.is_dir()
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
    if agent == "desktop":
        return _connect_desktop(cli, Path(os.environ.get("HOME") or Path.home()))
    try:
        r = subprocess.run([cli, "setup", "--only", agent], capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError) as e:
        return False, str(e)
    return r.returncode == 0, (r.stdout + r.stderr).strip()[-600:]
