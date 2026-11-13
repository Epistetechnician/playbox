"""A new build and the previous release work together in one room, in both directions (needs PLAYBOX_OLD_BIN)."""
from harness import room

TIER = "fast"


def talk(host, guest, tag):
    guest.ok("playbox_say", text=f"{tag}: from the guest")
    host.ok("playbox_say", text=f"{tag}: from the host")
    texts = [i["text"] for i in guest.ok("playbox_feed", since=0)["items"] if i.get("kind") == "say"]
    assert f"{tag}: from the guest" in texts and f"{tag}: from the host" in texts, texts
    htexts = [i["text"] for i in host.ok("playbox_feed", since=0)["items"] if i.get("kind") == "say"]
    assert f"{tag}: from the guest" in htexts, htexts


def run(ctx):
    # A current host with a previous-release guest: the host's join reply carries fields the old guest ignores.
    host, (guest,), _ = room(ctx, 1, old_guests=True)
    assert guest.ok("playbox_status")["joined"]["member_id"]
    talk(host, guest, "new host, old guest")
    assert host.ok("playbox_audit")["entries"] >= 2

    # A previous-release host with a current guest: its reply has no protocol field, which counts as protocol 1.
    host2, (guest2,), _ = room(ctx, 1, old_host=True)
    assert guest2.ok("playbox_status")["joined"]["member_id"]
    talk(host2, guest2, "old host, new guest")
