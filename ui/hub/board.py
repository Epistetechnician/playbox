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

from . import card as idcard

PREFIX = "::pbx "
KINDS = ("artifact", "card", "metric", "agent", "comment", "handoff")
ANCHORS = ("note", "event", "artifact")
ART_KINDS = ("proof", "code", "doc", "dataset", "chart", "note")
ART_STATUS = ("draft", "checking", "verified", "failed", "retired")
COLS = ("todo", "doing", "review", "done")
BUCKET_MS = 5 * 60 * 1000
MAX_BODY = 4000

TEMPLATES = [
    {
        "id": "swe",
        "label": "SWE · Multi-Agent Coding",
        "summary": "Sandboxed coding & test runner, package-registry egress allowlist, diff replay, and handoff board.",
        "runtime": "docker",
        "limits": {"cpus": 2.0, "mem_mib": 2048, "timeout_secs": 600, "network": False,
                   "allowed_hosts": ["github.com", "api.github.com", "crates.io", "index.crates.io", "pypi.org", "files.pythonhosted.org", "registry.npmjs.org"]},
        "budget": {"task_budget": 500, "max_members": 16, "max_concurrent": 4, "max_tasks_per_minute": 30},
        "memory_review": False,
        "capabilities": [{"name": "test_suite", "approval": False, "timeout_secs": 300}, {"name": "git_status", "approval": False, "timeout_secs": 30}],
    },
    {
        "id": "research",
        "label": "Research · Empirical & ML",
        "summary": "High-budget compute limits (4 CPU / 4 GiB), ML hub allowlist, live metric tracking, and reproducible evaluation.",
        "runtime": "docker",
        "limits": {"cpus": 4.0, "mem_mib": 4096, "timeout_secs": 1800, "network": False,
                   "allowed_hosts": ["huggingface.co", "cdn-lfs.huggingface.co", "pypi.org", "files.pythonhosted.org", "arxiv.org"]},
        "budget": {"task_budget": 1000, "max_members": 24, "max_concurrent": 4, "max_tasks_per_minute": 40},
        "memory_review": False,
        "capabilities": [{"name": "run_benchmark", "approval": False, "timeout_secs": 600}, {"name": "promote_candidate", "approval": True, "timeout_secs": 60}],
    },
    {
        "id": "review",
        "label": "Review · Security & Code Audit",
        "summary": "Zero-egress audit sandbox, owner-gated memory review, secret scanning capability, and deterministic diff verification.",
        "runtime": "docker",
        "limits": {"cpus": 1.0, "mem_mib": 1024, "timeout_secs": 180, "network": False, "allowed_hosts": []},
        "budget": {"task_budget": 200, "max_members": 12, "max_concurrent": 2, "max_tasks_per_minute": 20},
        "memory_review": True,
        "capabilities": [{"name": "secret_scan", "approval": False, "timeout_secs": 60}, {"name": "approve_release", "approval": True, "timeout_secs": 30}],
    },
    {
        "id": "ops",
        "label": "Ops · Incident & Runbook",
        "summary": "Diagnostics + owner-approved remediation capabilities, curated status egress, and shift handoff checkpoints.",
        "runtime": "docker",
        "limits": {"cpus": 2.0, "mem_mib": 2048, "timeout_secs": 300, "network": False,
                   "allowed_hosts": ["status.github.com", "api.github.com"]},
        "budget": {"task_budget": 300, "max_members": 16, "max_concurrent": 2, "max_tasks_per_minute": 20},
        "memory_review": True,
        "capabilities": [{"name": "health_check", "approval": False, "timeout_secs": 30}, {"name": "remediate", "approval": True, "timeout_secs": 120}],
    },
]


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


def clip_card(c):
    """The fields of a verified card, clipped: what the hub keeps, shows and hashes."""
    f = {k: _s(c.get(k), n) for k, n in (("name", 30), ("machine", 40), ("role", 80), ("harness", 30), ("model", 40), ("ans", 120))}
    f["did"] = _s(c.get("did"), 80)
    return f


