#!/usr/bin/env python3
"""A sidecar call can have a deadline, so a host that is gone cannot hold up the hub's poller (ui/playbox_ui.py)."""
import json
import os
import stat
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from playbox_ui import Sidecar  # noqa: E402

fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


# a stand-in for playbox-mcp: tool "slow" answers after 3 s, everything else at once
FAKE = r'''#!/usr/bin/env python3
import json, sys, time, threading
def reply(i, text):
    print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": text}]}}), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    if m["method"] == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {}}), flush=True)
    elif m["method"] == "tools/list":
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {"tools": [{"name": "slow"}, {"name": "fast"}]}}), flush=True)
    else:
        name = m["params"]["name"]
        if name == "slow":
            threading.Timer(3.0, reply, (m["id"], json.dumps({"late": True}))).start()
        else:
            reply(m["id"], json.dumps({"ok": name}))
'''
d = tempfile.mkdtemp(prefix="pbsc-")
fake = os.path.join(d, "fake-mcp")
open(fake, "w").write(FAKE)
os.chmod(fake, os.stat(fake).st_mode | stat.S_IXUSR)
sc = Sidecar(fake, d)
try:
    t0 = time.time()
    err, data = sc.call("slow", {}, timeout=1)
    check("a call with a deadline gives up on time", err and str(data).startswith("timeout") and 0.8 < time.time() - t0 < 2.0, f"{err} {data} {time.time() - t0:.1f}s")
    t0 = time.time()
    err, data = sc.call("fast", {}, timeout=5)
    check("the next call works at once, not behind the stuck one", not err and data == {"ok": "fast"} and time.time() - t0 < 1.5, f"{err} {data}")
    time.sleep(3)
    err, data = sc.call("fast", {}, timeout=5)
    check("the late answer to the abandoned call is skipped, not mistaken for a new one", not err and data == {"ok": "fast"}, f"{err} {data}")
    err, data = sc.call("slow", {})
    check("without a deadline a call still waits for its answer", not err and data == {"late": True})
finally:
    sc.close()
print("\nall passed" if not fails else f"\n{len(fails)} failed: {fails}")
sys.exit(1 if fails else 0)
