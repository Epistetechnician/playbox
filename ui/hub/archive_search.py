"""Fast, read-only cross-room archive search for the Playbox hub.

Searches recent archived items across all rooms (messages, ::pbx artifacts/metrics/cards, and task telemetry
metadata such as runner, image, program, policy_digest, and exit_code) and returns bounded, snippet-clipped
matches with deep-link targets (`/r/<room_id>/?tab=timeline&drill=<seq>`). Never executes anything and never
returns raw secrets.
"""
import re

from .rooms import pretty_pbx

MAX_QUERY = 160
TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9_.:-]*")


def _clean_query(raw):
    if not isinstance(raw, str):
        return ""
    return "".join(ch for ch in raw if ord(ch) >= 32)[:MAX_QUERY].strip()


def search_rooms(rooms, query, *, per_room=250, limit=12):
    """Search across `rooms` (iterable of Room instances) and return ranked, safe hits."""
    q = _clean_query(query)
    ql = q.lower()
    tokens = TOKEN_RE.findall(ql)
    if not tokens:
        return {"query": q, "total": 0, "results": []}

    hits = []
    for room in rooms:
        rid = room.id
        label = str(room.rec.get("label") or rid)[:60]
        try:
            items = room.archive.tail(per_room)
        except (OSError, ValueError, KeyError, AttributeError):
            continue
        for it in reversed(items):
            if not isinstance(it, dict):
                continue
            text = str(it.get("text") or "")
            pretty = pretty_pbx(text)
            meta = it.get("meta") if isinstance(it.get("meta"), dict) else {}
            meta_str = " ".join(
                str(v) if not isinstance(v, list) else " ".join(str(x) for x in v)
                for v in meta.values()
            )
            hay = f"{label} {pretty} {text} {it.get('from') or ''} {it.get('name') or ''} {meta_str}".lower()

            score = 0.0
            if ql in hay:
                score += 3.0
            matched = 0
            for tok in tokens:
                if tok in hay:
                    matched += 1
                    score += 1.2
            if matched == 0:
                continue
            if matched == len(tokens):
                score += 1.5
            # Slight preference for failed tasks, artifacts, and messages over routine ok tasks.
            kind = str(it.get("kind") or "event")
            if kind == "say":
                score += 0.4
            elif meta.get("ok") is False:
                score += 0.8

            hits.append((score, int(it.get("time_ms") or 0), {
                "room": rid,
                "label": label,
                "seq": it.get("seq"),
                "kind": kind,
                "from": str(it.get("name") or it.get("from") or "")[:24],
                "time_ms": int(it.get("time_ms") or 0),
                "snippet": pretty[:180],
                "runner": meta.get("runner"),
                "image": meta.get("image"),
                "ok": meta.get("ok"),
                "ms": meta.get("ms"),
                "href": f"/r/{rid}/?tab=timeline&drill={it.get('seq')}",
                "score": round(score, 2),
            }))

    hits.sort(key=lambda x: (-x[0], -x[1]))
    out = [h for _, _, h in hits[:max(1, min(limit, 25))]]
    return {"query": q, "total": len(hits), "results": out}
