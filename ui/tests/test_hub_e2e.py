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


def main():
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
            {"seq": 3, "kind": "say", "from": "peer1", "text": '::pbx metric {"name":"task_ms","value":70,"unit":"ms"}', "time_ms": 3000},
            {"seq": 4, "kind": "say", "from": "peer1", "text": '::pbx artifact {"id":"session-openshell","title":"openshell session","kind":"code","status":"checking","summary":"10 runs (9 ok, 1 fail)"}', "time_ms": 4000},
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

        print("hub e2e passed")
    finally:
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    main()
