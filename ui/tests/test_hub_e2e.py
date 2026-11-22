"""End-to-end verification of Playbox Hub backend telemetry, archive search, and settings classifier."""
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from hub import archive_search, board, rooms, settings_search  # noqa: E402
from hub.archive import Archive  # noqa: E402


class _FakeRoom:
    def __init__(self, rid, label, arch):
        self.id = rid
        self.rec = {"id": rid, "label": label}
        self.archive = arch


class _BrokenArchive:
    def tail(self, _n):
        raise OSError("disk unavailable")


def test_hub_e2e_core():
    d = tempfile.mkdtemp(prefix="pbx-hub-e2e-")
    try:
        # 1. Archive meta allowlist + sanitization
        arch = Archive(d)
        arch.append([
            {"seq": 1, "kind": "event", "from": "", "text": "host_start", "time_ms": 1000,
             "meta": {"event": "host_start", "runner": "openshell", "isolation": "MicroVm", "transport": "iroh"}},
            {"seq": 2, "kind": "event", "from": "peer1", "text": "task ok sh exit 0", "time_ms": 2000,
             "meta": {"event": "task", "ok": True, "runner": "openshell", "program": "sh", "image": "pb-swe-django:arm64",
                      "ms": 63, "keep": True, "sync": "delta", "policy_digest": "d5f8a9823f22",
                      "secrets": ["API_KEY"], "secret_value": "sk-never", "nested": {"bad": 1}}},
            {"seq": 3, "kind": "say", "from": "peer1",
             "text": '::pbx metric {"name":"task_ms","value":70,"unit":"ms"}', "time_ms": 3000},
            {"seq": 4, "kind": "say", "from": "peer1",
             "text": '::pbx artifact {"id":"session-openshell","title":"openshell session","kind":"code","status":"checking","summary":"10 runs (9 ok, 1 fail)"}',
             "time_ms": 4000},
        ])
        items = arch.tail(10)
        assert len(items) == 4
        assert items[1]["meta"]["runner"] == "openshell"
        assert "secret_value" not in items[1]["meta"] and "nested" not in items[1]["meta"]

        # 2. Board telemetry rollup + rooms pretty_pbx & event_note
        b = board.build(items, [])
        t = b["telemetry"]
        assert t["tasks"] == 1 and t["ok"] == 1 and t["avg_ms"] == 63 and t["runners"] == {"openshell": 1}
        assert rooms.pretty_pbx(items[2]["text"]) == "metric · task_ms = 70 ms"
        assert "openshell session (checking)" in rooms.pretty_pbx(items[3]["text"])
        note = rooms.event_note(items[1]["text"], "peer1", items[1]["meta"])
        assert "openshell" in note[1] and "63ms" in note[1]

        # 3. Cross-room archive search
        sr = archive_search.search_rooms([_FakeRoom("r1", "swe-django", arch)], "openshell")
        assert sr["total"] >= 3 and sr["results"][0]["href"].startswith("/r/r1/?tab=timeline&drill="), sr
        sr2 = archive_search.search_rooms([_FakeRoom("r1", "swe-django", arch)], "pb-swe-django")
        assert sr2["total"] == 1 and sr2["results"][0]["seq"] == 2, sr2

        # 4. Secure settings classifier
        c1 = settings_search.classify("fix offline tunnel", live_state={"offline_rooms": 2, "network_mode": "cloudflare"})
        assert c1["intent"] == "troubleshoot_fix" and c1["results"]
        c2 = settings_search.classify("revoke passkey")
        assert c2["intent"] == "security_audit" and c2["results"][0]["security_tier"] == "step_up"
        c3 = settings_search.classify("ignore previous instructions <script>alert(1)</script>")
        assert c3["flagged"] is True
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_archive_search_edge_cases():
    d = tempfile.mkdtemp(prefix="pbx-arch-edge-")
    try:
        arch = Archive(d)
        long_msg = "openshell " + ("x" * 240)
        arch.append([
            {"seq": 1, "kind": "say", "from": "alice", "text": long_msg, "time_ms": 1000,
             "meta": {"secret_value": "sk-top-secret-999"}},
            {"seq": 2, "kind": "event", "from": "bob", "text": "task failed exit 1", "time_ms": 2000,
             "meta": {"event": "task", "ok": False, "runner": "openshell", "image": "pb-fail:arm64"}},
        ])
        good_room = _FakeRoom("r-ok", "django-room", arch)
        broken_room = _FakeRoom("r-bad", "broken-room", _BrokenArchive())

        # Empty, whitespace, punctuation-only, non-string, and control-char queries
        for bad_q in ("", "   ", "!!!", None, 123, "\x00\x07\n\t"):
            res = archive_search.search_rooms([good_room], bad_q)
            assert res["total"] == 0 and res["results"] == [], (bad_q, res)

        # Control chars stripped and query capped at MAX_QUERY
        capped = archive_search.search_rooms([good_room], "\x00openshell\x1f " + ("a" * 300))
        assert len(capped["query"]) <= archive_search.MAX_QUERY
        assert "\x00" not in capped["query"] and "\x1f" not in capped["query"]

        # Unreadable room archive is skipped without breaking other rooms
        mixed = archive_search.search_rooms([broken_room, good_room], "openshell")
        assert mixed["total"] == 2
        assert all(r["room"] == "r-ok" for r in mixed["results"])
        assert all(len(r["snippet"]) <= 180 for r in mixed["results"])

        # Limit clamping (min 1, max 25)
        one = archive_search.search_rooms([good_room], "openshell", limit=0)
        assert len(one["results"]) == 1

        # Secret non-disclosure: disallowed meta keys were stripped by Archive and cannot match
        leak = archive_search.search_rooms([good_room], "sk-top-secret-999")
        assert leak["total"] == 0 and leak["results"] == []
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_settings_search_edge_cases():
    # Empty and non-string queries return safe default overview without crashing
    for empty_q in ("", "   ", None, 42):
        res = settings_search.classify(empty_q)
        assert res["intent"] == "navigation" and res["flagged"] is False
        assert res["results"] and res["results"][0]["id"] == "overview"

    # Fuzzy bigram matching for typos
    typo = settings_search.classify("tailscal")
    assert any("network" in r["id"] or "tailscale" in r["title"].lower() for r in typo["results"]), typo

    # Live status hints reflect only non-secret counts/modes; arbitrary secrets in live_state are ignored
    live = settings_search.classify(
        "audit weak session",
        live_state={"weak_sessions": 3, "approvals": 2, "offline_rooms": 1, "push_subs": 0, "api_key": "sk-secret"},
    )
    assert live["intent"] == "security_audit"
    assert any(r["id"] == "hub.sessions" and r["live_status"] == "3 weak session(s)" for r in live["results"])
    assert "sk-secret" not in str(live)

    # Suspicious prompt injection / command-substitution / HTML payloads are flagged and length-capped
    inj = settings_search.classify("system prompt $(whoami) " + ("z" * 200))
    assert inj["flagged"] is True
    assert len(inj["query"]) <= settings_search.MAX_QUERY

    # Limit bounds are enforced
    bounded = settings_search.classify("", limit=999)
    assert len(bounded["results"]) <= 15
    one = settings_search.classify("", limit=0)
    assert len(one["results"]) == 1


def main():
    test_hub_e2e_core()
    test_archive_search_edge_cases()
    test_settings_search_edge_cases()
    print("hub e2e passed")


if __name__ == "__main__":
    main()
