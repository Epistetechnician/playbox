"""The room's shared work surface, built only from its feed.

Agents publish work with ordinary `playbox_say` messages that start with `::pbx <kind> <json>`. Because the feed is already shared,
signed into the host's history and visible to every member, nothing else is needed for every member to see the same artifacts,
tasks and metrics. This module turns a feed into a model the canvas draws: members, artifacts (with revisions), kanban cards,
metric series, a changelog and message statistics.

Trust: every field is untrusted text from some member. Kinds, statuses and columns are whitelisted, strings are clipped, numbers must
be finite, and attribution always comes from the message's real sender, never from a name inside the JSON.
"""
import json
import math

PREFIX = "::pbx "
KINDS = ("artifact", "card", "metric", "agent")
ART_KINDS = ("proof", "code", "doc", "dataset", "chart", "note")
ART_STATUS = ("draft", "checking", "verified", "failed", "retired")
COLS = ("todo", "doing", "review", "done")
BUCKET_MS = 5 * 60 * 1000
MAX_BODY = 4000


def _s(v, n):
    return str(v).strip()[:n] if isinstance(v, (str, int, float)) and not isinstance(v, bool) else ""


def parse(text):
    """`::pbx kind {json}` -> (kind, dict) or None. Never raises."""
    if not isinstance(text, str) or not text.startswith(PREFIX):
        return None
    rest = text[len(PREFIX):]
    kind, _, body = rest.partition(" ")
    if kind not in KINDS or len(body) > MAX_BODY:
        return None
    try:
        d = json.loads(body)
    except ValueError:
        return None
    return (kind, d) if isinstance(d, dict) else None


def encode(kind, /, **fields):
    """The text an agent posts with playbox_say."""
    if kind not in KINDS:
        raise ValueError("unknown kind")
    return PREFIX + kind + " " + json.dumps({k: v for k, v in fields.items() if v is not None}, separators=(",", ":"))


def _short(i):
    return (i or "")[:6] or "host"


