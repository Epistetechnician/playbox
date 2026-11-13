#!/usr/bin/env python3
"""install.sh --join end to end against a local mirror: real `playbox` binary, a hub bundle built from this checkout,
checksums, a real room hosted by a real sidecar. HOME and the install dir are throwaway; nothing touches your setup."""
import hashlib
import http.client
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import http.server
import functools

UI = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT = os.path.dirname(UI)
sys.path.insert(0, UI)
fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + (f" [{detail}]" if detail and not cond else ""))
    if not cond:
        fails.append(name)


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


def main():
    playbox = shutil.which("playbox")
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp")
    if not (playbox and mcp):
        print("SKIP install tests (playbox / playbox-mcp not found)")
        return
    from playbox_ui import Sidecar
    d = tempfile.mkdtemp(prefix="pbinst-")
    try:
        # a real room to be invited to
        repo = os.path.join(d, "repo"); os.makedirs(repo)
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        subprocess.run(["git", "-c", "user.email=a@b", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "i"], cwd=repo, check=True)
        host = Sidecar(mcp, os.path.join(d, "host"))
        e, r = host.call("playbox_create", {"repo": repo, "local": True, "uses": 1})
        link = r["link"]
        # the mirror
        mirror = os.path.join(d, "mirror"); os.makedirs(mirror)
        osn, arch = ("darwin" if platform.system() == "Darwin" else "linux"), ("arm64" if platform.machine() in ("arm64", "aarch64") else "x86_64")
        asset = f"playbox-{osn}-{arch}.tar.gz"
        bin_dir = os.path.join(d, "binsrc"); os.makedirs(bin_dir); shutil.copy(playbox, os.path.join(bin_dir, "playbox"))
        with tarfile.open(os.path.join(mirror, asset), "w:gz") as t:
            t.add(os.path.join(bin_dir, "playbox"), arcname="playbox")
        with tarfile.open(os.path.join(mirror, "playbox-hub.tar.gz"), "w:gz") as t:
            t.add(UI, arcname="ui", filter=lambda ti: None if "__pycache__" in ti.name or ti.name.endswith(".pyc") else ti)
        sums = lambda: open(os.path.join(mirror, "SHA256SUMS"), "w").write("".join(f"{hashlib.sha256(open(os.path.join(mirror, n), 'rb').read()).hexdigest()}  {n}\n" for n in (asset, "playbox-hub.tar.gz")))
        sums()
        mport = free_port()
        class Quiet(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *a):
                pass
        srv = http.server.ThreadingHTTPServer(("127.0.0.1", mport), functools.partial(Quiet, directory=mirror))
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        home = os.path.join(d, "home"); os.makedirs(home)
        env = {**os.environ, "HOME": home, "PLAYBOX_INSTALL": os.path.join(home, "bin"), "PLAYBOX_BASE_URL": f"http://127.0.0.1:{mport}",
               "PLAYBOX_NO_SETUP": "1", "BROWSER": "true", "PLAYBOX_HUB_HOME": os.path.join(home, "hubhome")}
        run = lambda *a, **k: subprocess.run(["sh", os.path.join(ROOT, "install.sh"), *a], capture_output=True, text=True, env=env, timeout=120, **k)

        bad = run("--join", "https://evil.example/x", "--no-start")
        check("a non-playbox link is refused", bad.returncode != 0 and "playbox://" in bad.stderr)
        bad = run("--frobnicate")
        check("unknown options are refused", bad.returncode != 0)
        # tampered bundle
        good = open(os.path.join(mirror, "playbox-hub.tar.gz"), "rb").read()
        open(os.path.join(mirror, "playbox-hub.tar.gz"), "ab").write(b"x")
        bad = run("--hub", "--no-start")
        check("a hub bundle that doesn't match its checksum is refused", bad.returncode != 0 and "checksum mismatch for playbox-hub" in bad.stderr and not os.path.exists(os.path.join(home, ".local/share/playbox-hub")))
        open(os.path.join(mirror, "playbox-hub.tar.gz"), "wb").write(good)
        os.rename(os.path.join(mirror, "playbox-hub.tar.gz"), os.path.join(mirror, "gone"))
        bad = run("--hub", "--no-start")
        check("a release without a hub bundle says so, and how to run it from a checkout", bad.returncode != 0 and "no hub bundle" in bad.stderr and "git clone" in bad.stderr)
        os.rename(os.path.join(mirror, "gone"), os.path.join(mirror, "playbox-hub.tar.gz"))

        ok = run("--join", link, "--name", "Maya's room", "--no-start")
        check("installs the binary, the hub and a launcher", ok.returncode == 0 and os.access(os.path.join(home, "bin", "playbox"), os.X_OK)
              and os.access(os.path.join(home, "bin", "playbox-hub"), os.X_OK) and os.path.exists(os.path.join(home, ".local/share/playbox-hub/ui/playbox_hub.py")), ok.stderr[-300:])
        check("the invite link is not echoed to the terminal", link[:60] not in ok.stdout + ok.stderr)

        # the exec path: install.sh itself launches the hub with the join link in the environment
        port = free_port()
        env2 = {**env, "PLAYBOX_HUB_PORT": str(port)}
        p = subprocess.Popen(["sh", os.path.join(ROOT, "install.sh"), "--join", link, "--name", "Maya's room"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env2)
        out, code = "", None
        for _ in range(60):
            line = p.stdout.readline()
            out += line
            m = re.search(r"/pair#(\S+)", line)
            if m:
                code = m.group(1)
                break
        check("the installer starts the hub, which joins the room", 'Joined "Maya\'s room"' in out and code is not None, out[-300:])
        check("the pairing link uses localhost, which passkeys accept", f"http://localhost:{port}/pair#" in out)
        check("the invite link is not in the hub's process arguments", link[:40] not in subprocess.run(["ps", "-axo", "command"], capture_output=True, text=True).stdout)
        conn = http.client.HTTPConnection("localhost", port, timeout=20)
        conn.request("POST", "/api/auth/weak", json.dumps({"code": code, "name": "t"}), {"Content-Type": "application/json", "Origin": f"http://localhost:{port}", "Host": f"localhost:{port}"})
        r = conn.getresponse(); r.read(); ck = r.getheader("Set-Cookie").split(";")[0]
        time.sleep(1.5)
        conn = http.client.HTTPConnection("localhost", port, timeout=20)
        conn.request("GET", "/api/rooms", headers={"Cookie": ck, "Host": f"localhost:{port}"})
        rooms = json.loads(conn.getresponse().read())["rooms"]
        check("the room is there, joined as a guest, named from the invite", len(rooms) == 1 and rooms[0]["label"] == "Maya's room" and rooms[0]["mode"] == "guest", json.dumps(rooms)[:200])
        p.terminate(); p.wait(timeout=15)
        host.close()
    finally:
        shutil.rmtree(d, ignore_errors=True)


if __name__ == "__main__":
    main()
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    sys.exit(1 if fails else 0)
