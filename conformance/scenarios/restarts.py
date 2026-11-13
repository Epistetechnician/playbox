"""A host that is killed and started again keeps its room: finished calls are not run twice, a call that was running is abandoned, talk survives."""
import os
import tempfile
import threading

TIER = "fast"
# REQ: ROOM-2, ROOM-3, CAP-3, CAP-8


def run(ctx):
    os.environ["PLAYBOX_ALLOW_UNSAFE_RUNNER"] = "1"
    marker = os.path.join(tempfile.mkdtemp(dir=ctx.root), "count")
    caps = [
        {"name": "tick", "argv": ["sh", "-c", f"echo x >> {marker}; echo done"], "params": {}, "approval": False, "timeout_secs": 20},
        {"name": "slow", "argv": ["sh", "-c", f"echo s >> {marker}.slow; sleep 25"], "params": {}, "approval": False, "timeout_secs": 40},
    ]

    guest = ctx.mcp("guest")
    # A link bound to the guest's own key, so it is the same member each time it joins; a link that is not bound makes a new member.
    gid = guest.ok("playbox_status")["guest_id"]

    def start_host():
        host = ctx.mcp("host", PLAYBOX_ALLOW_UNSAFE_RUNNER="1")
        return host, host.ok("playbox_create", transport="local", name="restart-room", unsafe_process_runner=True, capabilities=caps, uses=1, for_guest=gid)

    host, created = start_host()
    guest.ok("playbox_join", link=created["link"], workspace=str(ctx.dir("ws")))
    member_before = guest.ok("playbox_status")["joined"]["member_id"]
    guest.ok("playbox_say", text="before the restart")

    first = guest.ok("playbox_capability_call", name="tick", args={}, call_id="done-1", wait_secs=30)["call"]
    assert first["state"] == "done"

    # A call still running when the host dies.
    t = threading.Thread(target=lambda: guest.tool("playbox_capability_call", name="slow", args={}, call_id="slow-1", wait_secs=2), daemon=True)
    t.start()
    t.join(timeout=20)

    host.close()
    host, created = start_host()
    # On the local transport the host's address changes with the process; the guest follows by joining with the new link, as itself.
    if guest.ok("playbox_status").get("joined"):
        guest.ok("playbox_leave")
    guest.ok("playbox_join", link=created["link"], workspace=str(ctx.dir("ws")))
    assert guest.ok("playbox_status")["joined"]["member_id"] == member_before, "the guest keeps its identity across the restart"

    # Talk survived the restart.
    texts = [i["text"] for i in host.ok("playbox_feed", since=0)["items"] if i.get("kind") == "say"]
    assert "before the restart" in texts, texts

    # The finished call is not run again; asking for it with the same id returns the same result.
    again = guest.ok("playbox_capability_call", name="tick", args={}, call_id="done-1", wait_secs=10)["call"]
    assert again["outcome"]["result_hash"] == first["outcome"]["result_hash"]
    with open(marker) as f:
        assert len(f.read().splitlines()) == 1, "a finished call must not run again after a restart"

    # The call that was running is abandoned, not restarted.
    gone = guest.ok("playbox_capability_call", name="slow", args={}, call_id="slow-1", wait_secs=5)["call"]
    assert gone["state"] == "abandoned", gone
    with open(marker + ".slow") as f:
        assert len(f.read().splitlines()) == 1, "an abandoned call must not be started again"

    # The audit log is still one chain across both lives of the host.
    audit = host.ok("playbox_audit")
    assert audit["entries"] >= 2 and len(audit["head"]) == 64, audit
