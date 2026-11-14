"""Conversations: roster-only, ordered, signed by authors, unread marks."""
TIER = "fast"
# REQ: CONV-1, CONV-2


def run(ctx):
    host, (a, b, c), created = _three(ctx)
    ids = [g.ok("playbox_status")["joined"]["member_id"] for g in (a, b, c)]
    conv = a.ok("playbox_conversation_new", title="plan", members=[ids[1]])["conversation"]
    cid = conv["id"]
    assert len(conv["roster"]) == 2
    c.refused("playbox_conversation_read", conversation=cid)
    c.refused("playbox_conversation_say", conversation=cid, text="let me in")
    e1 = a.ok("playbox_conversation_say", conversation=cid, text="ship friday")["entry"]
    e2 = b.ok("playbox_conversation_say", conversation=cid, text="agreed")["entry"]
    assert (e1["seq"], e2["seq"]) == (1, 2) and len(e1["sig"]) == 128
    listing = b.ok("playbox_conversations")["conversations"]
    assert listing[0]["unread"] == 1
    got = b.ok("playbox_conversation_read", conversation=cid)
    assert [e["text"] for e in got["conversation"]["entries"]] == ["ship friday", "agreed"]
    assert "untrusted" in got["note"]
    assert b.ok("playbox_conversations")["conversations"][0]["unread"] == 0
    a.ok("playbox_conversation_invite", conversation=cid, members=[ids[2]])
    c.ok("playbox_conversation_read", conversation=cid)


def _three(ctx):
    host = ctx.mcp("host")
    created = host.ok("playbox_create", transport="local", name="conv", uses=3)
    gs = []
    for i in range(3):
        g = ctx.mcp(f"g{i}")
        g.ok("playbox_join", link=created["link"], workspace=str(ctx.dir(f"ws{i}")))
        gs.append(g)
    return host, gs, created
