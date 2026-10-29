"""Record events into a signed Playbox history log so the scene can link back to them.

Each event's text is appended with `playbox record` (the same path agent hooks use), which signs and
chains it. The event then carries the entry's seq and the exact payload that was signed.
"""

import subprocess


def record_events(rows, log, playbox="playbox", key=None):
    """rows: dicts with a "text". Returns copies with "seq" and "payload" set."""
    try:
        seq = len(open(log).read().splitlines())
    except FileNotFoundError:
        seq = 0
    out = []
    for r in rows:
        cmd = [playbox, "record", "--kind", "world_event", "--log", log] + (["--key", key] if key else [])
        p = subprocess.run(cmd, input=r["text"].encode(), capture_output=True)
        if p.returncode:
            raise RuntimeError(f"playbox record failed: {p.stderr.decode().strip()}")
        out.append({**r, "seq": seq, "payload": r["text"]})
        seq += 1
    return out
