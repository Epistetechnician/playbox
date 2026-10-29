"""Click-through from a blob in the scene to its signed history entry.

The scene is a learned picture and proves nothing. An event only counts as linked if the signed log
backs it: entry `seq` exists and its `payload_hash` equals blake3(event.payload). Anything else is
shown as unlinked, so a blob can never claim provenance the log does not give it. This checks the
link, not the chain: run `playbox verify <log>` for signatures and hash chaining.
"""

from dataclasses import dataclass

import blake3
import numpy as np


@dataclass(frozen=True)
class Entry:
    seq: int
    kind: str
    payload_hash: str
    time_ms: int
    signer: str
    sig: str


def parse_log(text: str) -> list[Entry]:
    """Parse `playbox` history text: `seq prev kind payload_hash time_ms signer sig` per line."""
    out = []
    for n, line in enumerate(text.splitlines(), 1):
        if not line:
            continue
        f = line.split(" ")
        if len(f) != 7:
            raise ValueError(f"line {n}: expected 7 fields, found {len(f)}")
        out.append(Entry(int(f[0]), f[2], f[3], int(f[4]), f[5], f[6]))
    return out


def link_events(events, entries: list[Entry]) -> list[str]:
    """Per event: 'linked', 'mismatch' (payload differs from what was signed), 'missing' (no such
    entry) or 'unlinked' (the event names no entry)."""
    by_seq = {e.seq: e for e in entries}
    status = []
    for ev in events:
        if ev.seq is None or ev.payload is None:
            status.append("unlinked")
        elif ev.seq not in by_seq:
            status.append("missing")
        elif blake3.blake3(ev.payload.encode()).hexdigest() == by_seq[ev.seq].payload_hash:
            status.append("linked")
        else:
            status.append("mismatch")
    return status


def pick(positions, point, radius=0.05):
    """Indices of events within `radius` of a clicked 3D point, nearest first."""
    p = np.asarray(positions, dtype=np.float32)
    d = np.linalg.norm(p - np.asarray(point, dtype=np.float32), axis=1)
    idx = np.argsort(d)
    return [int(i) for i in idx if d[i] <= radius]


def click(events, positions, status, entries, point, radius=0.05):
    """What a click at `point` shows: each nearby event and, if linked, its signed entry."""
    by_seq = {e.seq: e for e in entries}
    return [
        {"index": i, "kind": events[i].kind, "agent": events[i].agent, "text": events[i].text,
         "status": status[i], "entry": by_seq[events[i].seq] if status[i] == "linked" else None}
        for i in pick(positions, point, radius)
    ]
