"""Consent before joining: a guest sees what the host is first, and declining costs nothing."""
from harness import room  # noqa: F401

TIER = "fast"
# REQ: ROOM-7


def run(ctx):
    host = ctx.mcp("host")
    created = host.ok("playbox_create", transport="local", name="consent", uses=1)
    guest = ctx.mcp("guest")

    seen = guest.ok("playbox_join", link=created["link"], preview=True)
    assert seen["preview"] is True and seen["joined"] is False, seen
    att = seen["host_attestation"]
    assert att["verified_tier"] and att["claimed_tier"] and "runner" in att, att
    assert "playbox_join" in seen["next"], seen
    # Previewing is not joining: no room yet, and the host has no members.
    assert not guest.ok("playbox_status").get("joined"), "a preview must not join"
    assert host.ok("playbox_status")["hosting"]["members"] == [], "a preview must not admit anyone"

    # The invite was single-use and is still good: previewing twice, then joining, all work.
    guest.ok("playbox_join", link=created["link"], preview=True)
    joined = guest.ok("playbox_join", link=created["link"], workspace=str(ctx.dir("ws")))
    assert joined["joined"] is True and joined["host_attestation"]["verified_tier"] == att["verified_tier"], joined
