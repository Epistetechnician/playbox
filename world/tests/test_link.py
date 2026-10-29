import blake3

from playbox_world.events import Event
from playbox_world.link import click, link_events, parse_log


def line(seq, payload):
    h = blake3.blake3(payload.encode()).hexdigest()
    return f"{seq} {'0' * 64} tool_call {h} {1000 + seq} {'a' * 64} {'b' * 128}"


def test_links_are_checked_against_the_signed_payload_hash():
    entries = parse_log("\n".join([line(0, "ran tests"), line(1, "edited a.rs")]) + "\n")
    events = [
        Event(0.0, "claude", "action", "ran tests", 0, "ran tests"),
        Event(0.1, "claude", "action", "edited", 1, "edited a.rs but altered"),  # payload differs
        Event(0.2, "codex", "action", "x", 7, "x"),                              # no such entry
        Event(0.3, "codex", "message", "hello"),                                  # names no entry
    ]
    assert link_events(events, entries) == ["linked", "mismatch", "missing", "unlinked"]


def test_click_returns_nearest_events_and_only_linked_ones_carry_an_entry():
    entries = parse_log(line(0, "ran tests") + "\n")
    events = [Event(0, "a", "action", "t", 0, "ran tests"), Event(0, "a", "action", "far", 5, "z")]
    pos = [[0.5, 0.5, 0.5], [0.9, 0.9, 0.9]]
    status = link_events(events, entries)
    hit = click(events, pos, status, entries, [0.51, 0.5, 0.5])
    assert len(hit) == 1 and hit[0]["entry"].seq == 0
    assert click(events, pos, status, entries, [0.9, 0.9, 0.9])[0]["entry"] is None
    assert click(events, pos, status, entries, [0.0, 0.0, 0.0]) == []


def test_end_to_end_with_the_real_cli(tmp_path):
    """Sign events with the real `playbox record`, then link them. Needs a built CLI."""
    import os, pathlib, subprocess, pytest
    from playbox_world.record import record_events
    from playbox_world.events import Event
    bin_ = pathlib.Path(__file__).resolve().parents[2] / "target/debug/playbox"
    if not bin_.exists():
        pytest.skip("cargo build -p playbox-cli first")
    log = str(tmp_path / "h.log")
    rows = record_events([{"text": "ran tests"}, {"text": "edited a.rs"}], log, str(bin_), str(tmp_path / "k.key"))
    entries = parse_log(open(log).read())
    evs = [Event(0, "a", "action", r["text"], r["seq"], r["payload"]) for r in rows]
    assert link_events(evs, entries) == ["linked", "linked"]
    ok = subprocess.run([str(bin_), "verify", log], capture_output=True)
    assert ok.returncode == 0, ok.stderr
    evs[1].payload = "tampered"
    assert link_events(evs, entries) == ["linked", "mismatch"]
