"""Convert a Claude Code session transcript (~/.claude/projects/*/<id>.jsonl) to events JSONL.

python -m playbox_world.claude_code <transcript.jsonl> <events.jsonl>
"""

import json
import re
import sys
from datetime import datetime

MAX_TEXT = 300
_TAG = re.compile(r"<(system-reminder|pasted_content)[^>]*>.*?</\1[^>]*>", re.S)


def _clean(s: str) -> str:
    return " ".join(_TAG.sub(" ", s).split())[:MAX_TEXT]


def _tool_summary(name: str, inp: dict) -> str:
    for key in ("command", "file_path", "pattern", "query", "url", "description", "prompt"):
        if isinstance(inp.get(key), str):
            return f"{name} {_clean(inp[key])}"
    return name


def convert(path: str):
    """Yield {"t","agent","kind","text"} rows. Tool results are skipped (noisy)."""
    with open(path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except ValueError:
                continue
            row_type, ts, msg = r.get("type"), r.get("timestamp"), r.get("message")
            if row_type not in ("user", "assistant") or not ts or not isinstance(msg, dict):
                continue
            t = datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
            agent = "user" if row_type == "user" else "claude"
            content = msg.get("content")
            blocks = [{"type": "text", "text": content}] if isinstance(content, str) else content or []
            for b in blocks:
                kind = b.get("type")
                if kind == "text" and (text := _clean(b.get("text", ""))):
                    yield {"t": t, "agent": agent, "kind": "message", "text": text}
                elif kind == "thinking" and (text := _clean(b.get("thinking", ""))):
                    yield {"t": t, "agent": agent, "kind": "reasoning", "text": text}
                elif kind == "tool_use":
                    inp = b.get("input") or {}
                    is_commit = b.get("name") == "Bash" and "git commit" in str(inp.get("command", ""))
                    yield {"t": t, "agent": agent, "kind": "commit" if is_commit else "action", "text": _tool_summary(b.get("name", "tool"), inp)}


def main():
    src, dst = sys.argv[1], sys.argv[2]
    n = 0
    with open(dst, "w") as out:
        for row in convert(src):
            out.write(json.dumps(row) + "\n")
            n += 1
    print(f"wrote {n} events to {dst}")


if __name__ == "__main__":
    main()
