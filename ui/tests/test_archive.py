#!/usr/bin/env python3
"""The hub's durable copy of a room's feed (ui/hub/archive.py).

    python3 ui/tests/test_archive.py
"""
import json
import os
import shutil
import stat
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from hub.archive import Archive  # noqa: E402

fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


def item(seq, text="hi", kind="say", who="abc"):
    return {"seq": seq, "kind": kind, "from": who, "name": None, "text": text, "time_ms": 1000 + seq}


d = tempfile.mkdtemp(prefix="pbarc-")
try:
    a = Archive(d)
    check("an empty archive reads as empty", a.read() == ([], 0) and a.info()["items"] == 0 and a.tail(10) == [])
    check("appending stores new items", a.append([item(1), item(2), item(3)], members=[{"id": "m1", "name": "Ann", "last_seen_ms": 5}], me="m0") == 3)
    check("the same items again add nothing", a.append([item(2), item(3)]) == 0 and a.info()["items"] == 3)
    check("items arriving out of order are stored in order, once", a.append([item(6), item(5), item(4), item(5)]) == 3 and [i["seq"] for i in a.read()[0]] == [1, 2, 3, 4, 5, 6])
    items, nxt = a.read(since=4)
    check("read(since) returns what came after, and the cursor", [i["seq"] for i in items] == [5, 6] and nxt == 6)
    check("read(limit) pages", [i["seq"] for i in a.read(0, limit=2)[0]] == [1, 2] and a.read(0, limit=2)[1] == 2)
    check("tail returns the newest, oldest first", [i["seq"] for i in a.tail(3)] == [4, 5, 6])
    mem, me = a.members()
    check("the member list and who we are are kept", mem and mem[0]["name"] == "Ann" and me == "m0")

    # survives a restart (a new object over the same directory)
    b = Archive(d)
    check("a restarted hub sees everything and keeps its cursor", b.info()["items"] == 6 and b.info()["last_seq"] == 6 and b.append([item(6), item(7)]) == 1)

    # permissions: the room's words are private to this user
    mode = lambda p: stat.S_IMODE(os.stat(p).st_mode)
    check("the directory is 0700 and every file 0600", mode(os.path.join(d, "archive")) == 0o700 and all(mode(os.path.join(d, "archive", f)) == 0o600 for f in os.listdir(os.path.join(d, "archive"))))

    # a crash mid-write tears the last line: readers skip it, the next append carries on
    seg = os.path.join(d, "archive", sorted(f for f in os.listdir(os.path.join(d, "archive")) if f.startswith("feed-"))[-1])
    with open(seg, "ab") as f:
        f.write(b'{"seq": 8, "kind": "say", "text": "torn')
    c = Archive(d)
    check("a torn last line is ignored", [i["seq"] for i in c.read()[0]] == [1, 2, 3, 4, 5, 6, 7])
    check("and appending after it works", c.append([item(8, "whole")]) == 1 and c.tail(1)[0]["text"] == "whole")

    # a damaged meta.json is rebuilt from the segments
    open(os.path.join(d, "archive", "meta.json"), "w").write("{not json")
    e = Archive(d)
    check("a corrupt meta.json is rebuilt from the files", e.info()["last_seq"] == 8 and e.info()["items"] >= 7 and e.append([item(9)]) == 1)
    os.remove(os.path.join(d, "archive", "meta.json"))
    check("a missing meta.json is rebuilt too", Archive(d).info()["last_seq"] == 9)

    # size: segments roll, and the oldest go once the cap is passed
    d2 = tempfile.mkdtemp(prefix="pbarc2-")
    r = Archive(d2, segment_bytes=2000, max_bytes=6000)
    for i in range(1, 400):
        r.append([item(i, "x" * 80)])
    files = [f for f in os.listdir(os.path.join(d2, "archive")) if f.startswith("feed-")]
    total = sum(os.path.getsize(os.path.join(d2, "archive", f)) for f in files)
    check("segments roll over", len(files) > 1)
    check("the oldest segments are deleted past the cap", total <= 6000 + 2000 + 400 and r.info()["items"] < 399)
    check("the newest items are always kept", r.tail(1)[0]["seq"] == 399 and r.read(since=390)[0][0]["seq"] == 391)
    check("meta counts match the files", r.info()["items"] == sum(1 for _ in r.read(0, limit=10**6)[0]))
    shutil.rmtree(d2, ignore_errors=True)

    # a host that lost its state starts its sequence over: a new epoch, the old history kept for export
    f2 = Archive(d)
    old = f2.info()["items"]
    f2.host_restarted()
    check("after a host reset the current view starts empty", f2.read()[0] == [] and f2.info()["epoch"] == 1)
    f2.append([item(1, "new life"), item(2, "again")])
    check("new items go to the new epoch", [i["text"] for i in f2.read()[0]] == ["new life", "again"])
    exp = [json.loads(ln) for ln in f2.export().splitlines()]
    check("export keeps every epoch, nothing merged", len(exp) == old + 2 and {i["epoch"] for i in exp} == {0, 1})

    # what is stored: only known fields, bounded size
    f2.append([{"seq": 10, "kind": "say", "from": "z", "text": "t" * 20000, "evil": "<script>", "time_ms": 1}])
    big = f2.tail(1)[0]
    check("unknown fields are dropped and huge text is cut", "evil" not in big and len(big["text"]) <= 8000)
    check("text is stored as given (data, never interpreted)", Archive(d).append([item(11, '<img src=x onerror=alert(1)>')]) == 1 and Archive(d).tail(1)[0]["text"] == '<img src=x onerror=alert(1)>')

    f2.delete()
    check("delete removes the archive", not os.path.exists(os.path.join(d, "archive")) and f2.info()["items"] == 0)
finally:
    shutil.rmtree(d, ignore_errors=True)

print("\nall passed" if not fails else f"\n{len(fails)} failed: {fails}")
sys.exit(1 if fails else 0)
