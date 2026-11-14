"""Join, talk, retry, and be revoked: the room's basic promises through two sidecars."""
from harness import room

TIER = "fast"
# REQ: ROOM-1


def run(ctx):
    host, (guest,), created = room(ctx, 1)
    status = guest.ok("playbox_status")
    assert status["joined"]["member_id"], status

    seq = guest.ok("playbox_say", text="hello from the guest")["seq"]
    host.ok("playbox_say", text="hello from the host")
    feed = guest.ok("playbox_feed", since=0)
    texts = [i["text"] for i in feed["items"] if i.get("kind") == "say"]
    assert "hello from the guest" in texts and "hello from the host" in texts, feed
    assert seq >= 1

    # The host's log verifies and names the guest's first requests.
    audit = host.ok("playbox_audit")
    assert audit["entries"] >= 2 and len(audit["head"]) == 64, audit

    # Revoke: the guest's next call is refused.
    mid = host.ok("playbox_status")["hosting"]["members"][0]
    assert mid == status["joined"]["member_id"]
    host.ok("playbox_revoke", member=mid)
    msg = guest.refused("playbox_say", text="after revoke")
    assert "revoked" in msg.lower() or "not authorized" in msg.lower() or "refused" in msg.lower(), msg