def build(items, members, me=None, now_ms=0):
    """feed items + member list -> the canvas model."""
    names, machines, roles = {}, {}, {}
    arts, cards, series, log = {}, {}, {}, []
    msgs, per, buckets, typed = [], {}, {}, {"artifact": 0, "card": 0, "metric": 0, "agent": 0}

    def who(i):
        return names.get(i) or _short(i)

    for it in items:
        if not isinstance(it, dict):
            continue
        ms, frm, kind, text = it.get("time_ms") or 0, it.get("from") or "", it.get("kind"), it.get("text") or ""
        if it.get("name"):
            names.setdefault(frm, _s(it["name"], 30))
        if kind == "event":
            t = str(text).lower()
            if t in ("join ok", "join failed") or t.startswith("refused"):
                log.append({"ms": ms, "type": "member", "who": frm, "text": ("joined" if t == "join ok" else "failed to join" if t == "join failed" else "was refused"), "seq": it.get("seq")})
            continue
        if kind != "say":
            continue
        per[frm] = per.get(frm, 0) + 1
        buckets[int(ms // BUCKET_MS)] = buckets.get(int(ms // BUCKET_MS), 0) + 1
        p = parse(text)
        if not p:
            msgs.append({"seq": it.get("seq"), "from": frm, "ms": ms, "text": text[:1500]})
            continue
        k, d = p
        typed[k] += 1
        if k == "agent":
            nm = _s(d.get("name"), 30)
            if nm:
                names[frm] = nm
            machines[frm] = _s(d.get("machine"), 40)
            roles[frm] = _s(d.get("role"), 80)
            log.append({"ms": ms, "type": "agent", "who": frm, "text": f"announced itself{' on ' + machines[frm] if machines[frm] else ''}", "seq": it.get("seq")})
        elif k == "artifact":
            aid = _s(d.get("id"), 40) or _s(d.get("title"), 40)
            if not aid:
                continue
            old = arts.get(aid)
            old_status = old["status"] if old else None
            kd = d.get("kind") if d.get("kind") in ART_KINDS else (old or {}).get("kind", "note")
            st = d.get("status") if d.get("status") in ART_STATUS else (old or {}).get("status", "draft")
            rev = {"ms": ms, "from": frm, "status": st, "summary": _s(d.get("summary"), 400), "seq": it.get("seq")}
            a = old or {"id": aid, "revs": [], "created": ms, "by": frm}
            a.update({"title": _s(d.get("title"), 90) or a.get("title") or aid, "kind": kd, "status": st, "ref": _s(d.get("ref"), 160) or a.get("ref", ""),
                      "summary": rev["summary"] or a.get("summary", ""), "body": _s(d.get("body"), 3000) or a.get("body", ""), "updated": ms, "last_by": frm,
                      "checks": [{"name": _s(c.get("name"), 40), "ok": bool(c.get("ok"))} for c in (d.get("checks") or [])[:12] if isinstance(c, dict)] or a.get("checks", [])})
            a["revs"].append(rev)
            arts[aid] = a
            if not old:
                log.append({"ms": ms, "type": "artifact", "who": frm, "text": f"published {kd} “{a['title']}” as {st}", "ref": aid, "seq": it.get("seq")})
            elif old_status != st:
                log.append({"ms": ms, "type": "artifact", "who": frm, "text": f"moved “{a['title']}” from {old_status} to {st}", "ref": aid, "seq": it.get("seq")})
            else:
                log.append({"ms": ms, "type": "artifact", "who": frm, "text": f"revised “{a['title']}” (rev {len(a['revs'])})", "ref": aid, "seq": it.get("seq")})
        elif k == "card":
            cid = _s(d.get("id"), 40) or _s(d.get("title"), 40)
            if not cid:
                continue
            old = cards.get(cid)
            old_col = old["col"] if old else None
            col = d.get("col") if d.get("col") in COLS else (old or {}).get("col", "todo")
            c = old or {"id": cid, "created": ms, "by": frm}
            c.update({"title": _s(d.get("title"), 120) or c.get("title") or cid, "col": col, "owner": _s(d.get("owner"), 30) or c.get("owner", ""),
                      "artifact": _s(d.get("artifact"), 40) or c.get("artifact", ""), "note": _s(d.get("note"), 300) or c.get("note", ""), "updated": ms, "last_by": frm})
            cards[cid] = c
            log.append({"ms": ms, "type": "card", "who": frm, "text": (f"added task “{c['title']}” to {col}" if not old else f"moved “{c['title']}” from {old_col} to {col}" if old_col != col else f"updated task “{c['title']}”"),
                        "ref": cid, "seq": it.get("seq")})
        elif k == "metric":
            nm, v = _s(d.get("name"), 40), d.get("value")
            if nm and isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v):
                s = series.setdefault(nm, {"name": nm, "unit": _s(d.get("unit"), 12), "pts": []})
                s["pts"].append({"ms": ms, "v": v, "from": frm})
                s["pts"] = s["pts"][-200:]
                if len(s["pts"]) in (1,):
                    log.append({"ms": ms, "type": "metric", "who": frm, "text": f"started tracking {nm}", "ref": nm, "seq": it.get("seq")})
    mem = {}
    for m in members or []:
        mid = m.get("id") or ""
        mem[mid] = {"id": mid, "present": bool(m.get("present")), "last_seen": m.get("last_seen_ms") or 0}
    for mid in list(per) + list(machines):
        mem.setdefault(mid, {"id": mid, "present": False, "last_seen": 0})
    for mid, m in mem.items():
        m.update({"name": names.get(mid) or m.get("name") or ("host" if mid == "host" else ""), "machine": machines.get(mid, ""), "role": roles.get(mid, ""), "msgs": per.get(mid, 0), "me": mid == me})
    for e in log:
        e["name"] = who(e["who"])
    for a in arts.values():
        a["by_name"], a["last_name"] = who(a["by"]), who(a["last_by"])
    for c in cards.values():
        c["by_name"] = who(c["by"])
    for m in msgs:
        m["name"] = who(m["from"])
    log.sort(key=lambda e: (e["ms"], e.get("seq") or 0), reverse=True)
    nb = int(now_ms // BUCKET_MS) if now_ms else (max(buckets) if buckets else 0)
    activity = [{"b": b, "n": buckets.get(b, 0)} for b in range(nb - 35, nb + 1)]
    return {"members": sorted(mem.values(), key=lambda m: (not m["present"], -m["msgs"])), "feed": msgs[-200:], "artifacts": sorted(arts.values(), key=lambda a: -a["updated"]),
            "cards": sorted(cards.values(), key=lambda c: -c["updated"]), "cols": list(COLS), "metrics": list(series.values()), "changelog": log[:200],
            "stats": {"messages": sum(per.values()), "typed": typed, "per_member": per, "activity": activity, "bucket_ms": BUCKET_MS,
                      "artifacts": {s: len([a for a in arts.values() if a["status"] == s]) for s in ART_STATUS}, "cards": {c: len([x for x in cards.values() if x["col"] == c]) for c in COLS}}}