def card_hash(c):
    """What an endorsement is bound to: the card's content, not its timestamps, so a routine re-announce of the same card keeps the endorsement
    and any change of name, machine, role, harness, model or public name does not."""
    import hashlib
    keys = ("key", "did", "name", "machine", "role", "harness", "model", "ans")
    return hashlib.sha256(json.dumps({k: c.get(k, "") for k in keys}, sort_keys=True, separators=(",", ":")).encode()).hexdigest()[:32]


def build(items, members, me=None, now_ms=0, room=None, endorsed=None, ans_checked=None):
    """feed items + member list -> the canvas model. `room` is the room's host id: signed identity cards are bound to it (hub/card.py)."""
    names, machines, roles, harnesses, models = {}, {}, {}, {}, {}
    launches = {}                                              # host-signed launch records by member key
    idcards, pinned, claims, rejected = {}, {}, {}, set()     # verified cards by key, the first verified card per key, unsigned claims, (key, reason) already reported
    arts, cards, series, log, comments = {}, {}, {}, [], []
    handoffs, tasks_list = {}, []
    msgs, per, buckets, typed = [], {}, {}, {"artifact": 0, "card": 0, "metric": 0, "agent": 0, "comment": 0, "handoff": 0}
    telem = {"tasks": 0, "ok": 0, "failed": 0, "timed_out": 0, "ms_sum": 0, "ms_count": 0, "ms_max": 0,
             "replays": 0, "reproducible": 0,
             "runners": {}, "images": {}, "programs": {}, "egress": {}, "secrets": {}, "policies": set(), "host": None}

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
            m = it.get("meta") if isinstance(it.get("meta"), dict) else {}
            ev = str(m.get("event") or "").lower()
            if t in ("join ok", "join failed") or t.startswith("refused"):
                log.append({"ms": ms, "type": "member", "who": frm, "text": ("joined" if t == "join ok" else "failed to join" if t == "join failed" else "was refused"), "seq": it.get("seq")})
            elif ev == "host_start" or t == "host_start":
                if m:
                    telem["host"] = {k: m[k] for k in ("runner", "isolation", "transport", "version", "repo", "replica") if k in m}
                bits = [str(m[k]) for k in ("runner", "isolation", "transport") if m.get(k)]
                log.append({"ms": ms, "type": "event", "who": frm, "text": f"host started ({' · '.join(bits)})" if bits else "host started", "seq": it.get("seq")})
            elif ev == "task" or t.startswith("task "):
                telem["tasks"] += 1
                ok = m.get("ok") if "ok" in m else t.startswith("task ok")
                if ok:
                    telem["ok"] += 1
                else:
                    telem["failed"] += 1
                if m.get("timed_out"):
                    telem["timed_out"] += 1
                dur = m.get("ms")
                if isinstance(dur, (int, float)) and not isinstance(dur, bool) and math.isfinite(dur) and dur >= 0:
                    telem["ms_sum"] += dur
                    telem["ms_count"] += 1
                    telem["ms_max"] = max(telem["ms_max"], int(dur))
                for key, bag in (("runner", "runners"), ("image", "images"), ("program", "programs")):
                    val = _s(m.get(key), 60)
                    if val:
                        telem[bag][val] = telem[bag].get(val, 0) + 1
                for key, bag in (("egress", "egress"), ("secrets", "secrets")):
                    for val in (m.get(key) or [])[:8] if isinstance(m.get(key), list) else []:
                        sv = _s(val, 40)
                        if sv:
                            telem[bag][sv] = telem[bag].get(sv, 0) + 1
                pd = _s(m.get("policy_digest"), 64)
                if pd:
                    telem["policies"].add(pd)
                tasks_list.append({
                    "seq": it.get("seq"),
                    "ms": ms,
                    "from": frm,
                    "ok": bool(ok),
                    "task_id": _s(m.get("task_id"), 64),
                    "runner": _s(m.get("runner"), 32),
                    "program": _s(m.get("program"), 60),
                    "cmd_hash": _s(m.get("cmd_hash"), 16),
                    "base": _s(m.get("base"), 60),
                    "base_commit": _s(m.get("base_commit") or m.get("commit"), 40),
                    "commit": _s(m.get("commit"), 40),
                    "branch": _s(m.get("branch"), 60),
                    "policy_digest": pd,
                    "patch_hash": _s(m.get("patch_hash"), 64),
                    "stdout_hash": _s(m.get("stdout_hash"), 64),
                    "patch_bytes": m.get("patch_bytes") if isinstance(m.get("patch_bytes"), int) else 0,
                    "changed_files": m.get("changed_files") if isinstance(m.get("changed_files"), int) else 0,
                    "exit_code": m.get("exit_code") if isinstance(m.get("exit_code"), int) else None,
                    "timed_out": bool(m.get("timed_out")),
                    "dur_ms": int(dur) if isinstance(dur, (int, float)) and not isinstance(dur, bool) and math.isfinite(dur) else None,
                })
                if not ok:
                    log.append({"ms": ms, "type": "task", "who": frm, "text": _s(text, 120), "seq": it.get("seq")})
            elif ev == "replay":
                telem["replays"] += 1
                rep_ok = bool(m.get("reproducible", m.get("ok")))
                if rep_ok:
                    telem["reproducible"] += 1
                log.append({"ms": ms, "type": "task", "who": frm, "text": f"replayed task {_s(m.get('task_id'), 16)}: {'reproducible ✓' if rep_ok else 'diverged ✕'}", "seq": it.get("seq")})
            elif ev == "policy_update":
                log.append({"ms": ms, "type": "event", "who": frm or "host", "text": f"updated live policy guardrails (digest {_s(m.get('policy_digest'), 12)})", "seq": it.get("seq")})
            continue
        if kind != "say":
            continue
        per[frm] = per.get(frm, 0) + 1
        buckets[int(ms // BUCKET_MS)] = buckets.get(int(ms // BUCKET_MS), 0) + 1
        if isinstance(text, str) and text.startswith(idcard.PREFIX):
            # A signed identity card. The sender is the key the room says sent it; the host's own messages come from "host", whose key is the room id.
            sender = room if frm == "host" else frm
            try:
                c = idcard.verify(text, sender, room, ms or now_ms)     # valid when it was posted; whether it has run out since is judged below
            except ValueError as e:
                if (frm, str(e)) not in rejected:
                    rejected.add((frm, str(e)))
                    log.append({"ms": ms, "type": "identity", "who": frm, "text": f"identity card refused: {str(e)[:60]}", "seq": it.get("seq")})
                continue
            fields = clip_card(c)
            fields.update(expires_ms=c["expires_ms"], issued_ms=c["issued_ms"])
            old = idcards.get(frm)
            if frm not in pinned:
                pinned[frm] = fields
                log.append({"ms": ms, "type": "identity", "who": frm, "text": f"signed identity card: {fields['name']}", "seq": it.get("seq")})
            elif old:
                for k in ("name", "machine", "harness", "model"):
                    if old[k] != fields[k]:
                        log.append({"ms": ms, "type": "identity", "who": frm, "text": f"{old['name']} changed {k}: {old[k] or 'none'} to {fields[k] or 'none'}", "seq": it.get("seq")})
            idcards[frm] = fields
            continue
        if isinstance(text, str) and text.startswith(idcard.LAUNCH_PREFIX):
            # The host saying it started a member. Checked against the room id (the host's key), so a member cannot write one.
            try:
                lr = idcard.verify_launch(text, room or "")
            except ValueError as e:
                log.append({"ms": ms, "type": "identity", "who": frm, "text": f"launch record refused: {str(e)[:60]}", "seq": it.get("seq")})
                continue
            launches[lr["key"]] = {k: _s(lr.get(k), n) for k, n in (("harness", 30), ("model", 40), ("image", 100))} | {"started_ms": lr["started_ms"]}
            log.append({"ms": ms, "type": "identity", "who": lr["key"], "text": f"host recorded a launch: {launches[lr['key']]['harness']}", "seq": it.get("seq")})
            continue
        p = parse(text)
        if not p:
            msgs.append({"seq": it.get("seq"), "from": frm, "ms": ms, "text": text[:1500]})
            continue
        k, d = p
        typed[k] += 1
        if k == "agent":
            # An unsigned claim: anyone can write it, so it never becomes an identity. Kept apart, shown only as muted text next to the member.
            claims[frm] = {k: _s(d.get(k), n) for k, n in (("name", 30), ("machine", 40), ("role", 80), ("harness", 30), ("model", 40))}
            log.append({"ms": ms, "type": "agent", "who": frm, "text": "posted an unsigned introduction (not verified)", "seq": it.get("seq")})
        elif k == "comment":
            on, txt = _s(d.get("on"), 60), _s(d.get("text"), 600)
            ak, _, ref = on.partition(":")
            if ak in ANCHORS and ref and txt and len(ref) <= 40:
                comments.append({"on": f"{ak}:{ref}", "kind": ak, "ref": ref, "text": txt, "from": frm, "ms": ms, "seq": it.get("seq")})
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
        elif k == "handoff":
            hid = _s(d.get("id"), 48) or f"h-{it.get('seq') or len(handoffs) + 1}"
            br = _s(d.get("branch"), 120) or "main"
            bc = _s(d.get("base_commit"), 64)
            sm = _s(d.get("summary"), 600)
            oc = [_s(x, 60) for x in (d.get("open_cards") or [])[:16] if _s(x, 60)] if isinstance(d.get("open_cards"), list) else []
            va = [_s(x, 80) for x in (d.get("verified_artifacts") or [])[:16] if _s(x, 80)] if isinstance(d.get("verified_artifacts"), list) else []
            fc = [_s(x, 120) for x in (d.get("failed_checks") or [])[:16] if _s(x, 120)] if isinstance(d.get("failed_checks"), list) else []
            bud = d.get("budget_remaining") if isinstance(d.get("budget_remaining"), dict) else {}
            clean_bud = {str(bk)[:40]: bv for bk, bv in list(bud.items())[:12] if isinstance(bv, (int, float, str, bool))}
            sig = _s(d.get("sig"), 128)
            h_obj = {
                "id": hid,
                "from": frm,
                "to": _s(d.get("to"), 64),
                "summary": sm,
                "branch": br,
                "base_commit": bc,
                "open_cards": oc,
                "verified_artifacts": va,
                "failed_checks": fc,
                "budget_remaining": clean_bud,
                "policy_digest": _s(d.get("policy_digest"), 64),
                "issued_ms": d.get("issued_ms") if isinstance(d.get("issued_ms"), int) else ms,
                "ms": ms,
                "seq": it.get("seq"),
                "sig": sig,
                "signed": bool(len(sig) == 128),
            }
            handoffs[hid] = h_obj
            log.append({"ms": ms, "type": "handoff", "who": frm, "text": f"handed off branch {br}" + (f" @ {bc[:8]}" if bc else "") + (f": {sm[:80]}" if sm else ""), "ref": hid, "seq": it.get("seq")})
    mem = {}
    for m in members or []:
        mid = m.get("id") or ""
        mem[mid] = {"id": mid, "present": bool(m.get("present")), "last_seen": m.get("last_seen_ms") or 0}
    for mid in list(per) + list(idcards) + list(claims) + list(launches):
        mem.setdefault(mid, {"id": mid, "present": False, "last_seen": 0})
    live = {k: c for k, c in idcards.items() if c["expires_ms"] > (now_ms or 0)}
    count = {}
    for c in live.values():
        count[c["name"]] = count.get(c["name"], 0) + 1
    for mid, m in mem.items():
        c = live.get(mid)
        ident = "signed" if c else ("expired" if mid in idcards else "unidentified")
        m.update({"name": (c or {}).get("name") or names.get(mid) or m.get("name") or ("host" if mid == "host" else ""), "name_verified": bool(c),
                  "identity": ident, "card": c or idcards.get(mid), "claim": claims.get(mid), "dup_name": bool(c and count.get(c["name"], 0) > 1),
                  "machine": (c or {}).get("machine", ""), "role": (c or {}).get("role", ""), "harness": (c or {}).get("harness", ""), "model": (c or {}).get("model", ""),
                  "ans_status": ("" if not ans_checked or mid not in ans_checked else "registered" if c and c.get("ans") == ans_checked[mid].get("name") else "changed"),
                  "ans_proof": (ans_checked or {}).get(mid) if (ans_checked or {}).get(mid) and c and c.get("ans") == ans_checked[mid].get("name") else None,
                  "launch": launches.get(mid), "launch_differs": bool(launches.get(mid) and c and ((c.get("harness") and c["harness"].lower() != launches[mid]["harness"].lower()) or (c.get("model") and launches[mid]["model"] and c["model"].lower() != launches[mid]["model"].lower()))),
                  "did": (c or {}).get("did", ""), "ans": (c or {}).get("ans", ""), "msgs": per.get(mid, 0), "me": mid == me,
                  "endorsement": ("" if not endorsed or mid not in endorsed else "endorsed" if c and endorsed[mid].get("card_hash") == card_hash(dict(c, key=mid)) else "changed"),
                  "card_hash": card_hash(dict(c, key=mid)) if c else ""})
    for e in log:
        e["name"] = who(e["who"])
    for a in arts.values():
        a["by_name"], a["last_name"] = who(a["by"]), who(a["last_by"])
    for c in cards.values():
        c["by_name"] = who(c["by"])
    for c in comments:
        c["name"] = who(c["from"])
        a = arts.get(c["ref"]) if c["kind"] in ("note", "artifact") else None
        if a is not None:
            a.setdefault("comments", []).append(c)
    ev_comments = {}
    for c in comments:
        if c["kind"] == "event":
            ev_comments.setdefault(c["ref"], []).append(c)
    for m in msgs:
        m["name"] = who(m["from"])
    for h in handoffs.values():
        h["from_name"] = who(h["from"])
    for tk in tasks_list:
        tk["from_name"] = who(tk["from"])
    log.sort(key=lambda e: (e["ms"], e.get("seq") or 0), reverse=True)
    nb = int(now_ms // BUCKET_MS) if now_ms else (max(buckets) if buckets else 0)
    activity = [{"b": b, "n": buckets.get(b, 0)} for b in range(nb - 35, nb + 1)]
    return {"members": sorted(mem.values(), key=lambda m: (not m["present"], -m["msgs"])), "feed": msgs[-200:], "artifacts": sorted(arts.values(), key=lambda a: -a["updated"]),
            "cards": sorted(cards.values(), key=lambda c: -c["updated"]), "cols": list(COLS), "metrics": list(series.values()), "changelog": log[:200], "comments": comments[-500:], "event_comments": ev_comments,
            "handoffs": sorted(handoffs.values(), key=lambda h: -(h.get("ms") or 0)),
            "tasks": tasks_list[-100:][::-1],
            "telemetry": {"tasks": telem["tasks"], "ok": telem["ok"], "failed": telem["failed"], "timed_out": telem["timed_out"],
                          "replays": telem["replays"], "reproducible": telem["reproducible"],
                          "avg_ms": int(telem["ms_sum"] / telem["ms_count"]) if telem["ms_count"] else None, "max_ms": telem["ms_max"] if telem["ms_count"] else None,
                          "runners": telem["runners"], "images": telem["images"], "programs": telem["programs"],
                          "egress": telem["egress"], "secrets": telem["secrets"], "policies": len(telem["policies"]), "host": telem["host"]},
            "stats": {"messages": sum(per.values()), "typed": typed, "per_member": per, "activity": activity, "bucket_ms": BUCKET_MS,
                      "artifacts": {s: len([a for a in arts.values() if a["status"] == s]) for s in ART_STATUS}, "cards": {c: len([x for x in cards.values() if x["col"] == c]) for c in COLS}}}
