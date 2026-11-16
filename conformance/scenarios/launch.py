"""`playbox launch`: the host's owner starts an agent with a key the host knows in advance; the host signs a record naming that key and file."""
import json
import os
import shlex
import shutil
import subprocess
import tempfile
import time
from harness import exe

TIER = "fast"
# REQ: IDENT-9


def run(ctx):
    hd = tempfile.mkdtemp(prefix="pbh", dir="/tmp")           # a unix socket path must stay short
    env = {**os.environ, "PLAYBOX_HOSTING_DIR": hd}
    state = ctx.dir("hoststate") / "state"
    host = subprocess.Popen([exe("playbox"), "up", "--local", "--state", str(state)], env={**env, "PLAYBOX_HOME": str(ctx.dir("host-home"))},
                            stdout=open(str(ctx.dir("logs") / "host.out"), "w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(60):
            recs = [f for f in os.listdir(hd) if f.endswith(".json")]
            if recs:
                break
            time.sleep(0.5)
        assert recs, "the host never published itself: " + open(str(ctx.dir("logs") / "host.out")).read()[-600:]
        rid = recs[0][:-5][:8]
        ghome = ctx.dir("agent-home")
        pb = exe("playbox")
        # The agent here is a bare sidecar: it joins by itself from PLAYBOX_JOIN_LINK_FILE and reports its own status.
        mcp = exe("playbox-mcp")
        init = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}
        stat = {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "playbox_status", "arguments": {}}}
        child = "printf '%s\\n' " + shlex.quote(json.dumps(init)) + " " + shlex.quote(json.dumps(stat)) + " | " + shlex.quote(mcp)
        r = subprocess.run([pb, "launch", "--room", rid, "--harness", "Conformance Harness", "--model", "m1", "--", "sh", "-c", child],
                           env={**env, "PLAYBOX_HOME": str(ghome)}, capture_output=True, text=True, timeout=120)
        assert "launch recorded" in r.stderr, (r.stdout, r.stderr)
        assert "joined the room from PLAYBOX_JOIN_LINK_FILE" in r.stderr, r.stderr
        assert '\\"member_id\\"' in r.stdout, r.stdout   # the sidecar's own status says it is joined
        homes = list((ghome / "launched").iterdir())
        assert len(homes) == 1, homes
        me = subprocess.run([pb, "id"], env={**env, "PLAYBOX_HOME": str(homes[0])}, capture_output=True, text=True).stdout.strip()
        recs = [json.loads(json.loads(l).get("item", {}).get("text", "x")[13:].rpartition(" ")[0])
                for l in open(state / "chat.jsonl") if "pbx launch" in l]
        assert len(recs) == 1, recs
        rec = recs[0]
        assert rec["key"] == me, "the record names the key the host made for the agent"
        assert rec["harness"] == "Conformance-Harness" and rec["model"] == "m1" and rec["image"].startswith("sha256:"), rec
        # An id that was never launched here, or is not a member, is refused by the host.
        sock = json.load(open(os.path.join(hd, recs and [f for f in os.listdir(hd) if f.endswith(".json")][0])))["sock"]
        import socket
        s = socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sock); s.sendall(("launch " + "ab" * 32 + " X\n").encode())
        assert s.recv(500).decode().startswith("error"), "the host refuses to vouch for a key that is not a member"
    finally:
        host.terminate()
        host.wait(timeout=10)
        shutil.rmtree(hd, ignore_errors=True)
