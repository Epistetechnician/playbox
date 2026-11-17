"""Join budget: from a link to a confirmed round trip, and the small tool list a guest sees."""
import os
import time

from harness import room

TIER = "fast"
# REQ: ROOM-5, ROOM-6

# Seconds from the guest's join call to the host reading the guest's message. Binaries are prebuilt, so this
# excludes download and install, which are timed separately by whoever runs install.sh.
BUDGET = float(os.environ.get("PLAYBOX_JOIN_BUDGET_SECS", "180"))
CORE_MAX = 7


def run(ctx):
    host = ctx.mcp("host")
    created = host.ok("playbox_create", transport="local", name="jt", uses=1)
    guest = ctx.mcp("guest", PLAYBOX_TOOLS="core")

    # ROOM-6: the guest's list is small, and playbox_more adds a group.
    names = [t["name"] for t in guest.rpc("tools/list", {})["result"]["tools"]]
    assert len(names) <= CORE_MAX, f"core lists {len(names)} tools: {names}"
    assert "playbox_join" in names and "playbox_more" in names, names
    guest.ok("playbox_more", group="work")
    after = [t["name"] for t in guest.rpc("tools/list", {})["result"]["tools"]]
    assert "playbox_push" in after and len(after) > len(names), after

    # ROOM-5: join, say, and the host sees it.
    t0 = time.monotonic()
    guest.ok("playbox_join", link=created["link"], workspace=str(ctx.dir("ws")))
    guest.ok("playbox_say", text="first message")
    feed = host.ok("playbox_feed", since=0)
    assert any(i.get("text") == "first message" for i in feed["items"]), feed
    took = time.monotonic() - t0
    print(f"    join to first confirmed round trip: {took:.2f}s (budget {BUDGET:.0f}s)")
    assert took <= BUDGET, f"join took {took:.1f}s, over the {BUDGET:.0f}s budget"
