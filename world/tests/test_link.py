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
