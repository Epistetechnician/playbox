"""A capability runs once however often it is called, within its bounds, and its result hash is in the host's audit log."""
import json
import os
import tempfile

TIER = "fast"


def run(ctx):
    marker = os.path.join(tempfile.mkdtemp(dir=ctx.root), "count")
    caps = [{
        "name": "tick", "argv": ["sh", "-c", f"echo x >> {marker}; echo done {{tag}}"],
        "params": {"tag": {"allowed": ["a", "b"]}}, "approval": False, "timeout_secs": 20,
    }]
    # No isolation: this scenario is about the call protocol, so the host may use the unsafe process runner.
    os.environ["PLAYBOX_ALLOW_UNSAFE_RUNNER"] = "1"
    host = ctx.mcp("host", PLAYBOX_ALLOW_UNSAFE_RUNNER="1")
    created = host.ok("playbox_create", transport="local", name="caps", unsafe_process_runner=True, capabilities=caps)
    guest = ctx.mcp("guest")
    guest.ok("playbox_join", link=created["link"], workspace=str(ctx.dir("ws")))

    offer = guest.ok("playbox_capabilities")["capabilities"]
    assert offer[0]["name"] == "tick" and "sh" not in json.dumps(offer), "members never see the command"

    first = guest.ok("playbox_capability_call", name="tick", args={"tag": "a"}, call_id="once-1", wait_secs=30)
    assert first["call"]["state"] == "done" and first["call"]["outcome"]["stdout"].strip() == "done a", first
    for _ in range(3):
        again = guest.ok("playbox_capability_call", name="tick", args={"tag": "a"}, call_id="once-1", wait_secs=5)
        assert again["call"]["outcome"]["result_hash"] == first["call"]["outcome"]["result_hash"]
    with open(marker) as f:
        assert len(f.read().splitlines()) == 1, "the command must run exactly once"

    assert "allows" in guest.refused("playbox_capability_call", name="tick", args={"tag": "z"}, call_id="bad-1")
    assert "different request" in guest.refused("playbox_capability_call", name="tick", args={"tag": "b"}, call_id="once-1")

    log = host.ok("playbox_audit", tail=50)
    assert first["call"]["outcome"]["result_hash"] in json.dumps(log), "the result hash must be in the audit log"
