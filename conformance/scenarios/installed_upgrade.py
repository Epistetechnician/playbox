"""An installed older release upgrades in place and its rooms come with it: same home, same room, same members, one unbroken audit log (needs PLAYBOX_OLD_BIN)."""
import io
import json
import os
import re
import subprocess
import tarfile
import tempfile
from pathlib import Path

from harness import Skip, bin_dir, exe

TIER = "fast"
REPO = Path(__file__).resolve().parents[2]


def version_of(binary: str) -> str:
    out = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=30).stdout
    m = re.search(r"(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)", out)
    assert m, out
    return m.group(1)


def mirror(root: Path, name: str, binary: str, version: str, key: str | None) -> Path:
    """A release directory (what a download server would hold) with this binary as `playbox` for this platform."""
    rel = root / name
    rel.mkdir(parents=True)
    plat = {"Darwin": "darwin", "Linux": "linux"}[os.uname().sysname] + "-" + {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x86_64"}[os.uname().machine]
    with tarfile.open(rel / f"playbox-{plat}.tar.gz", "w:gz") as t:
        t.add(binary, arcname="playbox")
    cur = exe("playbox")
    r = subprocess.run([cur, "release", "manifest", version, str(rel), "--changelog", str(root / "none.md"), "--out", str(rel / "release.json")], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    import hashlib
    lines = [f"{hashlib.sha256((rel / n).read_bytes()).hexdigest()}  {n}\n" for n in sorted(os.listdir(rel))]
    (rel / "SHA256SUMS").write_text("".join(lines))
    if key:
        s = subprocess.run([cur, "release", "sign", str(rel / "SHA256SUMS"), "--key", key], capture_output=True, text=True)
        assert s.returncode == 0, s.stderr
    return rel


def run(ctx):
    old_dir = os.environ.get("PLAYBOX_OLD_BIN")
    if not old_dir:
        raise Skip("PLAYBOX_OLD_BIN is not set")
    old_playbox, old_mcp = exe("playbox", old=True), exe("playbox-mcp", old=True)
    new_playbox = exe("playbox")
    old_v, new_v = version_of(old_playbox), version_of(new_playbox)
    if old_v == new_v:
        raise Skip(f"the old and new builds are both {old_v}")

    root = ctx.dir("mirrors")
    keyfile = str(root / "key")
    pub = subprocess.run([new_playbox, "release", "keygen", keyfile], capture_output=True, text=True).stdout.strip()
    # Unsigned: install.sh pins the maintainer's key, which this mirror cannot be signed with, so it is installed on purpose unsigned.
    old_rel = mirror(root, "old", old_playbox, old_v, None)
    new_rel = mirror(root, "new", new_playbox, new_v, keyfile)

    # 1. Install the old release the way a person does: with install.sh, from a mirror.
    inst = ctx.dir("installed")
    env = {**os.environ, "PLAYBOX_INSTALL": str(inst), "PLAYBOX_BASE_URL": f"file://{old_rel}", "PLAYBOX_NO_SETUP": "1", "PLAYBOX_ALLOW_UNSIGNED": "1", "HOME": str(ctx.dir("home"))}
    r = subprocess.run(["sh", str(REPO / "install.sh")], capture_output=True, text=True, env=env, timeout=120)
    assert r.returncode == 0, r.stderr
    installed = str(inst / "playbox")
    assert old_v in subprocess.run([installed, "--version"], capture_output=True, text=True).stdout

    # 2. The old release hosts a room with some history.
    state = ctx.dir("state")
    old_host = __import__("harness").Mcp(state, {"PLAYBOX_HOME": str(state)}, old=True)
    ctx.procs.append(old_host)
    guest = ctx.mcp("guest")
    gid = guest.ok("playbox_status")["guest_id"]
    made = old_host.ok("playbox_create", transport="local", name="upgrade-room", uses=1, for_guest=gid)
    guest.ok("playbox_join", link=made["link"], workspace=str(ctx.dir("ws")))
    member = guest.ok("playbox_status")["joined"]["member_id"]
    for i in range(3):
        guest.ok("playbox_say", text=f"history {i}")
    before = old_host.ok("playbox_audit")["entries"]
    assert before >= 1, before
    old_host.close()

    # 3. Upgrade the installed binary: `playbox update` when this old release has it, else the installer again (both are real paths).
    upd = subprocess.run([installed, "update", "--help"], capture_output=True, text=True)
    if upd.returncode == 0:
        r = subprocess.run([installed, "update", "--pubkey", pub], capture_output=True, text=True, env={**env, "PLAYBOX_BASE_URL": f"file://{new_rel}"}, timeout=120)
        assert r.returncode == 0, r.stdout + r.stderr
        how = "update"
    else:
        (new_rel / "SHA256SUMS.sig").unlink()
        r = subprocess.run(["sh", str(REPO / "install.sh")], capture_output=True, text=True, env={**env, "PLAYBOX_BASE_URL": f"file://{new_rel}"}, timeout=120)
        assert r.returncode == 0, r.stderr
        how = "installer"
    assert new_v in subprocess.run([installed, "--version"], capture_output=True, text=True).stdout, how
    doc = subprocess.run([installed, "doctor", "--json"], capture_output=True, text=True, env={**env, "PLAYBOX_HOME": str(state)}, timeout=60)
    assert doc.returncode in (0, 1) and "checks" in json.loads(doc.stdout), doc.stderr

    # 4. The new release hosts the same home: the room, its members and its history are there, and the log is one chain.
    new_host = ctx.mcp("state", PLAYBOX_ALLOW_UNSAFE_RUNNER="1")
    again = new_host.ok("playbox_create", transport="local", name="upgrade-room", uses=1, for_guest=gid)
    guest.ok("playbox_leave")
    guest.ok("playbox_join", link=again["link"], workspace=str(ctx.dir("ws")))
    assert guest.ok("playbox_status")["joined"]["member_id"] == member, "the member is the same one across the upgrade"
    texts = [i["text"] for i in new_host.ok("playbox_feed", since=0)["items"] if i.get("kind") == "say"]
    if tuple(int(x) for x in old_v.split("-")[0].split(".")) >= (0, 2, 0):
        # Room talk has been kept on disk since 0.2.0; 0.1.0 held it in memory, so there is nothing for it to carry over.
        assert [f"history {i}" for i in range(3)] == [t for t in texts if t.startswith("history ")], texts
    guest.ok("playbox_say", text="after the upgrade")
    audit = new_host.ok("playbox_audit")
    assert audit["entries"] > before and len(audit["head"]) == 64, (before, audit)
