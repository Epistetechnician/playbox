"""The command line judges a host the way the sidecar does: a tier it does not reach is refused, unspent."""
import os
import re
import subprocess
import time

from harness import exe

TIER = "fast"
# REQ: ROOM-8


def run(ctx):
    env = {**os.environ, "PLAYBOX_HOME": str(ctx.dir("host-home"))}
    up = subprocess.Popen(
        [exe("playbox"), "up", "--local", "--state", str(ctx.dir("state"))],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env, text=True,
    )
    try:
        link, end = None, time.time() + 20
        while time.time() < end and not link:
            m = re.search(r"playbox://\S+", up.stdout.readline())
            link = m.group(0) if m else None
        assert link, "the host printed no join link"
        guest = {"PLAYBOX_HOME": str(ctx.dir("guest-home"))}

        # A host that cannot prove T3 is refused, and the single-use invite is not spent.
        r = ctx.cli("join", link, "--local", "--yes", "--min-host-tier", "t3", env=guest, input="")
        assert r.returncode != 0, r.stdout
        assert "not joining" in r.stderr and "t3" in r.stderr.lower(), r.stderr

        # The same invite still works with the default requirement.
        r = ctx.cli("join", link, "--local", "--yes", env=guest, input="")
        assert r.returncode == 0 and "joined" in r.stdout, (r.stdout, r.stderr)
        assert "verified tier" in r.stderr, r.stderr
    finally:
        up.kill()
        up.wait(timeout=5)
